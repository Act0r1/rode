#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

mod agent;
mod codex;
mod codex_auth;
mod editor;
mod git;

use std::path::{Path, PathBuf};

use agent::{ProviderKind, ProviderStatus, discover_providers};
use codex::{ApprovalRequest, CodexEvent, CodexSession};
use codex_auth::{CodexAccount, begin_codex_login, read_codex_account};
use editor::{Editor, standard_actions};
use git::RepoSnapshot;
use gpui::{
    App, Bounds, Context, CursorStyle, Div, Entity, IntoElement, KeyBinding, PathPromptOptions,
    Render, Role, StyleRefinement, TitlebarOptions, Window, WindowBounds, WindowOptions, actions,
    div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

actions!(
    rode,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Home,
        End,
        InsertNewline,
        SendPrompt,
        ToggleDiff,
        RefreshRepo,
        Quit
    ]
);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageRole {
    User,
    Agent,
    Tool,
    System,
}

#[derive(Clone, Debug)]
struct Message {
    role: MessageRole,
    text: String,
}

#[derive(Clone, Debug)]
struct Chat {
    number: usize,
    thread_id: Option<String>,
    messages: Vec<Message>,
}

impl Chat {
    fn new(number: usize) -> Self {
        Self {
            number,
            thread_id: None,
            messages: vec![Message {
                role: MessageRole::System,
                text: "New local chat. The first prompt will open a new Codex app-server session."
                    .to_owned(),
            }],
        }
    }
}

#[derive(Clone, Debug)]
struct ChatFolder {
    name: String,
    path: PathBuf,
    chats: Vec<Chat>,
}

