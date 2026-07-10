use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use crate::actions::{
    CancelRename, DismissModal, RefreshRepo, SendPrompt, SubmitRename, ToggleDiff,
    ToggleDiffLayout, ToggleTerminal,
};
use crate::agent::{ProviderKind, ProviderStatus, discover_providers};
use crate::codex::{self, ApprovalRequest, CodexEvent, CodexSession};
use crate::codex_auth::{CodexAccount, begin_codex_login, read_codex_account};
use crate::diff::{
    DiffDocument, DiffFile, DiffHunk, DiffLine, DiffLineKind, DiffViewMode, split_rows,
};
use crate::editor::{Editor, standard_actions};
use crate::git::{
    RepoSnapshot, commit_all, create_pull_request, create_thread_worktree, push_current_branch,
};
use crate::notifications;
use crate::persistence::{StateStore, StoredMessage, StoredProject, StoredThread};
use crate::terminal::{TerminalCore, TerminalView};
use crate::theme;
use crate::ui::{button, modal, selectable_row, tabs, toast};
use gpui::{
    App, Context, CursorStyle, Div, Entity, IntoElement, PathPromptOptions, Render, Role,
    StyleRefinement, Subscription, Window, div, prelude::*, px, rgb,
};

const ISOLATE_NEW_THREADS_SETTING: &str = "isolate_new_threads";

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsSection {
    #[default]
    Appearance,
    AgentsAndModels,
    Terminal,
    GitAndWorktrees,
    Keybindings,
    Account,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AppRoute {
    Login,
    #[default]
    Workspace,
    SourceControl,
    Terminal,
    Settings(SettingsSection),
}

impl AppRoute {
    fn key_context(self) -> &'static str {
        match self {
            Self::Login => "Login",
            Self::Workspace => "Workspace",
            Self::SourceControl => "SourceControl",
            Self::Terminal => "TerminalRoute",
            Self::Settings(_) => "Settings",
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModalState {
    NewThread,
    ProjectPicker,
    ModelPicker,
    AccessPicker,
    AttachmentPicker,
    Commit,
    Account,
    CommandPalette,
}

impl ModalState {
    fn title(self) -> &'static str {
        match self {
            Self::NewThread => "New thread",
            Self::ProjectPicker => "Open project",
            Self::ModelPicker => "Choose model",
            Self::AccessPicker => "Runtime access",
            Self::AttachmentPicker => "Add context",
            Self::Commit => "Commit changes",
            Self::Account => "OpenAI account",
            Self::CommandPalette => "Command palette",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageRole {
    User,
    Agent,
    Tool,
    System,
}

impl MessageRole {
    fn storage_name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::System => "system",
        }
    }

    fn from_storage_name(value: &str) -> Self {
        match value {
            "user" => Self::User,
            "agent" => Self::Agent,
            "tool" => Self::Tool,
            _ => Self::System,
        }
    }
}

#[derive(Clone, Debug)]
struct Message {
    role: MessageRole,
    text: String,
}

#[derive(Clone, Debug)]
enum CodexAuthState {
    Unavailable,
    Checking,
    SignedOut,
    SignedIn(CodexAccount),
    SigningIn,
    Error(String),
}

impl CodexAuthState {
    fn requires_onboarding(&self) -> bool {
        !matches!(self, Self::SignedIn(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RenameTarget {
    Project(PathBuf),
    Thread(String),
}

enum PublishOperation {
    Commit(String),
    Push,
    PullRequest(String),
}

pub(crate) struct RodeApp {
    route: AppRoute,
    modal: Option<ModalState>,
    toasts: toast::ToastQueue,
    state_store: Option<StateStore>,
    known_projects: Vec<StoredProject>,
    known_threads: Vec<StoredThread>,
    project_root: PathBuf,
    project_path: PathBuf,
    project_name: String,
    repo: RepoSnapshot,
    providers: Vec<ProviderStatus>,
    codex_auth: CodexAuthState,
    messages: Vec<Message>,
    composer: Entity<Editor>,
    commit_editor: Entity<Editor>,
    rename_editor: Entity<Editor>,
    rename_target: Option<RenameTarget>,
    _rename_blur_subscription: Subscription,
    thread_id: String,
    thread_title: String,
    thread_branch: Option<String>,
    codex_session: Option<CodexSession>,
    codex_thread_id: Option<String>,
    active_turn_id: Option<String>,
    active_agent_message: Option<usize>,
    reasoning_preview: String,
    approvals: Vec<ApprovalRequest>,
    session_generation: u64,
    creating_worktree: bool,
    isolate_new_threads: bool,
    show_create_menu: bool,
    show_settings: bool,
    running: bool,
    show_diff: bool,
    diff_view: DiffViewMode,
    git_operation: Option<&'static str>,
    publish_status: Option<String>,
    show_terminal: bool,
    terminal_sessions: HashMap<String, Entity<TerminalView>>,
    thread_number: usize,
}

impl RodeApp {
    pub(crate) fn new(project_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let initial_repo = RepoSnapshot::load(&project_path);
        let project_root = initial_repo.root.clone();
        let (state_store, restored_thread, isolate_new_threads, persistence_error) =
            match StateStore::open_default() {
                Ok(store) => {
                    let isolate_new_threads = store
                        .load_bool_setting(ISOLATE_NEW_THREADS_SETTING, false)
                        .unwrap_or(false);
                    match store.load_active_thread(&project_root) {
                        Ok(thread) => (Some(store), thread, isolate_new_threads, None),
                        Err(error) => (
                            Some(store),
                            None,
                            isolate_new_threads,
                            Some(format!("Could not restore Rode state: {error:#}")),
                        ),
                    }
                }
                Err(error) => (
                    None,
                    None,
                    false,
                    Some(format!("Could not open Rode state database: {error:#}")),
                ),
            };
        let restored_workspace = restored_thread
            .as_ref()
            .map(|thread| thread.workspace_path.clone())
            .filter(|path| path.is_dir());
        let project_path = restored_workspace.unwrap_or_else(|| project_root.clone());
        let repo = RepoSnapshot::load(&project_path);
        let project_name = restored_thread
            .as_ref()
            .map(|thread| thread.project_name.clone())
            .unwrap_or_else(|| folder_name(&project_root));
        let composer = cx.new(|cx| {
            Editor::new(
                "",
                "Ask the agent to inspect, change, or explain the project…",
                window,
                cx,
            )
        });
        let composer_focus = composer.read(cx).focus_handle.clone();
        window.focus(&composer_focus, cx);
        let commit_editor =
            cx.new(|cx| Editor::new("", "Commit message or pull-request title", window, cx));
        let rename_editor = cx.new(|cx| Editor::new("", "New name", window, cx));
        let rename_focus = rename_editor.read(cx).focus_handle.clone();
        let rename_blur_subscription =
            cx.on_blur(&rename_focus, window, |this, _, cx| this.clear_rename(cx));

        let providers = discover_providers();
        let codex_auth = if providers
            .iter()
            .any(|provider| provider.kind == ProviderKind::Codex && provider.available)
        {
            CodexAuthState::Checking
        } else {
            CodexAuthState::Unavailable
        };

        let mut messages = restored_thread
            .as_ref()
            .map(|thread| {
                thread
                    .messages
                    .iter()
                    .map(|message| Message {
                        role: MessageRole::from_storage_name(&message.role),
                        text: message.text.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|messages| !messages.is_empty())
            .unwrap_or_else(|| {
                vec![Message {
                    role: MessageRole::System,
                    text: "Rode is using the native Wayland renderer. Codex turns run in the workspace-write sandbox by default.".to_owned(),
                }]
            });
        if let Some(error) = persistence_error {
            messages.push(Message {
                role: MessageRole::System,
                text: error,
            });
        }
        let thread_id = restored_thread
            .as_ref()
            .map(|thread| thread.id.clone())
            .unwrap_or_else(new_local_thread_id);
        let thread_branch = restored_thread
            .as_ref()
            .and_then(|thread| thread.branch.clone());
        let codex_thread_id = restored_thread
            .as_ref()
            .and_then(|thread| thread.provider_thread_id.clone());
        let thread_number = restored_thread
            .as_ref()
            .map(|thread| thread.ordinal.max(1))
            .unwrap_or(1);
        let thread_title = restored_thread
            .as_ref()
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| format!("Thread {thread_number}"));

        let mut app = Self {
            route: if codex_auth.requires_onboarding() {
                AppRoute::Login
            } else {
                AppRoute::Workspace
            },
            modal: None,
            toasts: toast::ToastQueue::default(),
            state_store,
            known_projects: Vec::new(),
            known_threads: Vec::new(),
            project_root,
            project_path,
            project_name,
            repo,
            providers,
            codex_auth,
            messages,
            composer,
            commit_editor,
            rename_editor,
            rename_target: None,
            _rename_blur_subscription: rename_blur_subscription,
            thread_id,
            thread_title,
            thread_branch,
            codex_session: None,
            codex_thread_id,
            active_turn_id: None,
            active_agent_message: None,
            reasoning_preview: String::new(),
            approvals: Vec::new(),
            session_generation: 0,
            creating_worktree: false,
            isolate_new_threads,
            show_create_menu: false,
            show_settings: false,
            running: false,
            show_diff: true,
            diff_view: DiffViewMode::Split,
            git_operation: None,
            publish_status: None,
            show_terminal: false,
            terminal_sessions: HashMap::new(),
            thread_number,
        };
        app.persist_current_thread();
        app
    }

    fn persist_current_thread(&mut self) {
        let Some(store) = self.state_store.as_mut() else {
            return;
        };
        let thread = StoredThread {
            id: self.thread_id.clone(),
            project_path: self.project_root.clone(),
            project_name: self.project_name.clone(),
            title: self.thread_title.clone(),
            workspace_path: self.project_path.clone(),
            branch: self.thread_branch.clone(),
            provider_thread_id: self.codex_thread_id.clone(),
            ordinal: self.thread_number,
            messages: self
                .messages
                .iter()
                .map(|message| StoredMessage {
                    role: message.role.storage_name().to_owned(),
                    text: message.text.clone(),
                })
                .collect(),
        };
        if let Err(error) = store.save_thread(&thread) {
            eprintln!("failed to persist Rode thread state: {error:#}");
        } else {
            self.refresh_known_state();
        }
    }

    fn refresh_known_state(&mut self) {
        let Some(store) = self.state_store.as_ref() else {
            return;
        };
        let projects = match store.load_projects() {
            Ok(projects) => projects,
            Err(error) => {
                eprintln!("failed to load Rode projects: {error:#}");
                return;
            }
        };
        let mut threads = Vec::new();
        for project in &projects {
            match store.load_threads(&project.path) {
                Ok(project_threads) => threads.extend(project_threads),
                Err(error) => eprintln!(
                    "failed to load threads for {}: {error:#}",
                    project.path.display()
                ),
            }
        }
        self.known_projects = projects;
        self.known_threads = threads;
    }

    fn switch_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        if self.thread_id == thread_id {
            return;
        }
        self.persist_current_thread();
        let Some(thread) = self
            .known_threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .cloned()
        else {
            return;
        };
        self.session_generation += 1;
        self.codex_session = None;
        self.running = false;
        self.creating_worktree = false;
        self.active_turn_id = None;
        self.active_agent_message = None;
        self.reasoning_preview.clear();
        self.approvals.clear();
        self.git_operation = None;
        self.publish_status = None;
        self.commit_editor
            .update(cx, |editor, cx| editor.set_text("", cx));
        self.thread_id = thread.id;
        self.thread_branch = thread.branch;
        self.codex_thread_id = thread.provider_thread_id;
        self.thread_number = thread.ordinal.max(1);
        self.thread_title = thread.title;
        self.project_root = thread.project_path;
        self.project_name = thread.project_name;
        self.project_path = if thread.workspace_path.is_dir() {
            thread.workspace_path
        } else {
            self.project_root.clone()
        };
        self.repo = RepoSnapshot::load(&self.project_path);
        self.messages = thread
            .messages
            .into_iter()
            .map(|message| Message {
                role: MessageRole::from_storage_name(&message.role),
                text: message.text,
            })
            .collect();
        if self.messages.is_empty() {
            self.messages.push(Message {
                role: MessageRole::System,
                text: "Restored thread has no messages yet.".to_owned(),
            });
        }
        self.persist_current_thread();
        cx.notify();
    }

    fn codex_available(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.kind == ProviderKind::Codex && provider.available)
    }

    fn codex_authenticated(&self) -> bool {
        matches!(self.codex_auth, CodexAuthState::SignedIn(_))
    }

    fn sync_route_with_auth(&mut self) {
        self.route = route_after_auth(self.route, self.codex_auth.requires_onboarding());
    }

    pub(crate) fn refresh_codex_account(&mut self, cx: &mut Context<Self>) {
        if !self.codex_available() {
            self.codex_auth = CodexAuthState::Unavailable;
            self.toasts.push(
                toast::ToastKind::Warning,
                "Codex is unavailable; authentication cannot start.",
            );
            self.sync_route_with_auth();
            cx.notify();
            return;
        }

        self.codex_auth = CodexAuthState::Checking;
        self.sync_route_with_auth();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { read_codex_account() })
                .await;
            this.update(cx, |this, cx| {
                this.codex_auth = match result {
                    Ok(status) => status
                        .account
                        .map(CodexAuthState::SignedIn)
                        .unwrap_or(CodexAuthState::SignedOut),
                    Err(error) => CodexAuthState::Error(format!("{error:#}")),
                };
                this.sync_route_with_auth();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sign_in_codex(&mut self, cx: &mut Context<Self>) {
        if !self.codex_available() || matches!(self.codex_auth, CodexAuthState::SigningIn) {
            return;
        }

        self.codex_auth = CodexAuthState::SigningIn;
        self.sync_route_with_auth();
        self.messages.push(Message {
            role: MessageRole::System,
            text: "Starting a secure ChatGPT sign-in through Codex…".to_owned(),
        });
        self.toasts
            .push(toast::ToastKind::Info, "Opening ChatGPT sign-in…");
        cx.notify();

        cx.spawn(async move |this, cx| {
            let pending = cx
                .background_spawn(async move { begin_codex_login() })
                .await;
            let pending = match pending {
                Ok(pending) => pending,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        let detail = format!("{error:#}");
                        this.codex_auth = CodexAuthState::Error(detail.clone());
                        this.sync_route_with_auth();
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Could not start Codex login: {detail}"),
                        });
                        this.toasts.push(
                            toast::ToastKind::Error,
                            "Could not start ChatGPT sign-in.",
                        );
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let auth_url = pending.auth_url.clone();
            if this
                .update(cx, |this, cx| {
                    cx.open_url(&auth_url);
                    this.messages.push(Message {
                        role: MessageRole::System,
                        text: "Complete sign-in in your browser. Rode is waiting for Codex to confirm it."
                            .to_owned(),
                    });
                    cx.notify();
                })
                .is_err()
            {
                return;
            }

            let result = cx.background_spawn(async move { pending.wait() }).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(status) => {
                        this.codex_auth = status
                            .account
                            .map(CodexAuthState::SignedIn)
                            .unwrap_or(CodexAuthState::SignedOut);
                        this.sync_route_with_auth();
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: "Signed in to OpenAI through Codex. You can now start a thread."
                                .to_owned(),
                        });
                        this.toasts
                            .push(toast::ToastKind::Success, "ChatGPT sign-in complete.");
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        this.codex_auth = CodexAuthState::Error(detail.clone());
                        this.sync_route_with_auth();
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Codex login did not complete: {detail}"),
                        });
                        this.toasts.push(
                            toast::ToastKind::Error,
                            "ChatGPT sign-in did not complete.",
                        );
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_prompt(&mut self, _: &SendPrompt, _: &mut Window, cx: &mut Context<Self>) {
        if self.running || self.creating_worktree {
            return;
        }

        let prompt = self.composer.read(cx).text();
        let prompt = prompt.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        if !self.codex_available() {
            self.messages.push(Message {
                role: MessageRole::System,
                text: "Codex was not found on PATH. Install and authenticate the Codex CLI, then restart Rode.".to_owned(),
            });
            cx.notify();
            return;
        }
        if !self.codex_authenticated() {
            self.messages.push(Message {
                role: MessageRole::System,
                text: "Sign in with ChatGPT from the Codex card in the sidebar before starting a thread."
                    .to_owned(),
            });
            cx.notify();
            return;
        }

        self.composer.update(cx, |editor, cx| editor.clear(cx));
        self.messages.push(Message {
            role: MessageRole::User,
            text: prompt.clone(),
        });
        self.active_agent_message = None;
        self.reasoning_preview.clear();
        self.running = true;
        self.persist_current_thread();
        cx.notify();

        let generation = self.session_generation;
        if let Some(session) = self.codex_session.clone() {
            Self::spawn_turn(session, prompt, generation, cx);
            return;
        }

        let cwd = self.project_path.clone();
        let resume_thread_id = self.codex_thread_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let (session, events) = CodexSession::start(&cwd, resume_thread_id.as_deref())?;
                    session.start_turn(&prompt)?;
                    anyhow::Ok((session, events))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.session_generation != generation {
                    return;
                }
                match result {
                    Ok((session, events)) => {
                        this.codex_session = Some(session);
                        this.start_codex_event_pump(events, generation, cx);
                    }
                    Err(error) => {
                        this.running = false;
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Could not start Codex app-server: {error:#}"),
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn spawn_turn(session: CodexSession, prompt: String, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { session.start_turn(&prompt) })
                .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    if this.session_generation == generation {
                        this.running = false;
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Could not start Codex turn: {error:#}"),
                        });
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn start_codex_event_pump(
        &mut self,
        events: async_channel::Receiver<CodexEvent>,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| {
                        if this.session_generation == generation {
                            this.handle_codex_event(event, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_codex_event(&mut self, event: CodexEvent, cx: &mut Context<Self>) {
        match event {
            CodexEvent::SessionReady { thread_id, model } => {
                self.codex_thread_id = Some(thread_id);
                self.messages.push(Message {
                    role: MessageRole::System,
                    text: format!("Codex app-server session ready · {model}"),
                });
                self.persist_current_thread();
            }
            CodexEvent::TurnStarted { turn_id } => {
                self.active_turn_id = Some(turn_id);
            }
            CodexEvent::AgentMessageDelta { delta } => {
                let index = match self.active_agent_message {
                    Some(index) if index < self.messages.len() => index,
                    _ => {
                        self.messages.push(Message {
                            role: MessageRole::Agent,
                            text: String::new(),
                        });
                        let index = self.messages.len() - 1;
                        self.active_agent_message = Some(index);
                        index
                    }
                };
                self.messages[index].text.push_str(&delta);
            }
            CodexEvent::AgentMessageCompleted { text } => {
                if let Some(index) = self.active_agent_message
                    && index < self.messages.len()
                {
                    self.messages[index].text = text;
                } else if !text.trim().is_empty() {
                    self.messages.push(Message {
                        role: MessageRole::Agent,
                        text,
                    });
                }
                self.active_agent_message = None;
                self.persist_current_thread();
            }
            CodexEvent::ReasoningDelta { delta } => {
                self.reasoning_preview.push_str(&delta);
                const MAX_REASONING_PREVIEW: usize = 240;
                if self.reasoning_preview.len() > MAX_REASONING_PREVIEW {
                    let keep_from = self.reasoning_preview.len() - MAX_REASONING_PREVIEW;
                    let keep_from = self.reasoning_preview.floor_char_boundary(keep_from);
                    self.reasoning_preview.drain(..keep_from);
                }
            }
            CodexEvent::CommandStarted {
                item_id,
                command,
                cwd,
            } => self.messages.push(Message {
                role: MessageRole::Tool,
                text: format!("$ {command}\n{cwd}\nitem {item_id}"),
            }),
            CodexEvent::CommandCompleted {
                item_id,
                command,
                exit_code,
                output,
            } => {
                let output = output.lines().take(20).collect::<Vec<_>>().join("\n");
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    text: format!(
                        "$ {command}\nexit {} · item {item_id}{}",
                        exit_code
                            .map(|code| code.to_string())
                            .unwrap_or_else(|| "?".to_owned()),
                        if output.is_empty() {
                            String::new()
                        } else {
                            format!("\n\n{output}")
                        }
                    ),
                });
            }
            CodexEvent::FileChangeStarted { item_id, summary } => {
                self.messages.push(Message {
                    role: MessageRole::Tool,
                    text: format!("Editing files · {summary}\nitem {item_id}"),
                });
            }
            CodexEvent::ApprovalRequested(request) => self.approvals.push(request),
            CodexEvent::TurnCompleted { status, error } => {
                self.running = false;
                self.active_turn_id = None;
                self.active_agent_message = None;
                self.reasoning_preview.clear();
                self.repo = RepoSnapshot::load(&self.project_path);
                let failed = error.is_some() || !matches!(status.as_str(), "completed" | "success");
                notifications::turn_finished(&self.thread_title, &status, failed);
                if let Some(error) = error {
                    self.messages.push(Message {
                        role: MessageRole::System,
                        text: format!("Codex turn {status}: {error}"),
                    });
                }
                self.persist_current_thread();
            }
            CodexEvent::Error(error) => self.messages.push(Message {
                role: MessageRole::System,
                text: format!("Codex app-server: {error}"),
            }),
            CodexEvent::Exited => {
                self.codex_session = None;
                if self.running {
                    self.running = false;
                    self.messages.push(Message {
                        role: MessageRole::System,
                        text: "Codex app-server exited before the turn completed.".to_owned(),
                    });
                }
                self.persist_current_thread();
            }
        }
        cx.notify();
    }

    fn respond_to_approval(
        &mut self,
        index: usize,
        decision: &'static str,
        cx: &mut Context<Self>,
    ) {
        if index >= self.approvals.len() {
            return;
        }
        let request = self.approvals.remove(index);
        let result = self.codex_session.as_ref().map_or_else(
            || Err(anyhow::anyhow!("Codex session is no longer running")),
            |session| session.respond_to_approval(&request.rpc_id, decision),
        );
        let outcome = if let Err(error) = result {
            format!("Could not answer approval request: {error:#}")
        } else {
            format!(
                "{} request {}.",
                match request.kind {
                    codex::ApprovalKind::Command => "Command",
                    codex::ApprovalKind::FileChange => "File-change",
                },
                if decision == "accept" {
                    "approved"
                } else {
                    "declined"
                }
            )
        };
        self.messages.push(Message {
            role: MessageRole::System,
            text: outcome,
        });
        self.persist_current_thread();
        cx.notify();
    }

    fn cancel_turn(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.codex_session.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { session.interrupt() })
                .await;
            if let Err(error) = result {
                this.update(cx, |this, cx| {
                    this.messages.push(Message {
                        role: MessageRole::System,
                        text: format!("Could not cancel Codex turn: {error:#}"),
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn toggle_create_menu(&mut self, cx: &mut Context<Self>) {
        self.show_create_menu = !self.show_create_menu;
        self.show_settings = false;
        cx.notify();
    }

    fn toggle_settings(&mut self, cx: &mut Context<Self>) {
        self.show_settings = !self.show_settings;
        self.show_create_menu = false;
        cx.notify();
    }

    fn toggle_thread_isolation(&mut self, cx: &mut Context<Self>) {
        self.isolate_new_threads = !self.isolate_new_threads;
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) =
                store.save_bool_setting(ISOLATE_NEW_THREADS_SETTING, self.isolate_new_threads)
        {
            self.messages.push(Message {
                role: MessageRole::System,
                text: format!("Could not save the thread isolation setting: {error:#}"),
            });
        }
        cx.notify();
    }

    fn open_folder_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_create_menu = false;
        cx.notify();
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match selection.await {
                Ok(result) => result,
                Err(_) => return,
            };
            this.update_in(cx, |this, _window, cx| match result {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.add_project(path, cx);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    this.messages.push(Message {
                        role: MessageRole::System,
                        text: format!("Could not open the folder picker: {error:#}"),
                    });
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn add_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        self.persist_current_thread();
        self.refresh_known_state();

        let existing_project = self
            .known_projects
            .iter()
            .find(|project| project.path == path)
            .cloned();
        if let Some(thread_id) = existing_project
            .as_ref()
            .and_then(|project| project.active_thread_id.clone())
            .or_else(|| {
                self.known_threads
                    .iter()
                    .find(|thread| thread.project_path == path)
                    .map(|thread| thread.id.clone())
            })
        {
            self.switch_thread(&thread_id, cx);
            return;
        }

        self.project_root = path.clone();
        self.project_path = path.clone();
        self.project_name = existing_project
            .map(|project| project.name)
            .unwrap_or_else(|| folder_name(&path));
        self.repo = RepoSnapshot::load(&path);
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.save_project(&StoredProject {
                path,
                name: self.project_name.clone(),
                active_thread_id: None,
            })
        {
            self.messages.push(Message {
                role: MessageRole::System,
                text: format!("Could not save the project folder: {error:#}"),
            });
        }
        self.start_new_thread(false, cx);
    }

    fn begin_rename(&mut self, target: RenameTarget, window: &mut Window, cx: &mut Context<Self>) {
        let value = match &target {
            RenameTarget::Project(path) => self
                .known_projects
                .iter()
                .find(|project| &project.path == path)
                .map(|project| project.name.clone()),
            RenameTarget::Thread(id) => self
                .known_threads
                .iter()
                .find(|thread| &thread.id == id)
                .map(|thread| thread.title.clone()),
        };
        let Some(value) = value else {
            return;
        };
        self.rename_target = Some(target);
        self.rename_editor
            .update(cx, |editor, cx| editor.set_text(value, cx));
        let focus = self.rename_editor.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    fn submit_rename(&mut self, _: &SubmitRename, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.rename_editor.read(cx).text();
        let value = value.trim();
        let Some(target) = self.rename_target.take() else {
            return;
        };
        if !value.is_empty() {
            let result = match target {
                RenameTarget::Project(path) => {
                    if self.project_root == path {
                        self.project_name = value.to_owned();
                    }
                    self.state_store
                        .as_mut()
                        .map(|store| store.rename_project(&path, value))
                }
                RenameTarget::Thread(id) => {
                    if self.thread_id == id {
                        self.thread_title = value.to_owned();
                    }
                    self.state_store
                        .as_mut()
                        .map(|store| store.rename_thread(&id, value))
                }
            };
            if let Some(Err(error)) = result {
                self.messages.push(Message {
                    role: MessageRole::System,
                    text: format!("Could not rename item: {error:#}"),
                });
            }
            self.refresh_known_state();
        }
        let focus = self.composer.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    fn clear_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename_target.take().is_some() {
            cx.notify();
        }
    }

    fn cancel_rename(&mut self, _: &CancelRename, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_rename(cx);
        let focus = self.composer.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
    }

    fn dismiss_modal(&mut self, _: &DismissModal, window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.take().is_some() {
            let focus = self.composer.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    fn new_thread(&mut self, cx: &mut Context<Self>) {
        self.show_create_menu = false;
        self.start_new_thread(true, cx);
    }

    fn new_thread_in_project(
        &mut self,
        project_path: PathBuf,
        project_name: String,
        cx: &mut Context<Self>,
    ) {
        self.persist_current_thread();
        self.project_root = project_path.clone();
        self.project_path = project_path;
        self.project_name = project_name;
        self.repo = RepoSnapshot::load(&self.project_path);
        self.start_new_thread(false, cx);
    }

    fn start_new_thread(&mut self, persist_previous: bool, cx: &mut Context<Self>) {
        if persist_previous {
            self.persist_current_thread();
        }
        self.session_generation += 1;
        self.codex_session = None;
        self.codex_thread_id = None;
        self.active_turn_id = None;
        self.active_agent_message = None;
        self.reasoning_preview.clear();
        self.approvals.clear();
        self.git_operation = None;
        self.publish_status = None;
        self.commit_editor
            .update(cx, |editor, cx| editor.set_text("", cx));
        self.running = false;
        self.thread_number = self
            .known_threads
            .iter()
            .filter(|thread| thread.project_path == self.project_root)
            .map(|thread| thread.ordinal)
            .max()
            .unwrap_or(0)
            + 1;
        self.thread_id = new_local_thread_id();
        self.thread_title = format!("Thread {}", self.thread_number);
        self.thread_branch = None;
        self.project_path = self.project_root.clone();
        self.repo = RepoSnapshot::load(&self.project_path);
        self.messages = vec![Message {
            role: MessageRole::System,
            text: if self.isolate_new_threads {
                "Creating an isolated Git worktree for the new thread…".to_owned()
            } else {
                "New local thread in the project folder. The first prompt will open a new Codex app-server session.".to_owned()
            },
        }];
        self.creating_worktree = self.isolate_new_threads;
        self.persist_current_thread();
        cx.notify();

        if !self.isolate_new_threads {
            return;
        }

        let generation = self.session_generation;
        let repository = self.project_root.clone();
        let thread_id = self.thread_id.clone();
        let title = self.thread_title.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    create_thread_worktree(&repository, &thread_id, &title)
                })
                .await;
            this.update(cx, |this, cx| {
                if this.session_generation != generation {
                    return;
                }
                match result {
                    Ok(worktree) => {
                        this.project_path = worktree.path;
                        this.thread_branch = Some(worktree.branch.clone());
                        this.repo = RepoSnapshot::load(&this.project_path);
                        this.messages = vec![Message {
                            role: MessageRole::System,
                            text: format!(
                                "Isolated worktree ready on `{}`. The first prompt will open a new Codex app-server session.",
                                worktree.branch
                            ),
                        }];
                    }
                    Err(error) => {
                        this.project_path = this.project_root.clone();
                        this.thread_branch = None;
                        this.repo = RepoSnapshot::load(&this.project_path);
                        this.messages = vec![Message {
                            role: MessageRole::System,
                            text: format!(
                                "Could not create an isolated worktree: {error:#}. This thread is using the main checkout."
                            ),
                        }];
                    }
                }
                this.creating_worktree = false;
                this.persist_current_thread();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_diff(&mut self, _: &ToggleDiff, _: &mut Window, cx: &mut Context<Self>) {
        self.show_diff = !self.show_diff;
        cx.notify();
    }

    fn toggle_diff_layout(&mut self, _: &ToggleDiffLayout, _: &mut Window, cx: &mut Context<Self>) {
        self.diff_view = match self.diff_view {
            DiffViewMode::Stack => DiffViewMode::Split,
            DiffViewMode::Split => DiffViewMode::Stack,
        };
        cx.notify();
    }

    fn ensure_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating_worktree || self.terminal_sessions.contains_key(&self.thread_id) {
            return;
        }
        match TerminalCore::start(&self.project_path) {
            Ok(core) => {
                let terminal = cx.new(|cx| TerminalView::new(core, window, cx));
                let focus = terminal.read(cx).focus_handle.clone();
                self.terminal_sessions
                    .insert(self.thread_id.clone(), terminal);
                window.focus(&focus, cx);
            }
            Err(error) => {
                self.show_terminal = false;
                self.messages.push(Message {
                    role: MessageRole::System,
                    text: format!("Could not start the native terminal: {error:#}"),
                });
                self.persist_current_thread();
            }
        }
    }

    fn toggle_terminal(&mut self, _: &ToggleTerminal, window: &mut Window, cx: &mut Context<Self>) {
        self.show_terminal = !self.show_terminal;
        if self.show_terminal {
            self.ensure_terminal(window, cx);
            if let Some(terminal) = self.terminal_sessions.get(&self.thread_id) {
                let focus = terminal.read(cx).focus_handle.clone();
                terminal.update(cx, |_, cx| cx.notify());
                window.focus(&focus, cx);
            }
        } else {
            let focus = self.composer.read(cx).focus_handle.clone();
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    fn refresh_repo(&mut self, _: &RefreshRepo, _: &mut Window, cx: &mut Context<Self>) {
        self.repo = RepoSnapshot::load(&self.project_path);
        cx.notify();
    }

    fn start_publish_operation(&mut self, operation: PublishOperation, cx: &mut Context<Self>) {
        if self.git_operation.is_some() {
            return;
        }
        let label = match &operation {
            PublishOperation::Commit(_) => "Committing",
            PublishOperation::Push => "Pushing",
            PublishOperation::PullRequest(_) => "Creating pull request",
        };
        let workspace = self.project_path.clone();
        self.git_operation = Some(label);
        self.publish_status = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match operation {
                        PublishOperation::Commit(message) => commit_all(&workspace, &message)
                            .map(|commit| format!("Committed {commit}")),
                        PublishOperation::Push => push_current_branch(&workspace)
                            .map(|branch| format!("Pushed `{branch}` to origin")),
                        PublishOperation::PullRequest(title) => {
                            create_pull_request(&workspace, &title)
                                .map(|url| format!("Pull request: {url}"))
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.git_operation = None;
                this.publish_status = Some(match result {
                    Ok(message) => message,
                    Err(error) => format!("Git workflow failed: {error:#}"),
                });
                this.repo = RepoSnapshot::load(&this.project_path);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn commit_changes(&mut self, cx: &mut Context<Self>) {
        let message = self.commit_editor.read(cx).text();
        self.start_publish_operation(PublishOperation::Commit(message), cx);
    }

    fn push_changes(&mut self, cx: &mut Context<Self>) {
        self.start_publish_operation(PublishOperation::Push, cx);
    }

    fn create_pr(&mut self, cx: &mut Context<Self>) {
        let title = self.commit_editor.read(cx).text();
        self.start_publish_operation(PublishOperation::PullRequest(title), cx);
    }

    fn render_inline_rename(&self, cx: &mut Context<Self>) -> Div {
        let focus = self.rename_editor.read(cx).focus_handle.clone();
        div()
            .key_context("Rename")
            .track_focus(&focus)
            .map(standard_actions(self.rename_editor.clone()))
            .on_mouse_down_out(
                cx.listener(|this, _, window, cx| this.cancel_rename(&CancelRename, window, cx)),
            )
            .w_full()
            .h(px(30.))
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x3b82f6))
            .bg(rgb(0x20232a))
            .text_sm()
            .text_color(rgb(0xf3f4f6))
            .child(
                self.rename_editor
                    .clone()
                    .cached(StyleRefinement::default().w_full()),
            )
    }

    fn render_project_group(
        &self,
        project_index: usize,
        project: &StoredProject,
        cx: &mut Context<Self>,
    ) -> Div {
        let project_path = project.path.clone();
        let project_path_for_rename = project.path.clone();
        let project_path_for_thread = project.path.clone();
        let project_name_for_thread = project.name.clone();
        let project_is_active = project.path == self.project_root;
        let renaming_project =
            self.rename_target == Some(RenameTarget::Project(project.path.clone()));
        let threads = self
            .known_threads
            .iter()
            .filter(|thread| thread.project_path == project.path)
            .collect::<Vec<_>>();

        div()
            .rounded_lg()
            .p_2()
            .bg(if project_is_active {
                rgb(0x1c2028)
            } else {
                rgb(0x171a20)
            })
            .border_1()
            .border_color(if project_is_active {
                rgb(0x374151)
            } else {
                rgb(0x292c33)
            })
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(renaming_project, |header| {
                                header.child(self.render_inline_rename(cx))
                            })
                            .when(!renaming_project, |header| {
                                header.child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(0xe5e7eb))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(project.name.clone()),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x666d7a))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(project_path.display().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .id(format!("new-thread-project-{project_index}"))
                                    .role(Role::Button)
                                    .aria_label(format!("New thread in {}", project.name))
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_sm()
                                    .text_color(rgb(0x8b93a3))
                                    .hover(|style| style.bg(rgb(0x2b303a)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.new_thread_in_project(
                                            project_path_for_thread.clone(),
                                            project_name_for_thread.clone(),
                                            cx,
                                        )
                                    }))
                                    .child("+"),
                            )
                            .child(
                                div()
                                    .id(format!("rename-project-{project_index}"))
                                    .role(Role::Button)
                                    .aria_label(format!("Rename project {}", project.name))
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(rgb(0x8b93a3))
                                    .hover(|style| style.bg(rgb(0x2b303a)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.begin_rename(
                                            RenameTarget::Project(project_path_for_rename.clone()),
                                            window,
                                            cx,
                                        )
                                    }))
                                    .child("✎"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(threads.into_iter().map(|thread| {
                        let active = thread.id == self.thread_id;
                        let local_thread_id = thread.id.clone();
                        let thread_id_for_rename = thread.id.clone();
                        let renaming_thread =
                            self.rename_target == Some(RenameTarget::Thread(thread.id.clone()));
                        let session = thread
                            .provider_thread_id
                            .as_deref()
                            .map(|id| id.chars().take(8).collect::<String>())
                            .unwrap_or_else(|| "not started".to_owned());
                        div()
                            .id(format!("thread-{}", thread.id))
                            .rounded_lg()
                            .p_2()
                            .bg(if active { rgb(0x272b35) } else { rgb(0x1a1d23) })
                            .border_1()
                            .border_color(if active { rgb(0x3b82f6) } else { rgb(0x292c33) })
                            .flex()
                            .items_start()
                            .gap_1()
                            .child(
                                div()
                                    .id(format!("thread-content-{}", thread.id))
                                    .min_w_0()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .when(!renaming_thread, |content| {
                                        content.cursor_pointer().on_click(cx.listener(
                                            move |this, _, _, cx| {
                                                this.switch_thread(&local_thread_id, cx)
                                            },
                                        ))
                                    })
                                    .when(renaming_thread, |content| {
                                        content.child(self.render_inline_rename(cx))
                                    })
                                    .when(!renaming_thread, |content| {
                                        content.child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(0xf3f4f6))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(thread.title.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(0x8b93a3))
                                            .child(format!("session {session}")),
                                    ),
                            )
                            .child(
                                div()
                                    .id(format!("rename-thread-{}", thread.id))
                                    .role(Role::Button)
                                    .aria_label(format!("Rename thread {}", thread.title))
                                    .flex_none()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .text_xs()
                                    .text_color(rgb(0x8b93a3))
                                    .hover(|style| style.bg(rgb(0x343946)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.begin_rename(
                                            RenameTarget::Thread(thread_id_for_rename.clone()),
                                            window,
                                            cx,
                                        )
                                    }))
                                    .child("✎"),
                            )
                    })),
            )
    }

    fn render_auth_onboarding(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let (status_color, status_label) = match &self.codex_auth {
            CodexAuthState::Unavailable => (0xf87171, "CLI not found"),
            CodexAuthState::Checking => (0xfbbf24, "Checking account"),
            CodexAuthState::SignedOut => (0x60a5fa, "Ready to connect"),
            CodexAuthState::SigningIn => (0x60a5fa, "Waiting for browser"),
            CodexAuthState::Error(_) => (0xf87171, "Connection error"),
            CodexAuthState::SignedIn(_) => (0x34d399, "Connected"),
        };
        let error = match &self.codex_auth {
            CodexAuthState::Error(error) => Some(error.clone()),
            _ => None,
        };

        div()
            .id("auth-onboarding")
            .size_full()
            .min_w(px(720.))
            .flex()
            .flex_col()
            .bg(rgb(0x0f1115))
            .text_color(rgb(0xd1d5db))
            .child(
                div()
                    .h(px(64.))
                    .flex_none()
                    .px_6()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(0x252932))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xf3f4f6))
                            .child("RODE"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_8()
                    .child(
                        div()
                            .w(px(560.))
                            .rounded_xl()
                            .border_1()
                            .border_color(rgb(0x343946))
                            .bg(rgb(0x191d25))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(28.))
                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                            .text_color(rgb(0xf3f4f6))
                                            .child("Connect Codex"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .line_height(px(21.))
                                            .text_color(rgb(0x9299a8))
                                            .child(
                                                "Rode uses your installed Codex CLI and OpenAI account. Authentication stays managed by Codex.",
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(rgb(0x3b82f6))
                                    .bg(rgb(0x161a22))
                                    .p_4()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .size(px(42.))
                                                    .rounded_lg()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .bg(rgb(0xf3f4f6))
                                                    .font_family("monospace")
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .text_color(rgb(0x111318))
                                                    .child(">_"),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_base()
                                                            .font_weight(
                                                                gpui::FontWeight::SEMIBOLD,
                                                            )
                                                            .text_color(rgb(0xf3f4f6))
                                                            .child("Codex"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(0x8f96a5))
                                                            .child("OpenAI coding agent"),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .px_3()
                                            .py_1()
                                            .rounded_full()
                                            .bg(rgb(0x202a3d))
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .size(px(7.))
                                                    .rounded_full()
                                                    .bg(rgb(status_color)),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(0xc5cada))
                                                    .child(status_label),
                                            ),
                                    ),
                            )
                            .when(!self.codex_available(), |content| {
                                content.child(
                                    div()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(0x513238))
                                        .bg(rgb(0x24191c))
                                        .p_4()
                                        .text_sm()
                                        .line_height(px(20.))
                                        .text_color(rgb(0xfca5a5))
                                        .child(
                                            "Install the Codex CLI and make sure `codex` is available on PATH, then restart Rode.",
                                        ),
                                )
                            })
                            .when_some(error, |content, error| {
                                content.child(
                                    div()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(0x513238))
                                        .bg(rgb(0x24191c))
                                        .p_4()
                                        .text_sm()
                                        .text_color(rgb(0xfca5a5))
                                        .child(error),
                                )
                            })
                            .child(match &self.codex_auth {
                                CodexAuthState::SignedOut => div()
                                    .id("onboarding-sign-in")
                                    .role(Role::Button)
                                    .aria_label("Sign in to OpenAI with Codex")
                                    .w_full()
                                    .h(px(44.))
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(0x2563eb))
                                    .hover(|style| style.bg(rgb(0x3b82f6)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .text_color(rgb(0xffffff))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sign_in_codex(cx)
                                    }))
                                    .child("Sign in with ChatGPT")
                                    .into_any_element(),
                                CodexAuthState::Error(_) => div()
                                    .id("onboarding-auth-retry")
                                    .role(Role::Button)
                                    .aria_label("Retry the Codex account check")
                                    .w_full()
                                    .h(px(44.))
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(0x343946))
                                    .hover(|style| style.bg(rgb(0x444b5a)))
                                    .text_sm()
                                    .text_color(rgb(0xf3f4f6))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_codex_account(cx)
                                    }))
                                    .child("Try again")
                                    .into_any_element(),
                                CodexAuthState::Checking => onboarding_status("Checking account…"),
                                CodexAuthState::SigningIn => {
                                    onboarding_status("Finish signing in in your browser…")
                                }
                                CodexAuthState::Unavailable => {
                                    onboarding_status("Codex CLI required")
                                }
                                CodexAuthState::SignedIn(_) => div().into_any_element(),
                            })
                            .child(
                                div()
                                    .text_center()
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(rgb(0x6f7685))
                                    .child(
                                        "Sign-in opens in your browser. Codex stores and refreshes the session.",
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let (codex_color, codex_label) = match &self.codex_auth {
            CodexAuthState::Unavailable => (0x6b7280, "Codex · missing".to_owned()),
            CodexAuthState::Checking => (0xf59e0b, "Codex · checking account".to_owned()),
            CodexAuthState::SignedOut => (0xf59e0b, "Codex · sign in required".to_owned()),
            CodexAuthState::SignedIn(account) => {
                (0x34d399, format!("Codex · {}", account.summary()))
            }
            CodexAuthState::SigningIn => (0x60a5fa, "Codex · waiting for browser".to_owned()),
            CodexAuthState::Error(_) => (0xf87171, "Codex · authentication error".to_owned()),
        };
        let codex_error = match &self.codex_auth {
            CodexAuthState::Error(error) => Some(error.clone()),
            _ => None,
        };
        let codex_status = div()
            .rounded_md()
            .p_2()
            .bg(rgb(0x191c22))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(7.)).rounded_full().bg(rgb(codex_color)))
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(rgb(0xb9bec9))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(codex_label),
                    ),
            )
            .when_some(codex_error, |status, error| {
                status.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xfca5a5))
                        .line_height(px(16.))
                        .child(error),
                )
            })
            .when(
                matches!(self.codex_auth, CodexAuthState::SignedOut),
                |status| {
                    status.child(
                        div()
                            .id("codex-sign-in")
                            .role(Role::Button)
                            .aria_label("Sign in to OpenAI with ChatGPT")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgb(0x2563eb))
                            .hover(|style| style.bg(rgb(0x3b82f6)))
                            .text_xs()
                            .text_color(rgb(0xffffff))
                            .on_click(cx.listener(|this, _, _, cx| this.sign_in_codex(cx)))
                            .child("Sign in with ChatGPT"),
                    )
                },
            )
            .when(
                matches!(self.codex_auth, CodexAuthState::Error(_)),
                |status| {
                    status.child(
                        div()
                            .id("codex-auth-retry")
                            .role(Role::Button)
                            .aria_label("Retry Codex account check")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgb(0x343946))
                            .hover(|style| style.bg(rgb(0x444b5a)))
                            .text_xs()
                            .text_color(rgb(0xf3f4f6))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_codex_account(cx)))
                            .child("Retry account check"),
                    )
                },
            );

        div()
            .w(px(252.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(0x111318))
            .border_r_1()
            .border_color(rgb(0x292c33))
            .child(
                div()
                    .h(px(58.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(0x292c33))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xf3f4f6))
                            .child("RODE"),
                    )
                    .child(
                        div()
                            .id("create-menu-toggle")
                            .role(Role::Button)
                            .aria_label("Create thread or add project folder")
                            .size(px(28.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(rgb(0x242831))
                            .hover(|style| style.bg(rgb(0x343946)))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_create_menu(cx)))
                            .child("+"),
                    ),
            )
            .when(self.show_create_menu, |sidebar| {
                sidebar.child(
                    div()
                        .mx_3()
                        .mt_2()
                        .p_1()
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x343946))
                        .bg(rgb(0x20232a))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(selectable_row::selectable_row(
                            "create-thread",
                            "New thread",
                            false,
                            false,
                            cx.listener(|this, _, _, cx| this.new_thread(cx)),
                        ))
                        .child(selectable_row::selectable_row(
                            "add-project-folder",
                            "Add project folder…",
                            false,
                            false,
                            cx.listener(|this, _, window, cx| this.open_folder_picker(window, cx)),
                        )),
                )
            })
            .child(
                div()
                    .id("project-list")
                    .p_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x777d8b)).child("PROJECTS"))
                    .children(
                        self.known_projects
                            .iter()
                            .enumerate()
                            .map(|(index, project)| self.render_project_group(index, project, cx)),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(0x292c33))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .id("settings-toggle")
                            .role(Role::Button)
                            .aria_label("Open Rode settings")
                            .px_2()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(0xb9bec9))
                            .hover(|style| style.bg(rgb(0x292d36)))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_settings(cx)))
                            .child(if self.show_settings {
                                "Settings ▴"
                            } else {
                                "Settings ▾"
                            }),
                    )
                    .when(self.show_settings, |footer| {
                        footer.child(
                            div()
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(0x343946))
                                .bg(rgb(0x191c22))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(0xd1d5db))
                                        .child("New thread workspace"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(px(16.))
                                        .text_color(rgb(0x7f8796))
                                        .child(if self.isolate_new_threads {
                                            "Each new thread gets an isolated Git worktree."
                                        } else {
                                            "New threads use the selected project folder."
                                        }),
                                )
                                .child(
                                    div()
                                        .id("isolate-new-threads")
                                        .role(Role::Button)
                                        .aria_label("Toggle isolated worktrees for new threads")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .bg(if self.isolate_new_threads {
                                            rgb(0x2563eb)
                                        } else {
                                            rgb(0x303540)
                                        })
                                        .text_xs()
                                        .text_color(rgb(0xf3f4f6))
                                        .hover(|style| style.bg(rgb(0x3b82f6)))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.toggle_thread_isolation(cx)
                                        }))
                                        .child(if self.isolate_new_threads {
                                            "Isolated worktree: On"
                                        } else {
                                            "Isolated worktree: Off"
                                        }),
                                ),
                        )
                    })
                    .child(codex_status),
            )
    }

    fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let branch = if self.repo.is_repository {
            self.repo.branch.clone()
        } else {
            "not a Git repository".to_owned()
        };
        div()
            .h(px(58.))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .bg(rgb(0x17191f))
            .border_b_1()
            .border_color(rgb(0x292c33))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xe5e7eb))
                            .child(self.thread_title.clone()),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(0x22252d))
                            .text_xs()
                            .text_color(rgb(0x9ca3af))
                            .child(format!("⎇ {branch}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(button::button(
                        "toggle-terminal",
                        "Terminal",
                        if self.show_terminal {
                            button::ButtonStyle::Primary
                        } else {
                            button::ButtonStyle::Secondary
                        },
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_terminal(&ToggleTerminal, window, cx)
                        }),
                    ))
                    .child(
                        div()
                            .id("refresh-repo")
                            .role(Role::Button)
                            .aria_label("Refresh repository")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(rgb(0xb9bec9))
                            .hover(|style| style.bg(rgb(0x292d36)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.refresh_repo(&RefreshRepo, window, cx)
                            }))
                            .child("Refresh"),
                    )
                    .child(button::button(
                        "toggle-diff",
                        format!("Diff · {}", self.repo.changed_files),
                        if self.show_diff {
                            button::ButtonStyle::Primary
                        } else {
                            button::ButtonStyle::Secondary
                        },
                        false,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_diff(&ToggleDiff, window, cx)
                        }),
                    ))
                    .when(self.running, |actions| {
                        actions.child(button::button(
                            "cancel-turn",
                            "Cancel",
                            button::ButtonStyle::Destructive,
                            false,
                            cx.listener(|this, _, _, cx| this.cancel_turn(cx)),
                        ))
                    }),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut messages = div()
            .id("messages")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_5()
            .flex()
            .flex_col()
            .gap_4();

        for (index, message) in self.messages.iter().enumerate() {
            let (label, background, border, text) = match message.role {
                MessageRole::User => ("YOU", 0x17233a, 0x284b7a, 0xe8eef9),
                MessageRole::Agent => ("CODEX", 0x1a1d24, 0x323641, 0xd8dbe2),
                MessageRole::Tool => ("TOOL", 0x181b20, 0x3b3f48, 0xc4c9d3),
                MessageRole::System => ("RODE", 0x171b20, 0x294137, 0xa7c7b8),
            };
            messages = messages.child(
                div()
                    .id(("message", index))
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(border))
                    .bg(rgb(background))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0x7f8796))
                            .child(label),
                    )
                    .child(
                        div()
                            .w_full()
                            .whitespace_normal()
                            .line_height(px(21.))
                            .text_sm()
                            .text_color(rgb(text))
                            .child(message.text.clone()),
                    ),
            );
        }

        for (index, request) in self.approvals.iter().enumerate() {
            let kind = match request.kind {
                codex::ApprovalKind::Command => "COMMAND APPROVAL",
                codex::ApprovalKind::FileChange => "FILE-CHANGE APPROVAL",
            };
            messages = messages.child(
                div()
                    .id(("approval", index))
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x92400e))
                    .bg(rgb(0x261d13))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xfbbf24))
                            .child(kind),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_sm()
                            .whitespace_normal()
                            .text_color(rgb(0xfef3c7))
                            .child(request.title.clone()),
                    )
                    .when(!request.detail.is_empty(), |card| {
                        card.child(
                            div()
                                .text_xs()
                                .whitespace_normal()
                                .text_color(rgb(0xd6b98b))
                                .child(request.detail.clone()),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(0x8b7355))
                            .child(format!("item {}", request.item_id)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id(("approval-accept", index))
                                    .role(Role::Button)
                                    .aria_label("Approve request")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(rgb(0x166534))
                                    .text_xs()
                                    .text_color(rgb(0xdcfce7))
                                    .hover(|style| style.bg(rgb(0x15803d)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.respond_to_approval(index, "accept", cx)
                                    }))
                                    .child("Approve once"),
                            )
                            .child(
                                div()
                                    .id(("approval-decline", index))
                                    .role(Role::Button)
                                    .aria_label("Decline request")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .bg(rgb(0x3f2424))
                                    .text_xs()
                                    .text_color(rgb(0xfecaca))
                                    .hover(|style| style.bg(rgb(0x5f2d2d)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.respond_to_approval(index, "decline", cx)
                                    }))
                                    .child("Decline"),
                            ),
                    ),
            );
        }
        if self.running {
            let activity = if self.reasoning_preview.trim().is_empty() {
                "Codex is working…".to_owned()
            } else {
                format!("Reasoning… {}", self.reasoning_preview.trim())
            };
            messages = messages.child(
                div()
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x323641))
                    .bg(rgb(0x1a1d24))
                    .p_4()
                    .text_sm()
                    .text_color(rgb(0x9ca3af))
                    .line_clamp(3)
                    .child(activity),
            );
        }
        messages
    }

    fn render_composer(&self, cx: &App) -> Div {
        let focus_handle = self.composer.read(cx).focus_handle.clone();
        div()
            .p_4()
            .flex_none()
            .border_t_1()
            .border_color(rgb(0x292c33))
            .bg(rgb(0x17191f))
            .child(
                div()
                    .id("composer")
                    .key_context("Composer")
                    .track_focus(&focus_handle)
                    .map(standard_actions(self.composer.clone()))
                    .cursor(CursorStyle::IBeam)
                    .w_full()
                    .h(px(112.))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x3a3f4b))
                    .bg(rgb(0x20232a))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .line_height(px(20.))
                    .text_size(px(14.))
                    .text_color(rgb(0xe5e7eb))
                    .child(
                        self.composer
                            .clone()
                            .cached(StyleRefinement::default().w_full().h(px(72.))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_xs()
                            .text_color(rgb(0x767d8d))
                            .child("Shift+Enter for a new line")
                            .child(if self.creating_worktree {
                                "Creating worktree"
                            } else if self.running {
                                "Turn running"
                            } else {
                                "Enter to send"
                            }),
                    ),
            )
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> Div {
        let terminal = self.terminal_sessions.get(&self.thread_id).cloned();
        let (title, exited) = terminal.as_ref().map_or_else(
            || ("Preparing terminal…".to_owned(), false),
            |terminal| {
                let terminal = terminal.read(cx);
                (terminal.title().to_owned(), terminal.exited())
            },
        );
        div()
            .h(px(300.))
            .min_h(px(180.))
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(0x292c33))
            .bg(rgb(0x0f1115))
            .child(
                div()
                    .h(px(34.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(0x17191f))
                    .border_b_1()
                    .border_color(rgb(0x292c33))
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(if exited { rgb(0xf87171) } else { rgb(0xb9bec9) })
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(if exited {
                                format!("{title} · exited")
                            } else {
                                title
                            }),
                    )
                    .child(
                        div()
                            .id("close-terminal")
                            .role(Role::Button)
                            .aria_label("Close terminal panel")
                            .px_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_sm()
                            .text_color(rgb(0x8b93a3))
                            .hover(|style| style.bg(rgb(0x292d36)).text_color(rgb(0xf3f4f6)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_terminal(&ToggleTerminal, window, cx)
                            }))
                            .child("×"),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .p_2()
                    .when_some(terminal, |panel, terminal| {
                        panel
                            .child(terminal.cached(StyleRefinement::default().w_full().h(px(250.))))
                    }),
            )
    }

    fn render_publish_controls(&self, cx: &mut Context<Self>) -> Div {
        let focus = self.commit_editor.read(cx).focus_handle.clone();
        let busy = self.git_operation.is_some();
        div()
            .flex_none()
            .p_3()
            .border_b_1()
            .border_color(rgb(0x292f38))
            .bg(rgb(0x11161d))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .key_context("CommitMessage")
                    .track_focus(&focus)
                    .map(standard_actions(self.commit_editor.clone()))
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x303742))
                    .bg(rgb(0x171c24))
                    .text_sm()
                    .text_color(rgb(0xe6edf3))
                    .child(
                        self.commit_editor
                            .clone()
                            .cached(StyleRefinement::default().w_full()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(publish_button(
                        "commit-all",
                        "Commit all",
                        busy,
                        cx.listener(|this, _, _, cx| this.commit_changes(cx)),
                    ))
                    .child(publish_button(
                        "push-branch",
                        "Push",
                        busy,
                        cx.listener(|this, _, _, cx| this.push_changes(cx)),
                    ))
                    .child(publish_button(
                        "create-pr",
                        "Create PR",
                        busy,
                        cx.listener(|this, _, _, cx| this.create_pr(cx)),
                    ))
                    .when_some(self.git_operation, |row, operation| {
                        row.child(
                            div()
                                .ml_auto()
                                .text_xs()
                                .text_color(rgb(0x93c5fd))
                                .child(format!("{operation}…")),
                        )
                    }),
            )
            .when_some(self.publish_status.clone(), |panel, status| {
                panel.child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(if status.starts_with("Git workflow failed") {
                            rgb(0xfca5a5)
                        } else {
                            rgb(0x86efac)
                        })
                        .child(status),
                )
            })
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> Div {
        let document = DiffDocument::parse(&self.repo.diff);
        let mut files = div()
            .id("diff-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .overflow_x_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_3();

        if document.files.is_empty() {
            files = files.child(
                div()
                    .w_full()
                    .p_5()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(0x292f38))
                    .bg(rgb(0x141820))
                    .text_sm()
                    .text_color(rgb(0x88919f))
                    .child("No uncommitted diff"),
            );
        } else {
            for (file_index, file) in document.files.iter().enumerate() {
                files = files.child(self.render_diff_file(file_index, file));
            }
        }

        div()
            .w(px(720.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(0x0d1117))
            .border_l_1()
            .border_color(rgb(0x292f38))
            .child(
                div()
                    .h(px(58.))
                    .px_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(0x292f38))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xe6edf3))
                                    .child("Working tree"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(0x202630))
                                    .text_xs()
                                    .text_color(rgb(0x9da7b5))
                                    .child(format!("{} files", self.repo.changed_files)),
                            ),
                    )
                    .child(
                        tabs::tab_list()
                            .p_1()
                            .rounded_md()
                            .bg(rgb(0x171c24))
                            .border_1()
                            .border_color(rgb(0x303742))
                            .flex()
                            .items_center()
                            .child(self.render_diff_mode_button(
                                "diff-mode-stack",
                                "Stack",
                                DiffViewMode::Stack,
                                cx,
                            ))
                            .child(self.render_diff_mode_button(
                                "diff-mode-split",
                                "Split",
                                DiffViewMode::Split,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .flex_none()
                    .border_b_1()
                    .border_color(rgb(0x292f38))
                    .text_xs()
                    .whitespace_normal()
                    .text_color(rgb(0x8b949e))
                    .child(self.repo.diff_stat.clone()),
            )
            .child(self.render_publish_controls(cx))
            .child(files)
    }

    fn render_diff_mode_button(
        &self,
        id: &'static str,
        label: &'static str,
        mode: DiffViewMode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.diff_view == mode;
        tabs::tab(
            id,
            label,
            selected,
            cx.listener(move |this, _, _, cx| {
                this.diff_view = mode;
                cx.notify();
            }),
        )
    }

    fn render_diff_file(&self, file_index: usize, file: &DiffFile) -> impl IntoElement {
        let mut body = div().w_full().flex().flex_col();
        let mut previous_old_end = 1;

        if self.diff_view == DiffViewMode::Split && !file.hunks.is_empty() {
            body = body.child(
                div()
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .bg(rgb(0x151a21))
                    .border_b_1()
                    .border_color(rgb(0x2a3039))
                    .text_xs()
                    .text_color(rgb(0x7d8794))
                    .child(
                        div()
                            .w_1_2()
                            .px_3()
                            .child(format!("Original · {}", file.old_path)),
                    )
                    .child(
                        div()
                            .w_1_2()
                            .px_3()
                            .border_l_1()
                            .border_color(rgb(0x343b46))
                            .child(format!("Modified · {}", file.new_path)),
                    ),
            );
        }

        for hunk in &file.hunks {
            let hidden = hunk.old_start.saturating_sub(previous_old_end);
            if hidden > 0 {
                body = body.child(render_unchanged_band(hidden));
            }
            body = body.child(match self.diff_view {
                DiffViewMode::Stack => render_stack_hunk(hunk),
                DiffViewMode::Split => render_split_hunk(hunk),
            });
            previous_old_end = hunk.old_start.saturating_add(hunk.old_count);
        }

        if file.hunks.is_empty() {
            body = body.child(
                div()
                    .p_4()
                    .bg(rgb(0x11161d))
                    .text_xs()
                    .text_color(rgb(0x8b949e))
                    .child(if file.binary {
                        "Binary file changed"
                    } else {
                        "No textual hunks"
                    }),
            );
        }

        div()
            .id(("diff-file", file_index))
            .min_w(px(680.))
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(0x303742))
            .bg(rgb(0x0f141b))
            .child(
                div()
                    .h(px(42.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(0x1a2029))
                    .border_b_1()
                    .border_color(rgb(0x303742))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().text_color(rgb(0x6e7681)).text_xs().child("▾"))
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_sm()
                                    .text_color(rgb(0xd6dde5))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(file.display_path().to_owned()),
                            )
                            .when_some(file.status.clone(), |header, status| {
                                header.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(0x272e38))
                                        .text_xs()
                                        .text_color(rgb(0xaab4c0))
                                        .child(status),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family("monospace")
                            .text_xs()
                            .child(
                                div()
                                    .text_color(rgb(0x3fb950))
                                    .child(format!("+{}", file.additions)),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0xf85149))
                                    .child(format!("−{}", file.deletions)),
                            ),
                    ),
            )
            .child(body)
    }
}

