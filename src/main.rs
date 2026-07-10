#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

mod agent;
mod codex_auth;
mod editor;
mod git;

use std::path::PathBuf;

use agent::{ProviderKind, ProviderStatus, discover_providers, run_codex};
use codex_auth::{CodexAccount, begin_codex_login, read_codex_account};
use editor::{Editor, standard_actions};
use git::RepoSnapshot;
use gpui::{
    App, Bounds, Context, CursorStyle, Div, Entity, IntoElement, KeyBinding, Render, Role,
    StyleRefinement, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
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
    System,
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

struct RodeApp {
    project_path: PathBuf,
    project_name: String,
    repo: RepoSnapshot,
    providers: Vec<ProviderStatus>,
    codex_auth: CodexAuthState,
    messages: Vec<Message>,
    composer: Entity<Editor>,
    codex_thread_id: Option<String>,
    running: bool,
    show_diff: bool,
    thread_number: usize,
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

        Self {
            project_path: repo.root.clone(),
            project_name,
            repo,
            providers,
            codex_auth,
            messages: vec![Message {
                role: MessageRole::System,
                text: "Rode is using the native Wayland renderer. Codex turns run in the workspace-write sandbox by default.".to_owned(),
            }],
            composer,
            codex_thread_id: None,
            running: false,
            show_diff: true,
            thread_number: 1,
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
        self.running = true;
        cx.notify();

        let cwd = self.project_path.clone();
        let thread_id = self.codex_thread_id.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { run_codex(&cwd, &prompt, thread_id.as_deref()) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                match result {
                    Ok(run) => {
                        this.codex_thread_id = run.thread_id;
                        this.messages.push(Message {
                            role: MessageRole::Agent,
                            text: run.message,
                        });
                    }
                    Err(error) => this.messages.push(Message {
                        role: MessageRole::System,
                        text: format!("Agent turn failed: {error:#}"),
                    }),
                }
                this.repo = RepoSnapshot::load(&this.project_path);
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

    fn refresh_repo(&mut self, _: &RefreshRepo, _: &mut Window, cx: &mut Context<Self>) {
        self.repo = RepoSnapshot::load(&self.project_path);
        cx.notify();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let root_label = self.project_path.display().to_string();
        let thread_id = self
            .codex_thread_id
            .as_deref()
            .map(|id| id.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "not started".to_owned());
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
                            .id("new-thread")
                            .role(Role::Button)
                            .aria_label("New agent thread")
                            .size(px(28.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .bg(rgb(0x242831))
                            .hover(|style| style.bg(rgb(0x343946)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.thread_number += 1;
                                this.codex_thread_id = None;
                                this.messages = vec![Message {
                                    role: MessageRole::System,
                                    text: "New local thread. The first prompt will start a new Codex session.".to_owned(),
                                }];
                                cx.notify();
                            }))
                            .child("+"),
                    ),
            )
            .child(
                div()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x777d8b))
                            .child("PROJECT"),
                    )
                    .child(
                        div()
                            .rounded_lg()
                            .p_3()
                            .bg(rgb(0x1c1f26))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xe5e7eb))
                                    .child(self.project_name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x777d8b))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(root_label),
                            ),
                    )
                    .child(
                        div()
                            .mt_3()
                            .text_xs()
                            .text_color(rgb(0x777d8b))
                            .child("THREADS"),
                    )
                    .child(
                        div()
                            .rounded_lg()
                            .p_3()
                            .bg(rgb(0x272b35))
                            .border_1()
                            .border_color(rgb(0x3b82f6))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0xf3f4f6))
                                    .child(format!("Thread {}", self.thread_number)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x8b93a3))
                                    .child(format!("session {thread_id}")),
                            ),
                    ),
            )
            .child(div().flex_1())
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
                                    .child(
                                        div().text_xs().text_color(rgb(0xb9bec9)).child(format!(
                                            "{} · {}",
                                            ProviderKind::Claude.label(),
                                            if claude_available { "ready" } else { "missing" }
                                        )),
                                    ),
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
                            .child(format!("Thread {}", self.thread_number)),
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
                    ),
            )
    }

    fn render_messages(&self) -> impl IntoElement {
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
        if self.running {
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
                    .child("Codex is working…"),
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
                    .child(self.render_messages())
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
