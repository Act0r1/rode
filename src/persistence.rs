use crate::conversation::{CardKind, CardStatus, ConversationCard, NoticeTone};
use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static PROJECT_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const SCHEMA_VERSION: i64 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProject {
    pub id: String,
    pub path: PathBuf,
    pub git_root: PathBuf,
    pub name: String,
    pub active_thread_id: Option<String>,
    pub last_opened_ms: i64,
    pub settings_override: Option<String>,
}

impl StoredProject {
    pub fn new(path: PathBuf, name: String) -> Self {
        Self {
            id: new_project_id(&path),
            git_root: path.clone(),
            path,
            name,
            active_thread_id: None,
            last_opened_ms: 0,
            settings_override: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredThread {
    pub id: String,
    pub project_path: PathBuf,
    pub project_name: String,
    pub title: String,
    pub workspace_path: PathBuf,
    pub branch: Option<String>,
    pub provider_thread_id: Option<String>,
    pub ordinal: usize,
    pub draft: String,
    pub activity: String,
    pub activity_updated_ms: i64,
    pub base_branch: Option<String>,
    pub last_error: Option<String>,
    pub dirty_count: usize,
    pub unread: bool,
    pub conversation_scroll_item: usize,
    pub conversation_scroll_offset_millis: i64,
    pub messages: Vec<StoredMessage>,
    pub events: Vec<ConversationCard>,
}

pub struct StateStore {
    connection: Connection,
}

impl StateStore {
    pub fn open_default() -> Result<Self> {
        let state_dir = rode_state_dir()?;
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("failed to create {}", state_dir.display()))?;
        Self::open(&state_dir.join("state.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)
            .with_context(|| format!("failed to open Rode state database {}", path.display()))?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS projects (
                    path TEXT PRIMARY KEY NOT NULL,
                    id TEXT,
                    git_root TEXT,
                    name TEXT NOT NULL,
                    active_thread_id TEXT,
                    last_opened_ms INTEGER NOT NULL,
                    settings_json TEXT
                 );
                 CREATE TABLE IF NOT EXISTS threads (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_path TEXT NOT NULL,
                    title TEXT NOT NULL,
                    workspace_path TEXT NOT NULL,
                    branch TEXT,
                    provider_thread_id TEXT,
                    ordinal INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    draft TEXT NOT NULL DEFAULT '',
                    activity TEXT NOT NULL DEFAULT 'waiting',
                    activity_updated_ms INTEGER NOT NULL DEFAULT 0,
                    base_branch TEXT,
                    last_error TEXT,
                    dirty_count INTEGER NOT NULL DEFAULT 0,
                    unread INTEGER NOT NULL DEFAULT 0,
                    conversation_scroll_item INTEGER NOT NULL DEFAULT 0,
                    conversation_scroll_offset_millis INTEGER NOT NULL DEFAULT 0,
                    FOREIGN KEY(project_path) REFERENCES projects(path) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS threads_project_ordinal
                    ON threads(project_path, ordinal);
                 CREATE TABLE IF NOT EXISTS messages (
                    thread_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    text TEXT NOT NULL,
                    PRIMARY KEY(thread_id, sequence),
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                 );",
            )
            .context("failed to initialize Rode state schema")?;
        ensure_schema_columns(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn save_thread(&mut self, thread: &StoredThread) -> Result<()> {
        let project_path = path_text(&thread.project_path);
        let workspace_path = path_text(&thread.workspace_path);
        let now = now_ms();
        let activity_updated_ms = if thread.activity_updated_ms > 0 {
            thread.activity_updated_ms
        } else {
            now
        };
        let transaction = self
            .connection
            .transaction()
            .context("failed to begin Rode state transaction")?;
        let project_id = existing_project_id(&transaction, &thread.project_path)?
            .unwrap_or_else(|| new_project_id(&thread.project_path));
        transaction.execute(
            "INSERT INTO projects(path, id, git_root, name, active_thread_id, last_opened_ms)
             VALUES (?1, ?2, ?1, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                git_root = excluded.git_root,
                name = excluded.name,
                active_thread_id = excluded.active_thread_id",
            params![
                project_path,
                project_id,
                thread.project_name,
                thread.id,
                now
            ],
        )?;
        transaction.execute(
            "INSERT INTO threads(
                id, project_path, title, workspace_path, branch,
                provider_thread_id, ordinal, updated_at_ms, draft,
                activity, activity_updated_ms, base_branch,
                last_error, dirty_count, unread, conversation_scroll_item,
                conversation_scroll_offset_millis
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
                project_path = excluded.project_path,
                title = excluded.title,
                workspace_path = excluded.workspace_path,
                branch = excluded.branch,
                provider_thread_id = excluded.provider_thread_id,
                ordinal = excluded.ordinal,
                updated_at_ms = excluded.updated_at_ms,
                draft = excluded.draft,
                activity = excluded.activity,
                activity_updated_ms = excluded.activity_updated_ms,
                base_branch = excluded.base_branch,
                last_error = excluded.last_error,
                dirty_count = excluded.dirty_count,
                unread = excluded.unread,
                conversation_scroll_item = excluded.conversation_scroll_item,
                conversation_scroll_offset_millis = excluded.conversation_scroll_offset_millis",
            params![
                thread.id,
                project_path,
                thread.title,
                workspace_path,
                thread.branch,
                thread.provider_thread_id,
                thread.ordinal as i64,
                now,
                thread.draft,
                thread.activity,
                activity_updated_ms,
                thread.base_branch,
                thread.last_error,
                usize_to_i64(thread.dirty_count),
                thread.unread,
                usize_to_i64(thread.conversation_scroll_item),
                thread.conversation_scroll_offset_millis,
            ],
        )?;
        transaction.execute(
            "DELETE FROM messages WHERE thread_id = ?1",
            params![thread.id],
        )?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO messages(thread_id, sequence, role, text)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (sequence, message) in thread.messages.iter().enumerate() {
                statement.execute(params![
                    thread.id,
                    sequence as i64,
                    message.role,
                    message.text
                ])?;
            }
        }
        replace_conversation_events(&transaction, &thread.id, &thread.events)?;
        transaction.commit().context("failed to save Rode state")
    }

    /// Replaces the complete ordered conversation projection for a thread.
    ///
    /// This is useful at turn boundaries, where the in-memory projection is the
    /// authority and should be committed atomically.
    pub fn save_thread_events(
        &mut self,
        thread_id: &str,
        events: &[ConversationCard],
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction()
            .context("failed to begin conversation event transaction")?;
        replace_conversation_events(&transaction, thread_id, events)?;
        transaction
            .execute(
                "UPDATE threads SET updated_at_ms = ?2 WHERE id = ?1",
                params![thread_id, now_ms()],
            )
            .context("failed to update conversation thread timestamp")?;
        transaction
            .commit()
            .context("failed to save conversation events")
    }

    /// Upserts one streamed card while retaining its stable event identity.
    /// The supplied sequence is its position in the thread projection.
    pub fn upsert_thread_event(
        &mut self,
        thread_id: &str,
        sequence: usize,
        event: &ConversationCard,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction()
            .context("failed to begin conversation event update")?;
        upsert_conversation_event(&transaction, thread_id, sequence, event)?;
        transaction
            .execute(
                "UPDATE threads SET updated_at_ms = ?2 WHERE id = ?1",
                params![thread_id, now_ms()],
            )
            .context("failed to update conversation thread timestamp")?;
        transaction
            .commit()
            .context("failed to update conversation event")
    }

    pub fn save_thread_draft(&mut self, id: &str, draft: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE threads SET draft = ?2 WHERE id = ?1",
            params![id, draft],
        )?;
        Ok(())
    }

    pub fn update_thread_activity(
        &mut self,
        id: &str,
        activity: &str,
        last_error: Option<&str>,
        dirty_count: usize,
        unread: bool,
    ) -> Result<()> {
        let now = now_ms();
        self.connection.execute(
            "UPDATE threads
             SET activity = ?2,
                 activity_updated_ms = ?3,
                 last_error = ?4,
                 dirty_count = ?5,
                 unread = ?6,
                 updated_at_ms = ?3
             WHERE id = ?1",
            params![
                id,
                activity,
                now,
                last_error,
                usize_to_i64(dirty_count),
                unread,
            ],
        )?;
        Ok(())
    }

    pub fn mark_thread_read(&mut self, id: &str) -> Result<()> {
        self.connection
            .execute("UPDATE threads SET unread = 0 WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn save_project(&mut self, project: &StoredProject) -> Result<()> {
        self.connection.execute(
            "INSERT INTO projects(
                path, id, git_root, name, active_thread_id, last_opened_ms, settings_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(path) DO UPDATE SET
                git_root = excluded.git_root,
                name = excluded.name,
                active_thread_id = COALESCE(excluded.active_thread_id, projects.active_thread_id),
                last_opened_ms = excluded.last_opened_ms,
                settings_json = excluded.settings_json",
            params![
                path_text(&project.path),
                project.id,
                path_text(&project.git_root),
                project.name,
                project.active_thread_id,
                now_ms(),
                project.settings_override,
            ],
        )?;
        Ok(())
    }

    pub fn remove_project(&mut self, path: &Path) -> Result<()> {
        let removed_id = existing_project_id(&self.connection, path)?;
        self.connection.execute(
            "DELETE FROM projects WHERE path = ?1",
            params![path_text(path)],
        )?;
        if let Some(removed_id) = removed_id
            && self.load_string_setting("active_project_id")?.as_deref() == Some(&removed_id)
        {
            self.connection
                .execute("DELETE FROM settings WHERE key = 'active_project_id'", [])?;
        }
        Ok(())
    }

    pub fn repair_project_path(
        &mut self,
        old_path: &Path,
        new_path: &Path,
        new_name: &str,
    ) -> Result<()> {
        let old_path = path_text(old_path);
        let new_path = path_text(new_path);
        if old_path == new_path {
            self.connection.execute(
                "UPDATE projects SET name = ?2, last_opened_ms = ?3 WHERE path = ?1",
                params![old_path, new_name, now_ms()],
            )?;
            return Ok(());
        }

        let transaction = self.connection.transaction()?;
        transaction.execute_batch("PRAGMA defer_foreign_keys = ON;")?;
        if existing_project_id(&transaction, Path::new(&new_path))?.is_some() {
            anyhow::bail!("the replacement folder is already a saved Rode project");
        }
        transaction.execute(
            "UPDATE threads SET project_path = ?2 WHERE project_path = ?1",
            params![old_path, new_path],
        )?;
        let changed = transaction.execute(
            "UPDATE projects
             SET path = ?2, git_root = ?2, name = ?3, last_opened_ms = ?4
             WHERE path = ?1",
            params![old_path, new_path, new_name, now_ms()],
        )?;
        if changed == 0 {
            anyhow::bail!("the project being repaired is no longer saved");
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn rename_project(&mut self, path: &Path, name: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE projects SET name = ?2 WHERE path = ?1",
            params![path_text(path), name],
        )?;
        Ok(())
    }

    pub fn rename_thread(&mut self, id: &str, title: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE threads SET title = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![id, title, now_ms()],
        )?;
        Ok(())
    }

    pub fn load_projects(&self) -> Result<Vec<StoredProject>> {
        let mut statement = self.connection.prepare(
            "SELECT id, path, git_root, name, active_thread_id, last_opened_ms, settings_json
             FROM projects
             ORDER BY last_opened_ms DESC, name COLLATE NOCASE ASC",
        )?;
        let projects = statement
            .query_map([], |row| {
                Ok(StoredProject {
                    id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                    git_root: PathBuf::from(row.get::<_, String>(2)?),
                    name: row.get(3)?,
                    active_thread_id: row.get(4)?,
                    last_opened_ms: row.get(5)?,
                    settings_override: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to load Rode projects")?;
        Ok(projects)
    }

    pub fn load_active_project_id(&self) -> Result<Option<String>> {
        self.load_string_setting("active_project_id")
    }

    pub fn save_active_project_id(&mut self, id: &str) -> Result<()> {
        self.save_string_setting("active_project_id", id)
    }

    pub fn load_bool_setting(&self, key: &str, default: bool) -> Result<bool> {
        let value = self.load_string_setting(key)?;
        Ok(value.map_or(default, |value| value == "true"))
    }

    pub fn save_bool_setting(&mut self, key: &str, value: bool) -> Result<()> {
        self.save_string_setting(key, if value { "true" } else { "false" })
    }

    pub fn load_string_setting(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .context("failed to load Rode setting")
    }

    pub fn save_string_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn load_f32_setting(&self, key: &str, default: f32) -> Result<f32> {
        Ok(self
            .load_string_setting(key)?
            .and_then(|value| value.parse::<f32>().ok())
            .unwrap_or(default))
    }

    pub fn save_f32_setting(&mut self, key: &str, value: f32) -> Result<()> {
        self.save_string_setting(key, &value.to_string())
    }

    pub fn load_active_thread(&self, project_path: &Path) -> Result<Option<StoredThread>> {
        let project_path_text = path_text(project_path);
        let active_thread_id = self
            .connection
            .query_row(
                "SELECT active_thread_id FROM projects WHERE path = ?1",
                params![project_path_text],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        let Some(active_thread_id) = active_thread_id else {
            return Ok(None);
        };
        self.load_thread(&active_thread_id)
    }

    pub fn load_threads(&self, project_path: &Path) -> Result<Vec<StoredThread>> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM threads WHERE project_path = ?1
             ORDER BY ordinal ASC, updated_at_ms ASC",
        )?;
        let ids = statement
            .query_map(params![path_text(project_path)], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| {
                self.load_thread(&id)?
                    .with_context(|| format!("thread {id} disappeared while loading project state"))
            })
            .collect()
    }

    fn load_thread(&self, id: &str) -> Result<Option<StoredThread>> {
        let row = self
            .connection
            .query_row(
                "SELECT id, project_path, title, workspace_path, branch,
                        provider_thread_id, ordinal, draft, activity,
                        activity_updated_ms, base_branch, last_error,
                        dirty_count, unread, conversation_scroll_item,
                        conversation_scroll_offset_millis
                 FROM threads WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, i64>(12)?,
                        row.get::<_, bool>(13)?,
                        row.get::<_, i64>(14)?,
                        row.get::<_, i64>(15)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            id,
            project_path,
            title,
            workspace_path,
            branch,
            provider_thread_id,
            ordinal,
            draft,
            activity,
            activity_updated_ms,
            base_branch,
            last_error,
            dirty_count,
            unread,
            conversation_scroll_item,
            conversation_scroll_offset_millis,
        )) = row
        else {
            return Ok(None);
        };
        let mut statement = self.connection.prepare(
            "SELECT role, text FROM messages WHERE thread_id = ?1 ORDER BY sequence ASC",
        )?;
        let messages = statement
            .query_map(params![id], |row| {
                Ok(StoredMessage {
                    role: row.get(0)?,
                    text: row.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let events = load_conversation_events(&self.connection, &id, &messages)?;
        let project_name = self
            .connection
            .query_row(
                "SELECT name FROM projects WHERE path = ?1",
                params![project_path],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_else(|_| folder_name(Path::new(&project_path)));
        Ok(Some(StoredThread {
            id,
            project_path: PathBuf::from(project_path),
            project_name,
            title,
            workspace_path: PathBuf::from(workspace_path),
            branch,
            provider_thread_id,
            ordinal: ordinal.max(0) as usize,
            draft,
            activity,
            activity_updated_ms,
            base_branch,
            last_error,
            dirty_count: dirty_count.max(0) as usize,
            unread,
            conversation_scroll_item: conversation_scroll_item.max(0) as usize,
            conversation_scroll_offset_millis,
            messages,
            events,
        }))
    }
}

fn ensure_schema_columns(connection: &mut Connection) -> Result<()> {
    let current_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .context("failed to read Rode schema version")?;
    anyhow::ensure!(
        current_version <= SCHEMA_VERSION,
        "Rode state schema version {current_version} is newer than supported version {SCHEMA_VERSION}"
    );
    if current_version == SCHEMA_VERSION {
        return Ok(());
    }

    let transaction = connection
        .transaction()
        .context("failed to begin Rode schema migration")?;
    let mut statement = transaction.prepare("PRAGMA table_info(projects)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    if !columns.iter().any(|column| column == "id") {
        transaction.execute("ALTER TABLE projects ADD COLUMN id TEXT", [])?;
    }
    if !columns.iter().any(|column| column == "git_root") {
        transaction.execute("ALTER TABLE projects ADD COLUMN git_root TEXT", [])?;
    }
    if !columns.iter().any(|column| column == "settings_json") {
        transaction.execute("ALTER TABLE projects ADD COLUMN settings_json TEXT", [])?;
    }

    let projects = {
        let mut statement = transaction.prepare(
            "SELECT rowid, path FROM projects WHERE id IS NULL OR id = '' OR git_root IS NULL",
        )?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (rowid, path) in projects {
        transaction.execute(
            "UPDATE projects
             SET id = COALESCE(NULLIF(id, ''), ?2), git_root = COALESCE(git_root, path)
             WHERE rowid = ?1",
            params![rowid, legacy_project_id(&path)],
        )?;
    }
    transaction.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS projects_stable_id ON projects(id)",
        [],
    )?;

    let mut statement = transaction.prepare("PRAGMA table_info(threads)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    for (column, definition) in [
        ("draft", "TEXT NOT NULL DEFAULT ''"),
        ("activity", "TEXT NOT NULL DEFAULT 'waiting'"),
        ("activity_updated_ms", "INTEGER NOT NULL DEFAULT 0"),
        ("base_branch", "TEXT"),
        ("last_error", "TEXT"),
        ("dirty_count", "INTEGER NOT NULL DEFAULT 0"),
        ("unread", "INTEGER NOT NULL DEFAULT 0"),
        ("conversation_scroll_item", "INTEGER NOT NULL DEFAULT 0"),
        (
            "conversation_scroll_offset_millis",
            "INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        if !columns.iter().any(|existing| existing == column) {
            transaction.execute(
                &format!("ALTER TABLE threads ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    transaction.execute(
        "UPDATE threads
         SET activity_updated_ms = updated_at_ms
         WHERE activity_updated_ms <= 0",
        [],
    )?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversation_events (
            thread_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            payload_json TEXT NOT NULL,
            updated_at_ms INTEGER NOT NULL,
            PRIMARY KEY(thread_id, event_id),
            FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
         );
         CREATE INDEX IF NOT EXISTS conversation_events_thread_sequence
            ON conversation_events(thread_id, sequence);",
    )?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .context("failed to update Rode schema version")?;
    transaction
        .commit()
        .context("failed to commit Rode schema migration")
}

fn replace_conversation_events(
    transaction: &Transaction<'_>,
    thread_id: &str,
    events: &[ConversationCard],
) -> Result<()> {
    transaction
        .execute(
            "DELETE FROM conversation_events WHERE thread_id = ?1",
            params![thread_id],
        )
        .context("failed to replace conversation events")?;
    let mut statement = transaction.prepare(
        "INSERT INTO conversation_events(
            thread_id, event_id, sequence, payload_json, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (sequence, event) in events.iter().enumerate() {
        let payload = serde_json::to_string(event)
            .with_context(|| format!("failed to serialize conversation event {}", event.id))?;
        statement
            .execute(params![
                thread_id,
                event.id,
                usize_to_i64(sequence),
                payload,
                now_ms(),
            ])
            .with_context(|| format!("failed to save conversation event {}", event.id))?;
    }
    Ok(())
}

fn upsert_conversation_event(
    transaction: &Transaction<'_>,
    thread_id: &str,
    sequence: usize,
    event: &ConversationCard,
) -> Result<()> {
    let payload = serde_json::to_string(event)
        .with_context(|| format!("failed to serialize conversation event {}", event.id))?;
    transaction
        .execute(
            "INSERT INTO conversation_events(
                thread_id, event_id, sequence, payload_json, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(thread_id, event_id) DO UPDATE SET
                sequence = excluded.sequence,
                payload_json = excluded.payload_json,
                updated_at_ms = excluded.updated_at_ms",
            params![
                thread_id,
                event.id,
                usize_to_i64(sequence),
                payload,
                now_ms(),
            ],
        )
        .with_context(|| format!("failed to upsert conversation event {}", event.id))?;
    Ok(())
}

fn load_conversation_events(
    connection: &Connection,
    thread_id: &str,
    legacy_messages: &[StoredMessage],
) -> Result<Vec<ConversationCard>> {
    let mut statement = connection.prepare(
        "SELECT event_id, payload_json
         FROM conversation_events
         WHERE thread_id = ?1
         ORDER BY sequence ASC, rowid ASC",
    )?;
    let rows = statement
        .query_map(params![thread_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if rows.is_empty() {
        return Ok(legacy_message_events(legacy_messages));
    }

    let mut events = Vec::with_capacity(rows.len());
    for (event_id, payload) in rows {
        let event = serde_json::from_str::<ConversationCard>(&payload)
            .with_context(|| format!("failed to deserialize conversation event {event_id}"))?;
        anyhow::ensure!(
            event.id == event_id,
            "conversation event {event_id} payload does not match its stable event id"
        );
        events.push(event);
    }
    Ok(events)
}

fn legacy_message_events(messages: &[StoredMessage]) -> Vec<ConversationCard> {
    messages
        .iter()
        .enumerate()
        .map(|(sequence, message)| ConversationCard {
            id: format!("legacy-message-{sequence}"),
            turn_id: None,
            created_at_ms: 0,
            status: CardStatus::Complete,
            collapsed: false,
            kind: match message.role.as_str() {
                "user" => CardKind::UserMessage {
                    text: message.text.clone(),
                    model: "legacy".to_owned(),
                    access: "unknown".to_owned(),
                    attachments: Vec::new(),
                },
                "agent" | "assistant" => CardKind::AssistantMessage {
                    text: message.text.clone(),
                },
                _ => CardKind::Notice {
                    tone: NoticeTone::Info,
                    text: message.text.clone(),
                },
            },
        })
        .collect()
}

fn existing_project_id(connection: &Connection, path: &Path) -> Result<Option<String>> {
    connection
        .query_row(
            "SELECT id FROM projects WHERE path = ?1",
            params![path_text(path)],
            |row| row.get(0),
        )
        .optional()
        .context("failed to read Rode project identity")
}

fn new_project_id(path: &Path) -> String {
    let sequence = PROJECT_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        "project-{:x}-{:x}-{:x}",
        now_ms(),
        std::process::id(),
        sequence ^ stable_path_hash(path)
    )
}

fn legacy_project_id(path: &str) -> String {
    format!("legacy-project-{:016x}", stable_path_hash(path))
}

fn stable_path_hash(path: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn rode_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("rode"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/state/rode"))
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("project")
        .to_owned()
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn usize_to_i64(value: usize) -> i64 {
    value.min(i64::MAX as usize) as i64
}

#[cfg(test)]
mod tests {
    use super::{StateStore, StoredMessage, StoredProject, StoredThread};
    use crate::conversation::{CardKind, CardStatus, ConversationCard};
    use rusqlite::Connection;
    use std::fs;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn round_trips_active_thread_and_messages() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-state-test-{nonce}"));
        fs::create_dir_all(&root).expect("create state fixture");
        let database = root.join("state.sqlite3");
        let project = root.join("project");
        let worktree = root.join("worktree");
        fs::create_dir_all(&project).expect("create project fixture");
        fs::create_dir_all(&worktree).expect("create worktree fixture");
        let mut store = StateStore::open(&database).expect("open state database");
        let thread = StoredThread {
            id: "thread-1".to_owned(),
            project_path: project.clone(),
            project_name: "Rode project".to_owned(),
            title: "Native rendering".to_owned(),
            workspace_path: worktree.clone(),
            branch: Some("rode/thread-1-native-rendering".to_owned()),
            provider_thread_id: Some("provider-thread-1".to_owned()),
            ordinal: 1,
            draft: "Keep this draft".to_owned(),
            activity: "running".to_owned(),
            activity_updated_ms: 1234,
            base_branch: Some("main".to_owned()),
            last_error: None,
            dirty_count: 2,
            unread: true,
            conversation_scroll_item: 4,
            conversation_scroll_offset_millis: 12_500,
            messages: vec![
                StoredMessage {
                    role: "user".to_owned(),
                    text: "Build it".to_owned(),
                },
                StoredMessage {
                    role: "agent".to_owned(),
                    text: "Done".to_owned(),
                },
            ],
            events: vec![
                ConversationCard::stable(
                    "user-turn-1",
                    CardKind::UserMessage {
                        text: "Build it".to_owned(),
                        model: "gpt-5.4".to_owned(),
                        access: "workspace-write".to_owned(),
                        attachments: vec!["design.png".to_owned()],
                    },
                    CardStatus::Complete,
                    Some("turn-1".to_owned()),
                ),
                ConversationCard::stable(
                    "assistant-turn-1",
                    CardKind::AssistantMessage {
                        text: "Done".to_owned(),
                    },
                    CardStatus::Complete,
                    Some("turn-1".to_owned()),
                ),
            ],
        };
        store.save_thread(&thread).expect("save thread");

        let loaded = store
            .load_active_thread(&project)
            .expect("load active thread")
            .expect("active thread exists");
        assert_eq!(loaded, thread);
        assert_eq!(store.load_threads(&project).unwrap(), vec![thread]);

        let mut streamed = loaded.events[1].clone();
        streamed.status = CardStatus::Running;
        streamed.kind = CardKind::AssistantMessage {
            text: "Done, with a streamed update".to_owned(),
        };
        store
            .upsert_thread_event("thread-1", 1, &streamed)
            .expect("upsert streamed event");
        let streamed_thread = store
            .load_active_thread(&project)
            .expect("load streamed thread")
            .expect("streamed thread exists");
        assert_eq!(streamed_thread.events.len(), 2);
        assert_eq!(streamed_thread.events[1], streamed);
        assert_eq!(streamed_thread.events[1].id, "assistant-turn-1");

        store
            .save_thread_draft("thread-1", "Updated draft")
            .expect("update thread draft");
        store
            .update_thread_activity("thread-1", "error", Some("worktree failed"), 4, true)
            .expect("update thread activity");
        let updated = store
            .load_active_thread(&project)
            .expect("load updated active thread")
            .expect("updated active thread exists");
        assert_eq!(updated.draft, "Updated draft");
        assert_eq!(updated.activity, "error");
        assert!(updated.activity_updated_ms > 1234);
        assert_eq!(updated.last_error.as_deref(), Some("worktree failed"));
        assert_eq!(updated.dirty_count, 4);
        assert!(updated.unread);
        store
            .mark_thread_read("thread-1")
            .expect("mark thread read");
        assert!(!store.load_active_thread(&project).unwrap().unwrap().unread);
        let projects = store.load_projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, project);
        assert_eq!(projects[0].git_root, project);
        assert_eq!(projects[0].name, "Rode project");
        assert_eq!(projects[0].active_thread_id.as_deref(), Some("thread-1"));
        assert!(projects[0].last_opened_ms > 0);
        assert!(projects[0].settings_override.is_none());
        let stable_id = projects[0].id.clone();
        store.save_active_project_id(&stable_id).unwrap();

        assert!(!store.load_bool_setting("isolate", false).unwrap());
        store.save_bool_setting("isolate", true).unwrap();
        assert!(store.load_bool_setting("isolate", false).unwrap());
        store.save_string_setting("theme", "daylight").unwrap();
        assert_eq!(
            store.load_string_setting("theme").unwrap().as_deref(),
            Some("daylight")
        );
        store.save_f32_setting("sidebar_width", 284.5).unwrap();
        assert_eq!(
            store.load_f32_setting("sidebar_width", 252.0).unwrap(),
            284.5
        );

        let repaired_project = root.join("repaired-project");
        fs::create_dir_all(&repaired_project).expect("create repaired project fixture");
        store
            .repair_project_path(&project, &repaired_project, "Repaired project")
            .expect("repair project path");
        let repaired = store.load_projects().expect("load repaired project");
        assert_eq!(repaired.len(), 1);
        assert_eq!(repaired[0].path, repaired_project);
        assert_eq!(repaired[0].name, "Repaired project");
        assert_eq!(repaired[0].id, stable_id);
        assert_eq!(store.load_active_project_id().unwrap(), Some(stable_id));
        assert_eq!(
            store
                .load_active_thread(&repaired[0].path)
                .expect("load repaired active thread")
                .expect("repaired active thread")
                .project_path,
            repaired[0].path
        );
        store
            .remove_project(&repaired[0].path)
            .expect("remove repaired project");
        assert!(store.load_projects().unwrap().is_empty());

        drop(store);
        fs::remove_dir_all(root).expect("clean state fixture");
    }

    #[test]
    fn saving_a_thread_does_not_reorder_projects() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-project-order-test-{nonce}"));
        fs::create_dir_all(&root).expect("create state fixture");
        let database = root.join("state.sqlite3");
        let older_project = root.join("older");
        let newer_project = root.join("newer");
        fs::create_dir_all(&older_project).expect("create older project fixture");
        fs::create_dir_all(&newer_project).expect("create newer project fixture");
        let mut store = StateStore::open(&database).expect("open state database");
        store
            .save_project(&StoredProject::new(
                older_project.clone(),
                "Older".to_owned(),
            ))
            .expect("save older project");
        store
            .save_project(&StoredProject::new(
                newer_project.clone(),
                "Newer".to_owned(),
            ))
            .expect("save newer project");
        store
            .connection
            .execute(
                "UPDATE projects SET last_opened_ms = CASE path WHEN ?1 THEN 100 WHEN ?2 THEN 200 END",
                rusqlite::params![
                    older_project.to_string_lossy(),
                    newer_project.to_string_lossy()
                ],
            )
            .expect("set deterministic project order");

        store
            .save_thread(&StoredThread {
                id: "older-thread".to_owned(),
                project_path: older_project.clone(),
                project_name: "Older".to_owned(),
                title: "Thread 1".to_owned(),
                workspace_path: older_project.clone(),
                branch: None,
                provider_thread_id: None,
                ordinal: 1,
                draft: String::new(),
                activity: "ready".to_owned(),
                activity_updated_ms: 100,
                base_branch: None,
                last_error: None,
                dirty_count: 0,
                unread: false,
                conversation_scroll_item: 0,
                conversation_scroll_offset_millis: 0,
                messages: Vec::new(),
                events: Vec::new(),
            })
            .expect("save thread in older project");

        let projects = store.load_projects().expect("load ordered projects");
        assert_eq!(projects[0].path, newer_project);
        assert_eq!(projects[0].last_opened_ms, 200);
        assert_eq!(projects[1].path, older_project);
        assert_eq!(projects[1].last_opened_ms, 100);

        store
            .save_project(&projects[1])
            .expect("open older project");
        let reordered = store.load_projects().expect("load reordered projects");
        assert_eq!(reordered[0].path, older_project);
        assert!(reordered[0].last_opened_ms > 200);

        drop(store);
        fs::remove_dir_all(root).expect("clean state fixture");
    }

    #[test]
    fn upgrades_legacy_projects_with_stable_identity_and_git_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-state-migration-{nonce}"));
        fs::create_dir_all(&root).expect("create migration fixture");
        let database = root.join("state.sqlite3");
        let connection = Connection::open(&database).expect("open legacy database");
        connection
            .execute_batch(
                "CREATE TABLE projects (
                    path TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    active_thread_id TEXT,
                    last_opened_ms INTEGER NOT NULL
                 );
                 INSERT INTO projects(path, name, active_thread_id, last_opened_ms)
                 VALUES ('/tmp/legacy-rode-project', 'Legacy', NULL, 7);",
            )
            .expect("write legacy schema");
        drop(connection);

        let store = StateStore::open(&database).expect("upgrade state database");
        let projects = store.load_projects().expect("load upgraded project");
        assert_eq!(projects.len(), 1);
        assert!(projects[0].id.starts_with("legacy-project-"));
        assert_eq!(projects[0].git_root, projects[0].path);
        assert!(projects[0].settings_override.is_none());

        drop(store);
        fs::remove_dir_all(root).expect("clean migration fixture");
    }

    #[test]
    fn transactionally_upgrades_legacy_thread_activity_columns() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-thread-migration-{nonce}"));
        fs::create_dir_all(&root).expect("create migration fixture");
        let database = root.join("state.sqlite3");
        let connection = Connection::open(&database).expect("open legacy database");
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE projects (
                    path TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    active_thread_id TEXT,
                    last_opened_ms INTEGER NOT NULL
                 );
                 CREATE TABLE threads (
                    id TEXT PRIMARY KEY NOT NULL,
                    project_path TEXT NOT NULL,
                    title TEXT NOT NULL,
                    workspace_path TEXT NOT NULL,
                    branch TEXT,
                    provider_thread_id TEXT,
                    ordinal INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    FOREIGN KEY(project_path) REFERENCES projects(path) ON DELETE CASCADE
                 );
                 CREATE TABLE messages (
                    thread_id TEXT NOT NULL,
                    sequence INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    text TEXT NOT NULL,
                    PRIMARY KEY(thread_id, sequence),
                    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
                 );
                 CREATE TABLE settings (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                 );
                 INSERT INTO projects(path, name, active_thread_id, last_opened_ms)
                 VALUES ('/tmp/legacy-thread-project', 'Legacy thread project', 'legacy-thread', 7);
                 INSERT INTO threads(
                    id, project_path, title, workspace_path, branch,
                    provider_thread_id, ordinal, updated_at_ms
                 ) VALUES (
                    'legacy-thread', '/tmp/legacy-thread-project', 'Legacy thread',
                    '/tmp/legacy-thread-project', 'main', NULL, 1, 4242
                 );
                 INSERT INTO messages(thread_id, sequence, role, text)
                 VALUES
                    ('legacy-thread', 0, 'user', 'Legacy request'),
                    ('legacy-thread', 1, 'agent', 'Legacy response');",
            )
            .expect("write legacy thread schema");
        drop(connection);

        let store = StateStore::open(&database).expect("upgrade state database");
        let loaded = store
            .load_active_thread(Path::new("/tmp/legacy-thread-project"))
            .expect("load upgraded active thread")
            .expect("upgraded active thread exists");
        assert_eq!(loaded.draft, "");
        assert_eq!(loaded.activity, "waiting");
        assert_eq!(loaded.activity_updated_ms, 4242);
        assert_eq!(loaded.base_branch, None);
        assert_eq!(loaded.last_error, None);
        assert_eq!(loaded.dirty_count, 0);
        assert!(!loaded.unread);
        assert_eq!(loaded.events.len(), 2);
        assert_eq!(loaded.events[0].id, "legacy-message-0");
        assert!(matches!(
            &loaded.events[0].kind,
            CardKind::UserMessage { text, .. } if text == "Legacy request"
        ));
        assert!(matches!(
            &loaded.events[1].kind,
            CardKind::AssistantMessage { text } if text == "Legacy response"
        ));

        let mut event_schema = store
            .connection
            .prepare("PRAGMA table_info(conversation_events)")
            .expect("inspect event schema");
        let event_columns = event_schema
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query event schema")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect event schema");
        assert_eq!(
            event_columns,
            vec![
                "thread_id",
                "event_id",
                "sequence",
                "payload_json",
                "updated_at_ms"
            ]
        );
        drop(event_schema);

        drop(store);
        fs::remove_dir_all(root).expect("clean migration fixture");
    }
}