impl ChatFolder {
    fn new(path: PathBuf) -> Self {
        Self {
            name: folder_name(&path),
            path,
            chats: vec![Chat::new(1)],
        }
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Folder")
        .to_owned()
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

struct RodeApp {
    project_path: PathBuf,
    project_name: String,
    repo: RepoSnapshot,
    providers: Vec<ProviderStatus>,
    codex_auth: CodexAuthState,
    messages: Vec<Message>,
    composer: Entity<Editor>,
    codex_session: Option<CodexSession>,
    codex_thread_id: Option<String>,
    active_turn_id: Option<String>,
    active_agent_message: Option<usize>,
    reasoning_preview: String,
    approvals: Vec<ApprovalRequest>,
    session_generation: u64,
    running: bool,
    show_diff: bool,
    chat_number: usize,
    chat_folders: Vec<ChatFolder>,
    active_folder: usize,
    active_chat: usize,
}

impl RodeApp {
    fn new(project_path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let repo = RepoSnapshot::load(&project_path);
        let project_name = repo
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned();
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

        let providers = discover_providers();
        let codex_auth = if providers
            .iter()
            .any(|provider| provider.kind == ProviderKind::Codex && provider.available)
        {
            CodexAuthState::Checking
        } else {
            CodexAuthState::Unavailable
        };

        let initial_messages = vec![Message {
            role: MessageRole::System,
            text: "Rode is using the native Wayland renderer. Codex turns run in the workspace-write sandbox by default.".to_owned(),
        }];
        let mut initial_folder = ChatFolder::new(repo.root.clone());
        initial_folder.chats[0].messages = initial_messages.clone();

        Self {
            project_path: repo.root.clone(),
            project_name,
            repo,
            providers,
            codex_auth,
            messages: initial_messages,
            composer,
            codex_session: None,
            codex_thread_id: None,
            active_turn_id: None,
            active_agent_message: None,
            reasoning_preview: String::new(),
            approvals: Vec::new(),
            session_generation: 0,
            running: false,
            show_diff: true,
            chat_number: 1,
            chat_folders: vec![initial_folder],
            active_folder: 0,
            active_chat: 0,
        }
    }

    fn codex_available(&self) -> bool {
        self.providers
            .iter()
            .any(|provider| provider.kind == ProviderKind::Codex && provider.available)
    }

    fn codex_authenticated(&self) -> bool {
        matches!(self.codex_auth, CodexAuthState::SignedIn(_))
    }

    fn refresh_codex_account(&mut self, cx: &mut Context<Self>) {
        if !self.codex_available() {
            self.codex_auth = CodexAuthState::Unavailable;
            cx.notify();
            return;
        }

        self.codex_auth = CodexAuthState::Checking;
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
        self.messages.push(Message {
            role: MessageRole::System,
            text: "Starting a secure ChatGPT sign-in through Codex…".to_owned(),
        });
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
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Could not start Codex login: {detail}"),
                        });
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
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: "Signed in to OpenAI through Codex. You can now start a thread."
                                .to_owned(),
                        });
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        this.codex_auth = CodexAuthState::Error(detail.clone());
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: format!("Codex login did not complete: {detail}"),
                        });
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_prompt(&mut self, _: &SendPrompt, _: &mut Window, cx: &mut Context<Self>) {
        if self.running {
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
                if let Some(error) = error {
                    self.messages.push(Message {
                        role: MessageRole::System,
                        text: format!("Codex turn {status}: {error}"),
                    });
                }
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

    fn save_active_chat(&mut self) {
        let Some(folder) = self.chat_folders.get_mut(self.active_folder) else {
            return;
        };
        let Some(chat) = folder.chats.get_mut(self.active_chat) else {
            return;
        };
        chat.thread_id = self.codex_thread_id.clone();
        chat.messages = self.messages.clone();
    }

    fn reset_chat_runtime(&mut self) {
        self.session_generation += 1;
        self.codex_session = None;
        self.active_turn_id = None;
        self.active_agent_message = None;
        self.reasoning_preview.clear();
        self.approvals.clear();
        self.running = false;
    }

    fn switch_chat(&mut self, folder_index: usize, chat_index: usize, cx: &mut Context<Self>) {
        if folder_index == self.active_folder && chat_index == self.active_chat {
            return;
        }
        let Some(folder) = self.chat_folders.get(folder_index) else {
            return;
        };
        let Some(chat) = folder.chats.get(chat_index) else {
            return;
        };
        let project_path = folder.path.clone();
        let project_name = folder.name.clone();
        let chat_number = chat.number;
        let thread_id = chat.thread_id.clone();
        let messages = chat.messages.clone();

        self.save_active_chat();
        self.reset_chat_runtime();
        self.active_folder = folder_index;
        self.active_chat = chat_index;
        self.project_path = project_path;
        self.project_name = project_name;
        self.repo = RepoSnapshot::load(&self.project_path);
        self.chat_number = chat_number;
        self.codex_thread_id = thread_id;
        self.messages = messages;
        self.composer.update(cx, |editor, cx| editor.clear(cx));
        cx.notify();
    }

    fn new_chat(&mut self, folder_index: usize, cx: &mut Context<Self>) {
        if folder_index >= self.chat_folders.len() {
            return;
        }
        self.save_active_chat();
        let chat_index = self.chat_folders[folder_index].chats.len();
        let chat_number = chat_index + 1;
        self.chat_folders[folder_index]
            .chats
            .push(Chat::new(chat_number));

        if folder_index == self.active_folder && chat_index == self.active_chat {
            return;
        }
        self.switch_chat(folder_index, chat_index, cx);
    }

    fn add_chat_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let path = path.canonicalize().unwrap_or(path);
        if let Some(folder_index) = self
            .chat_folders
            .iter()
            .position(|folder| folder.path == path)
        {
            let chat_index = self.chat_folders[folder_index]
                .chats
                .len()
                .saturating_sub(1);
            self.switch_chat(folder_index, chat_index, cx);
            return;
        }

        self.save_active_chat();
        self.chat_folders.push(ChatFolder::new(path));
        let folder_index = self.chat_folders.len() - 1;
        self.switch_chat(folder_index, 0, cx);
    }

    fn open_folder_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add folder".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match selection.await {
                Ok(result) => result,
                Err(_) => return,
            };
            this.update_in(cx, |this, _window, cx| match result {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.add_chat_folder(path, cx);
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

    fn toggle_diff(&mut self, _: &ToggleDiff, _: &mut Window, cx: &mut Context<Self>) {
        self.show_diff = !self.show_diff;
        cx.notify();
    }

    fn refresh_repo(&mut self, _: &RefreshRepo, _: &mut Window, cx: &mut Context<Self>) {
        self.repo = RepoSnapshot::load(&self.project_path);
        cx.notify();
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
        let claude_provider = self
            .providers
            .iter()
            .find(|provider| provider.kind == ProviderKind::Claude);
        let claude_available = claude_provider.is_some_and(|provider| provider.available);
        let claude_path = claude_provider
            .and_then(|provider| provider.path.as_ref())
            .map(|path| path.display().to_string());
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
        let folder_list = self.chat_folders.iter().enumerate().fold(
            div().flex().flex_col().gap_2(),
            |folder_list, (folder_index, folder)| {
                let chats = folder.chats.iter().enumerate().fold(
                    div().pl_3().flex().flex_col().gap_1(),
                    |chats, (chat_index, chat)| {
                        let is_active =
                            folder_index == self.active_folder && chat_index == self.active_chat;
                        let session_label = if is_active {
                            self.codex_thread_id.as_deref()
                        } else {
                            chat.thread_id.as_deref()
                        }
                        .map(|id| id.chars().take(8).collect::<String>())
                        .unwrap_or_else(|| "not started".to_owned());
                        chats.child(
                            div()
                                .id(format!("folder-{folder_index}-chat-{chat_index}"))
                                .role(Role::Button)
                                .aria_label(format!("Open Chat {} in {}", chat.number, folder.name))
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if is_active {
                                    rgb(0x272b35)
                                } else {
                                    rgb(0x17191f)
                                })
                                .border_1()
                                .border_color(if is_active {
                                    rgb(0x3b82f6)
                                } else {
                                    rgb(0x242831)
                                })
                                .hover(|style| style.bg(rgb(0x242831)))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.switch_chat(folder_index, chat_index, cx)
                                }))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0xf3f4f6))
                                        .child(format!("Chat {}", chat.number)),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0x777d8b))
                                        .child(format!("session {session_label}")),
                                ),
                        )
                    },
                );

                folder_list.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .px_1()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgb(0xe5e7eb))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(folder.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(0x666d7a))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(folder.path.display().to_string()),
                                        ),
                                )
                                .child(
                                    div()
                                        .id(format!("new-chat-{folder_index}"))
                                        .role(Role::Button)
                                        .aria_label(format!("New chat in {}", folder.name))
                                        .flex_none()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_xs()
                                        .text_color(rgb(0x9ca3af))
                                        .hover(|style| style.bg(rgb(0x2b303a)))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.new_chat(folder_index, cx)
                                        }))
                                        .child("New chat"),
                                ),
                        )
                        .child(chats),
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
                            .id("new-folder")
                            .role(Role::Button)
                            .aria_label("Add a chat folder")
                            .size(px(28.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(rgb(0x242831))
                            .hover(|style| style.bg(rgb(0x343946)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_folder_picker(window, cx)
                            }))
                            .child("+"),
                    ),
            )
            .child(
                div()
                    .id("folder-list-scroll")
                    .p_3()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x777d8b)).child("FOLDERS"))
                    .child(folder_list),
            )
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(0x292c33))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(codex_status)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().size(px(7.)).rounded_full().bg(
                                        if claude_available {
                                            rgb(0x34d399)
                                        } else {
                                            rgb(0x6b7280)
                                        },
                                    ))
                                    .child(div().text_xs().text_color(rgb(0xb9bec9)).child(
                                        format!(
                                            "{} · {}",
                                            ProviderKind::Claude.label(),
                                            if claude_available { "ready" } else { "missing" }
                                        ),
                                    )),
                            )
                            .when_some(claude_path, |status, path| {
                                status.child(
                                    div()
                                        .pl_4()
                                        .text_xs()
                                        .text_color(rgb(0x6b7280))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(path),
                                )
                            }),
                    ),
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
                            .child(format!("{} · Chat {}", self.project_name, self.chat_number)),
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
                    .child(
                        div()
                            .id("toggle-diff")
                            .role(Role::Button)
                            .aria_label("Toggle diff panel")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_xs()
                            .bg(if self.show_diff {
                                rgb(0x2563eb)
                            } else {
                                rgb(0x292d36)
                            })
                            .text_color(rgb(0xf3f4f6))
                            .hover(|style| style.bg(rgb(0x3b82f6)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.toggle_diff(&ToggleDiff, window, cx)
                            }))
                            .child(format!("Diff · {}", self.repo.changed_files)),
                    )
                    .when(self.running, |actions| {
                        actions.child(
                            div()
                                .id("cancel-turn")
                                .role(Role::Button)
                                .aria_label("Cancel the running Codex turn")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .cursor_pointer()
                                .text_xs()
                                .bg(rgb(0x7f1d1d))
                                .text_color(rgb(0xfecaca))
                                .hover(|style| style.bg(rgb(0x991b1b)))
                                .on_click(cx.listener(|this, _, _, cx| this.cancel_turn(cx)))
                                .child("Cancel"),
                        )
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
                            .child(if self.running {
                                "Turn running"
                            } else {
                                "Enter to send"
                            }),
                    ),
            )
    }

    fn render_diff(&self) -> Div {
        let preview = self
            .repo
            .diff
            .lines()
            .take(180)
            .collect::<Vec<_>>()
            .join("\n");
        div()
            .w(px(380.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(0x121419))
            .border_l_1()
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
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xe5e7eb))
                            .child("Working tree"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x8b93a3))
                            .child(format!("{} files", self.repo.changed_files)),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .border_b_1()
                    .border_color(rgb(0x292c33))
                    .text_xs()
                    .whitespace_normal()
                    .text_color(rgb(0x9aa1af))
                    .child(self.repo.diff_stat.clone()),
            )
            .child(
                div()
                    .id("diff-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .font_family("monospace")
                    .text_xs()
                    .line_height(px(18.))
                    .whitespace_normal()
                    .text_color(rgb(0xbac0cb))
                    .child(preview),
            )
    }
}