fn publish_button(
    id: &'static str,
    label: &'static str,
    busy: bool,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    button::button(id, label, button::ButtonStyle::Primary, busy, listener)
}

fn render_unchanged_band(count: u32) -> Div {
    div()
        .h(px(25.))
        .px_2()
        .flex()
        .items_center()
        .bg(rgb(0x242a33))
        .border_b_1()
        .border_color(rgb(0x303742))
        .font_family("monospace")
        .text_xs()
        .text_color(rgb(0x8b949e))
        .child(format!("▾ {count} unchanged lines"))
}

fn render_hunk_header(header: &str) -> Div {
    div()
        .h(px(25.))
        .px_2()
        .flex()
        .items_center()
        .bg(rgb(0x242a33))
        .border_b_1()
        .border_color(rgb(0x303742))
        .font_family("monospace")
        .text_xs()
        .text_color(rgb(0x9aa4b2))
        .child(header.to_owned())
}

fn render_stack_hunk(hunk: &DiffHunk) -> Div {
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .child(render_hunk_header(&hunk.header));
    for line in &hunk.lines {
        element = element.child(render_stack_line(line));
    }
    element
}

fn render_stack_line(line: &DiffLine) -> Div {
    let (background, foreground, marker) = diff_line_style(line.kind);
    div()
        .min_w(px(680.))
        .h(px(21.))
        .flex()
        .items_center()
        .bg(rgb(background))
        .font_family("monospace")
        .text_xs()
        .line_height(px(21.))
        .text_color(rgb(foreground))
        .child(render_line_number(line.old_line))
        .child(render_line_number(line.new_line))
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_center()
                .text_color(rgb(foreground))
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr_3()
                .whitespace_nowrap()
                .child(line.text.clone()),
        )
}

