use anyhow::{Context as _, Result};
use rusqlite::{Connection, OptionalExtension as _, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredThread {
    pub id: String,
    pub project_path: PathBuf,
    pub title: String,
    pub workspace_path: PathBuf,
    pub branch: Option<String>,
    pub provider_thread_id: Option<String>,
    pub ordinal: usize,
    pub messages: Vec<StoredMessage>,
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
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open Rode state database {}", path.display()))?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS projects (
                    path TEXT PRIMARY KEY NOT NULL,
                    name TEXT NOT NULL,
                    active_thread_id TEXT,
                    last_opened_ms INTEGER NOT NULL
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
                 );",
            )
            .context("failed to initialize Rode state schema")?;
        Ok(Self { connection })
    }

    pub fn save_thread(&mut self, thread: &StoredThread) -> Result<()> {
        let project_path = path_text(&thread.project_path);
        let workspace_path = path_text(&thread.workspace_path);
        let project_name = thread
            .project_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("project");
        let now = now_ms();
        let transaction = self
            .connection
            .transaction()
            .context("failed to begin Rode state transaction")?;
        transaction.execute(
            "INSERT INTO projects(path, name, active_thread_id, last_opened_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                name = excluded.name,
                active_thread_id = excluded.active_thread_id,
                last_opened_ms = excluded.last_opened_ms",
            params![project_path, project_name, thread.id, now],
        )?;
        transaction.execute(
            "INSERT INTO threads(
                id, project_path, title, workspace_path, branch,
                provider_thread_id, ordinal, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                project_path = excluded.project_path,
                title = excluded.title,
                workspace_path = excluded.workspace_path,
                branch = excluded.branch,
                provider_thread_id = excluded.provider_thread_id,
                ordinal = excluded.ordinal,
                updated_at_ms = excluded.updated_at_ms",
            params![
                thread.id,
                project_path,
                thread.title,
                workspace_path,
                thread.branch,
                thread.provider_thread_id,
                thread.ordinal as i64,
                now
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
        transaction.commit().context("failed to save Rode state")
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
                        provider_thread_id, ordinal
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
                    ))
                },
            )
            .optional()?;
        let Some((id, project_path, title, workspace_path, branch, provider_thread_id, ordinal)) =
            row
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
        Ok(Some(StoredThread {
            id,
            project_path: PathBuf::from(project_path),
            title,
            workspace_path: PathBuf::from(workspace_path),
            branch,
            provider_thread_id,
            ordinal: ordinal.max(0) as usize,
            messages,
        }))
    }
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{StateStore, StoredMessage, StoredThread};
    use std::fs;
    use std::path::PathBuf;
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
        let project = PathBuf::from("/tmp/rode-project");
        let worktree = PathBuf::from("/tmp/rode-worktree");
        let mut store = StateStore::open(&database).expect("open state database");
        let thread = StoredThread {
            id: "thread-1".to_owned(),
            project_path: project.clone(),
            title: "Native rendering".to_owned(),
            workspace_path: worktree.clone(),
            branch: Some("rode/thread-1-native-rendering".to_owned()),
            provider_thread_id: Some("provider-thread-1".to_owned()),
            ordinal: 1,
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
        };
        store.save_thread(&thread).expect("save thread");

        let loaded = store
            .load_active_thread(&project)
            .expect("load active thread")
            .expect("active thread exists");
        assert_eq!(loaded, thread);
        assert_eq!(store.load_threads(&project).unwrap(), vec![thread]);

        drop(store);
        fs::remove_dir_all(root).expect("clean state fixture");
    }
}