impl Render for RodeApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("rode-root")
            .on_action(cx.listener(Self::send_prompt))
            .on_action(cx.listener(Self::toggle_diff))
            .on_action(cx.listener(Self::refresh_repo))
            .size_full()
            .min_w(px(900.))
            .flex()
            .bg(rgb(0x0f1115))
            .text_color(rgb(0xd1d5db))
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
                    .child(self.render_composer(cx)),
            )
            .when(self.show_diff, |root| root.child(self.render_diff()))
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let project_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("reading the current directory"));

    application().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, Some("Composer")),
            KeyBinding::new("delete", Delete, Some("Composer")),
            KeyBinding::new("left", Left, Some("Composer")),
            KeyBinding::new("right", Right, Some("Composer")),
            KeyBinding::new("home", Home, Some("Composer")),
            KeyBinding::new("end", End, Some("Composer")),
            KeyBinding::new("shift-enter", InsertNewline, Some("Composer")),
            KeyBinding::new("enter", SendPrompt, Some("Composer")),
            KeyBinding::new("ctrl-d", ToggleDiff, None),
            KeyBinding::new("ctrl-r", RefreshRepo, None),
            KeyBinding::new("ctrl-q", Quit, None),
        ]);

        let bounds = Bounds::centered(None, size(px(1380.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("dev.rode.Rode".to_owned()),
                titlebar: Some(TitlebarOptions {
                    title: Some("Rode".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                let project_path = project_path.clone();
                let app = cx.new(|cx| RodeApp::new(project_path, window, cx));
                app.update(cx, |app, cx| app.refresh_codex_account(cx));
                app
            },
        )
        .expect("opening the Rode window");
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.activate(true);
    });
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Rode currently targets Linux/Wayland only.");
}

#[cfg(test)]
mod tests {
    use super::{ChatFolder, MessageRole, folder_name};
    use std::path::Path;

    #[test]
    fn folder_label_comes_from_selected_directory() {
        assert_eq!(folder_name(Path::new("/work/client-app")), "client-app");
    }

    #[test]
    fn a_selected_folder_starts_with_chat_one() {
        let folder = ChatFolder::new("/work/client-app".into());

        assert_eq!(folder.name, "client-app");
        assert_eq!(folder.chats.len(), 1);
        assert_eq!(folder.chats[0].number, 1);
        assert_eq!(folder.chats[0].messages.len(), 1);
        assert_eq!(folder.chats[0].messages[0].role, MessageRole::System);
    }
}