fn render_split_hunk(hunk: &DiffHunk) -> Div {
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .child(render_hunk_header(&hunk.header));
    for row in split_rows(hunk) {
        element = element.child(
            div()
                .min_w(px(680.))
                .h(px(21.))
                .flex()
                .child(render_split_cell(row.left.as_ref(), true))
                .child(render_split_cell(row.right.as_ref(), false)),
        );
    }
    element
}

fn render_split_cell(line: Option<&DiffLine>, left: bool) -> Div {
    let (background, foreground, marker) = line
        .map(|line| diff_line_style(line.kind))
        .unwrap_or((0x11161d, 0x6e7681, ""));
    let number = line.and_then(|line| if left { line.old_line } else { line.new_line });
    let text = line.map(|line| line.text.clone()).unwrap_or_default();

    div()
        .w_1_2()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .bg(rgb(background))
        .when(!left, |cell| cell.border_l_1().border_color(rgb(0x343b46)))
        .font_family("monospace")
        .text_xs()
        .line_height(px(21.))
        .text_color(rgb(foreground))
        .child(render_line_number(number))
        .child(div().w(px(20.)).flex_none().text_center().child(marker))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr_2()
                .whitespace_nowrap()
                .child(text),
        )
}

fn render_line_number(number: Option<u32>) -> Div {
    div()
        .w(px(42.))
        .h_full()
        .flex_none()
        .pr_2()
        .flex()
        .items_center()
        .justify_end()
        .bg(rgb(0x161b22))
        .border_r_1()
        .border_color(rgb(0x2b313a))
        .text_color(rgb(0x6e7681))
        .child(number.map(|number| number.to_string()).unwrap_or_default())
}

fn diff_line_style(kind: DiffLineKind) -> (u32, u32, &'static str) {
    match kind {
        DiffLineKind::Context => (0x0f141b, 0xc9d1d9, " "),
        DiffLineKind::Addition => (0x123523, 0xc8e6d0, "+"),
        DiffLineKind::Deletion => (0x431d22, 0xf0c5ca, "−"),
        DiffLineKind::Meta => (0x1a2029, 0x8b949e, ""),
    }
}

impl Render for RodeApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = &theme::current().colors;
        if self.codex_auth.requires_onboarding() {
            return self.render_auth_onboarding(cx);
        }
        if self.show_terminal {
            self.ensure_terminal(window, cx);
        }
        div()
            .id("rode-root")
            .key_context(self.route.key_context())
            .on_action(cx.listener(Self::send_prompt))
            .on_action(cx.listener(Self::submit_rename))
            .on_action(cx.listener(Self::cancel_rename))
            .on_action(cx.listener(Self::dismiss_modal))
            .on_action(cx.listener(Self::toggle_terminal))
            .on_action(cx.listener(Self::toggle_diff))
            .on_action(cx.listener(Self::toggle_diff_layout))
            .on_action(cx.listener(Self::refresh_repo))
            .size_full()
            .min_w(px(900.))
            .relative()
            .flex()
            .bg(rgb(colors.root))
            .text_color(rgb(colors.text))
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(cx))
                    .child(self.render_messages(cx))
                    .when(self.show_terminal, |column| {
                        column.child(self.render_terminal(cx))
                    })
                    .child(self.render_composer(cx)),
            )
            .when(self.show_diff, |root| root.child(self.render_diff(cx)))
            .when_some(self.modal, |root, active_modal| {
                root.child(modal::modal_frame(
                    active_modal.title(),
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted_text))
                        .child("This dialog is ready for its feature-specific content."),
                ))
            })
            .child(
                div()
                    .absolute()
                    .top_4()
                    .right_4()
                    .w(px(360.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(self.toasts.iter().map(toast::toast)),
            )
            .into_any_element()
    }
}

fn onboarding_status(label: &'static str) -> gpui::AnyElement {
    div()
        .w_full()
        .h(px(44.))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x242831))
        .text_sm()
        .text_color(rgb(0x8f96a5))
        .child(label)
        .into_any_element()
}

fn new_local_thread_id() -> String {
    format!(
        "thread-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default()
    )
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Project")
        .to_owned()
}

fn route_after_auth(current: AppRoute, requires_onboarding: bool) -> AppRoute {
    if requires_onboarding {
        AppRoute::Login
    } else if current == AppRoute::Login {
        AppRoute::Workspace
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::{AppRoute, CodexAccount, CodexAuthState, SettingsSection, route_after_auth};

    #[test]
    fn workspace_is_only_available_after_codex_authentication() {
        assert!(CodexAuthState::Unavailable.requires_onboarding());
        assert!(CodexAuthState::Checking.requires_onboarding());
        assert!(CodexAuthState::SignedOut.requires_onboarding());
        assert!(CodexAuthState::SigningIn.requires_onboarding());
        assert!(CodexAuthState::Error("failed".to_owned()).requires_onboarding());
        assert!(
            !CodexAuthState::SignedIn(CodexAccount::ChatGpt {
                email: None,
                plan: "plus".to_owned(),
            })
            .requires_onboarding()
        );
    }

    #[test]
    fn application_route_defaults_to_workspace() {
        assert_eq!(AppRoute::default(), AppRoute::Workspace);
        assert_ne!(
            AppRoute::Settings(SettingsSection::Appearance),
            AppRoute::Workspace
        );
    }

    #[test]
    fn authentication_routes_to_onboarding_without_clobbering_signed_in_navigation() {
        assert_eq!(
            route_after_auth(AppRoute::SourceControl, true),
            AppRoute::Login
        );
        assert_eq!(
            route_after_auth(AppRoute::Login, false),
            AppRoute::Workspace
        );
        assert_eq!(
            route_after_auth(AppRoute::Terminal, false),
            AppRoute::Terminal
        );
    }
}
