use std::{collections::HashMap, path::PathBuf};

use crate::actions::{
    ActivateRailItem, CancelRename, CycleTheme, DismissModal, OpenSettings, OpenSourceControl,
    OpenTerminalRoute, OpenWorkspace, RefreshRepo, SendPrompt, SubmitRename, ToggleDiff,
    ToggleDiffLayout, ToggleTerminal,
};
use crate::agent::{ProviderKind, ProviderStatus, discover_providers};
use crate::codex::{self, ApprovalRequest, CodexEvent, CodexSession};
use crate::codex_auth::{
    CodexAccount, CodexLoginOutcome, PendingCodexLoginCancellation, begin_codex_login,
    read_codex_account,
};
use crate::diff::{
    DiffDocument, DiffFile, DiffHunk, DiffLine, DiffLineKind, DiffViewMode, split_rows,
};
use crate::editor::{Editor, standard_actions};
use crate::git::{
    RepoSnapshot, commit_all, create_pull_request, create_thread_worktree, push_current_branch,
};
use crate::notifications;
use crate::persistence::{StateStore, StoredMessage, StoredProject, StoredThread};
use crate::project::{ValidatedProject, validate_project};
use crate::terminal::{TerminalCore, TerminalView};
use crate::theme::{self, ThemeKind};
use crate::ui::{button, modal, selectable_row, split_pane, tabs, toast};
use gpui::{
    App, Context, CursorStyle, Div, Entity, IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent,
    MouseUpEvent, PathPromptOptions, Render, Role, StyleRefinement, Subscription, Window, div,
    prelude::*, px, rgb,
};

const ISOLATE_NEW_THREADS_SETTING: &str = "isolate_new_threads";
const ROUTE_SETTING: &str = "ui.route";
const THEME_SETTING: &str = "ui.theme";
const SIDEBAR_WIDTH_SETTING: &str = "ui.workspace.sidebar_width";
const INSPECTOR_WIDTH_SETTING: &str = "ui.workspace.inspector_width";

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

    fn storage_name(self) -> &'static str {
        match self {
            Self::Login | Self::Workspace => "workspace",
            Self::SourceControl => "source_control",
            Self::Terminal => "terminal",
            Self::Settings(_) => "settings",
        }
    }

    fn from_storage_name(value: &str) -> Self {
        match value {
            "source_control" => Self::SourceControl,
            "terminal" => Self::Terminal,
            "settings" => Self::Settings(SettingsSection::Appearance),
            _ => Self::Workspace,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Login => "Sign in",
            Self::Workspace => "Workspace",
            Self::SourceControl => "Source control",
            Self::Terminal => "Terminal",
            Self::Settings(_) => "Settings",
        }
    }

    fn same_surface(self, other: Self) -> bool {
        matches!(
            (self, other),
            (Self::Login, Self::Login)
                | (Self::Workspace, Self::Workspace)
                | (Self::SourceControl, Self::SourceControl)
                | (Self::Terminal, Self::Terminal)
                | (Self::Settings(_), Self::Settings(_))
        )
    }

    fn requires_project(self) -> bool {
        matches!(self, Self::SourceControl | Self::Terminal)
    }
}

impl SettingsSection {
    const ALL: [Self; 6] = [
        Self::Appearance,
        Self::AgentsAndModels,
        Self::Terminal,
        Self::GitAndWorktrees,
        Self::Keybindings,
        Self::Account,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::AgentsAndModels => "Agents & models",
            Self::Terminal => "Terminal",
            Self::GitAndWorktrees => "Git & worktrees",
            Self::Keybindings => "Keybindings",
            Self::Account => "Account",
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
    BrowserPending { auth_url: String },
    Cancelling,
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProjectSelectionState {
    Idle,
    ChoosingFolder,
    Validating(PathBuf),
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct UiPreferences {
    route: AppRoute,
    theme: ThemeKind,
    panels: split_pane::PanelLayout,
}

impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            route: AppRoute::Workspace,
            theme: ThemeKind::Ember,
            panels: split_pane::PanelLayout::default(),
        }
    }
}

impl UiPreferences {
    fn load(store: &StateStore) -> Self {
        let defaults = Self::default();
        Self {
            route: store
                .load_string_setting(ROUTE_SETTING)
                .ok()
                .flatten()
                .as_deref()
                .map(AppRoute::from_storage_name)
                .unwrap_or(defaults.route),
            theme: store
                .load_string_setting(THEME_SETTING)
                .ok()
                .flatten()
                .as_deref()
                .map(ThemeKind::from_storage_name)
                .unwrap_or(defaults.theme),
            panels: split_pane::PanelLayout {
                sidebar_width: store
                    .load_f32_setting(SIDEBAR_WIDTH_SETTING, defaults.panels.sidebar_width)
                    .unwrap_or(defaults.panels.sidebar_width),
                inspector_width: store
                    .load_f32_setting(INSPECTOR_WIDTH_SETTING, defaults.panels.inspector_width)
                    .unwrap_or(defaults.panels.inspector_width),
            }
            .sanitized(),
        }
    }
}

pub(crate) struct RodeApp {
    route: AppRoute,
    last_authenticated_route: AppRoute,
    theme: ThemeKind,
    panel_layout: split_pane::PanelLayout,
    active_split: Option<split_pane::SplitTarget>,
    modal: Option<ModalState>,
    toasts: toast::ToastQueue,
    state_store: Option<StateStore>,
    known_projects: Vec<StoredProject>,
    known_threads: Vec<StoredThread>,
    project_open: bool,
    project_selection: ProjectSelectionState,
    project_selection_generation: u64,
    project_picker_error: Option<String>,
    repairing_project: Option<PathBuf>,
    project_root: PathBuf,
    project_path: PathBuf,
    project_name: String,
    repo: RepoSnapshot,
    providers: Vec<ProviderStatus>,
    codex_auth: CodexAuthState,
    auth_attempt_generation: u64,
    pending_codex_login: Option<PendingCodexLoginCancellation>,
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
    drafts: HashMap<String, String>,
    thread_number: usize,
}

impl RodeApp {
    pub(crate) fn new(
        requested_project: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (
            state_store,
            known_projects,
            active_project_id,
            isolate_new_threads,
            ui_preferences,
            persistence_error,
        ) = match StateStore::open_default() {
            Ok(store) => {
                let ui_preferences = UiPreferences::load(&store);
                let isolate_new_threads = store
                    .load_bool_setting(ISOLATE_NEW_THREADS_SETTING, false)
                    .unwrap_or(false);
                let active_project_id = store.load_active_project_id().ok().flatten();
                match store.load_projects() {
                    Ok(projects) => (
                        Some(store),
                        projects,
                        active_project_id,
                        isolate_new_threads,
                        ui_preferences,
                        None,
                    ),
                    Err(error) => (
                        Some(store),
                        Vec::new(),
                        active_project_id,
                        isolate_new_threads,
                        ui_preferences,
                        Some(format!("Could not restore Rode projects: {error:#}")),
                    ),
                }
            }
            Err(error) => (
                None,
                Vec::new(),
                None,
                false,
                UiPreferences::default(),
                Some(format!("Could not open Rode state database: {error:#}")),
            ),
        };
        let (startup_project, project_picker_error) = select_startup_project(
            requested_project,
            &known_projects,
            active_project_id.as_deref(),
        );
        let project_open = false;
        let project_root = PathBuf::new();
        let project_path = PathBuf::new();
        let repo = RepoSnapshot::default();
        let restored_route = if ui_preferences.route.requires_project() {
            AppRoute::Workspace
        } else {
            ui_preferences.route
        };
        let project_name = "Choose a project".to_owned();
        let composer = cx.new(|cx| {
            Editor::new(
                "",
                "Ask the agent to inspect, change, or explain the project…",
                window,
                cx,
            )
        });
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

        let mut messages = vec![Message {
            role: MessageRole::System,
            text: "Rode is using the native Wayland renderer. Codex turns run in the workspace-write sandbox by default.".to_owned(),
        }];
        for error in [persistence_error].into_iter().flatten() {
            messages.push(Message {
                role: MessageRole::System,
                text: error,
            });
        }
        let thread_id = new_local_thread_id();
        let thread_branch = None;
        let codex_thread_id = None;
        let thread_number = 1;
        let thread_title = "Thread 1".to_owned();

        let mut app = Self {
            route: if codex_auth.requires_onboarding() {
                AppRoute::Login
            } else {
                restored_route
            },
            last_authenticated_route: restored_route,
            theme: ui_preferences.theme,
            panel_layout: ui_preferences.panels,
            active_split: None,
            modal: None,
            toasts: toast::ToastQueue::default(),
            state_store,
            known_projects,
            known_threads: Vec::new(),
            project_open,
            project_selection: ProjectSelectionState::Idle,
            project_selection_generation: 0,
            project_picker_error,
            repairing_project: None,
            project_root,
            project_path,
            project_name,
            repo,
            providers,
            codex_auth,
            auth_attempt_generation: 0,
            pending_codex_login: None,
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
            drafts: HashMap::new(),
            thread_number,
        };
        app.refresh_known_state();
        if let Some(path) = startup_project {
            app.validate_project_selection(path, None, cx);
        }
        app
    }

    fn persist_current_thread(&mut self) {
        if !self.project_open
            || !self.repo.is_repository
            || self.project_root.as_os_str().is_empty()
        {
            return;
        }
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
        if self.project_open {
            self.save_current_draft(cx);
            self.persist_current_thread();
        }
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
        if !self.repo.is_repository {
            let missing_path = self.project_root.clone();
            self.close_project();
            self.project_picker_error = Some(format!(
                "{} is no longer a Git repository. Repair or remove it below.",
                missing_path.display()
            ));
            cx.notify();
            return;
        }
        self.project_open = true;
        self.reconcile_project_route();
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
        let draft = self
            .drafts
            .get(&self.thread_id)
            .cloned()
            .unwrap_or_default();
        self.composer
            .update(cx, |editor, cx| editor.set_text(draft, cx));
        self.save_active_project();
        self.persist_current_thread();
        cx.notify();
    }

    fn save_current_draft(&mut self, cx: &mut Context<Self>) {
        self.drafts
            .insert(self.thread_id.clone(), self.composer.read(cx).text());
    }

    fn save_active_project(&mut self) {
        let Some(project_id) = self
            .known_projects
            .iter()
            .find(|project| project.path == self.project_root)
            .map(|project| project.id.clone())
        else {
            return;
        };
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.save_active_project_id(&project_id)
        {
            eprintln!("failed to save active Rode project: {error:#}");
        }
    }

    fn close_project(&mut self) {
        self.project_open = false;
        self.project_root.clear();
        self.project_path.clear();
        self.project_name = "Choose a project".to_owned();
        self.repo = RepoSnapshot::default();
        self.codex_session = None;
        self.codex_thread_id = None;
        self.active_turn_id = None;
        self.active_agent_message = None;
        self.running = false;
        self.creating_worktree = false;
        self.show_terminal = false;
        self.terminal_sessions.clear();
        self.route = AppRoute::Workspace;
        self.last_authenticated_route = AppRoute::Workspace;
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
        self.route = route_after_auth(
            self.route,
            self.last_authenticated_route,
            self.codex_auth.requires_onboarding(),
        );
    }

    fn reconcile_project_route(&mut self) {
        if !self.repo.is_repository
            && (self.route.requires_project() || self.last_authenticated_route.requires_project())
        {
            self.route = AppRoute::Workspace;
            self.last_authenticated_route = AppRoute::Workspace;
        }
    }

    fn navigate_to(&mut self, route: AppRoute, window: &mut Window, cx: &mut Context<Self>) {
        if self.codex_auth.requires_onboarding() {
            self.route = AppRoute::Login;
            cx.notify();
            return;
        }
        if route.requires_project() && !self.repo.is_repository {
            self.toasts.push(
                toast::ToastKind::Warning,
                format!("{} requires an open Git project.", route.label()),
            );
            cx.notify();
            return;
        }

        self.route = route;
        self.last_authenticated_route = route;
        self.show_create_menu = false;
        self.show_settings = false;
        self.modal = None;
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.save_string_setting(ROUTE_SETTING, route.storage_name())
        {
            self.toasts.push(
                toast::ToastKind::Error,
                format!("Could not save the selected route: {error:#}"),
            );
        }

        match route {
            AppRoute::Workspace => {
                let focus = self.composer.read(cx).focus_handle.clone();
                window.focus(&focus, cx);
            }
            AppRoute::Terminal => {
                self.ensure_terminal(window, cx);
                if let Some(terminal) = self.terminal_sessions.get(&self.thread_id) {
                    let focus = terminal.read(cx).focus_handle.clone();
                    window.focus(&focus, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn open_workspace(&mut self, _: &OpenWorkspace, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to(AppRoute::Workspace, window, cx);
    }

    fn open_source_control(
        &mut self,
        _: &OpenSourceControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_to(AppRoute::SourceControl, window, cx);
    }

    fn open_terminal_route(
        &mut self,
        _: &OpenTerminalRoute,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_to(AppRoute::Terminal, window, cx);
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_to(AppRoute::Settings(SettingsSection::Appearance), window, cx);
    }

    fn set_theme(&mut self, selected: ThemeKind, cx: &mut Context<Self>) {
        if self.theme == selected {
            return;
        }
        self.theme = selected;
        for terminal in self.terminal_sessions.values() {
            terminal.update(cx, |terminal, cx| terminal.set_theme(selected, cx));
        }
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.save_string_setting(THEME_SETTING, selected.storage_name())
        {
            self.toasts.push(
                toast::ToastKind::Error,
                format!("Could not save the selected theme: {error:#}"),
            );
        }
        cx.notify();
    }

    fn set_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
        let route = AppRoute::Settings(section);
        self.route = route;
        self.last_authenticated_route = route;
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.save_string_setting(ROUTE_SETTING, route.storage_name())
        {
            self.toasts.push(
                toast::ToastKind::Error,
                format!("Could not save the settings section: {error:#}"),
            );
        }
        cx.notify();
    }

    fn cycle_theme(&mut self, _: &CycleTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.set_theme(self.theme.next(), cx);
    }

    fn resize_panels(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(active) = self.active_split else {
            return;
        };
        match active {
            split_pane::SplitTarget::Sidebar => {
                self.panel_layout.sidebar_width =
                    f32::from(event.position.x) - split_pane::RAIL_WIDTH;
            }
            split_pane::SplitTarget::Inspector => {
                self.panel_layout.inspector_width =
                    f32::from(window.viewport_size().width) - f32::from(event.position.x);
            }
        }
        self.panel_layout = self.panel_layout.sanitized();
        cx.notify();
    }

    fn finish_panel_resize(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.active_split.take().is_none() {
            return;
        }
        if let Some(store) = self.state_store.as_mut() {
            let sidebar =
                store.save_f32_setting(SIDEBAR_WIDTH_SETTING, self.panel_layout.sidebar_width);
            let inspector =
                store.save_f32_setting(INSPECTOR_WIDTH_SETTING, self.panel_layout.inspector_width);
            if let Err(error) = sidebar.and(inspector) {
                self.toasts.push(
                    toast::ToastKind::Error,
                    format!("Could not save panel widths: {error:#}"),
                );
            }
        }
        cx.notify();
    }

    pub(crate) fn refresh_codex_account(&mut self, cx: &mut Context<Self>) {
        self.auth_attempt_generation = self.auth_attempt_generation.wrapping_add(1);
        self.pending_codex_login = None;
        let generation = self.auth_attempt_generation;
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
                if this.auth_attempt_generation != generation {
                    return;
                }
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
        if !self.codex_available()
            || matches!(
                self.codex_auth,
                CodexAuthState::SigningIn
                    | CodexAuthState::BrowserPending { .. }
                    | CodexAuthState::Cancelling
            )
        {
            return;
        }

        self.auth_attempt_generation = self.auth_attempt_generation.wrapping_add(1);
        let generation = self.auth_attempt_generation;
        self.pending_codex_login = None;
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
                        if this.auth_attempt_generation != generation {
                            return;
                        }
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
            let cancellation = pending.cancellation();
            if this
                .update(cx, |this, cx| {
                    if this.auth_attempt_generation != generation {
                        return;
                    }
                    this.pending_codex_login = Some(cancellation);
                    this.codex_auth = CodexAuthState::BrowserPending {
                        auth_url: auth_url.clone(),
                    };
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
                if this.auth_attempt_generation != generation {
                    return;
                }
                this.pending_codex_login = None;
                match result {
                    Ok(CodexLoginOutcome::Complete(status)) => {
                        this.codex_auth = status.account.map(CodexAuthState::SignedIn).unwrap_or(
                            CodexAuthState::SignedOut,
                        );
                        this.sync_route_with_auth();
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: "Signed in to OpenAI through Codex.".to_owned(),
                        });
                        this.toasts
                            .push(toast::ToastKind::Success, "ChatGPT sign-in complete.");
                    }
                    Ok(CodexLoginOutcome::Cancelled) => {
                        this.codex_auth = CodexAuthState::SignedOut;
                        this.sync_route_with_auth();
                        this.messages.push(Message {
                            role: MessageRole::System,
                            text: "ChatGPT sign-in was cancelled.".to_owned(),
                        });
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

    fn reopen_codex_login(&mut self, cx: &mut Context<Self>) {
        if let CodexAuthState::BrowserPending { auth_url } = &self.codex_auth {
            cx.open_url(auth_url);
        }
    }

    fn cancel_codex_login(&mut self, cx: &mut Context<Self>) {
        let Some(cancellation) = self.pending_codex_login.take() else {
            return;
        };
        let generation = self.auth_attempt_generation;
        let auth_url = match &self.codex_auth {
            CodexAuthState::BrowserPending { auth_url } => auth_url.clone(),
            _ => return,
        };
        self.codex_auth = CodexAuthState::Cancelling;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let request = cancellation.clone();
            let result = cx.background_spawn(async move { request.cancel() }).await;
            this.update(cx, |this, cx| {
                if this.auth_attempt_generation != generation {
                    return;
                }
                if let Err(error) = result {
                    this.pending_codex_login = Some(cancellation);
                    this.codex_auth = CodexAuthState::BrowserPending { auth_url };
                    this.toasts.push(
                        toast::ToastKind::Error,
                        format!("Could not cancel ChatGPT sign-in: {error:#}"),
                    );
                }
                this.sync_route_with_auth();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn send_prompt(&mut self, _: &SendPrompt, _: &mut Window, cx: &mut Context<Self>) {
        if !self.project_open || !self.repo.is_repository {
            self.project_picker_error =
                Some("Open a Git project before starting a thread.".to_owned());
            cx.notify();
            return;
        }
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
        let repair_target = self.repairing_project.take();
        self.project_selection = ProjectSelectionState::ChoosingFolder;
        self.project_picker_error = None;
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
                Err(_) => {
                    this.update_in(cx, |this, _, cx| {
                        this.project_selection = ProjectSelectionState::Idle;
                        this.project_picker_error =
                            Some("The desktop folder picker closed unexpectedly.".to_owned());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            this.update_in(cx, |this, _window, cx| match result {
                Ok(Some(paths)) => {
                    if let Some(path) = paths.into_iter().next() {
                        this.validate_project_selection(path, repair_target, cx);
                    }
                }
                Ok(None) => {
                    this.project_selection = ProjectSelectionState::Idle;
                    cx.notify();
                }
                Err(error) => {
                    this.project_selection = ProjectSelectionState::Idle;
                    this.project_picker_error =
                        Some(format!("Could not open the folder picker: {error:#}"));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn validate_project_selection(
        &mut self,
        path: PathBuf,
        repair_target: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.project_selection_generation = self.project_selection_generation.wrapping_add(1);
        let generation = self.project_selection_generation;
        self.project_selection = ProjectSelectionState::Validating(path.clone());
        self.project_picker_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { validate_project(&path) })
                .await;
            this.update(cx, |this, cx| {
                if this.project_selection_generation != generation {
                    return;
                }
                this.project_selection = ProjectSelectionState::Idle;
                match result {
                    Ok(project) => this.open_validated_project(project, repair_target, cx),
                    Err(error) => {
                        this.project_picker_error = Some(format!("{error:#}"));
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_validated_project(
        &mut self,
        project: ValidatedProject,
        repair_target: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.project_open {
            self.save_current_draft(cx);
            self.persist_current_thread();
        }
        self.refresh_known_state();
        let duplicate = self
            .known_projects
            .iter()
            .find(|stored| stored.path == project.root)
            .cloned();
        if let Some(old_path) = repair_target.as_ref()
            && duplicate
                .as_ref()
                .is_some_and(|stored| &stored.path != old_path)
        {
            self.project_picker_error = Some(
                "That repository is already in Recent projects. Open it directly instead."
                    .to_owned(),
            );
            cx.notify();
            return;
        }

        let persistence_result = if let Some(old_path) = repair_target.as_ref() {
            self.state_store
                .as_mut()
                .map(|store| store.repair_project_path(old_path, &project.root, &project.name))
        } else if duplicate.is_none() {
            self.state_store.as_mut().map(|store| {
                store.save_project(&StoredProject::new(
                    project.root.clone(),
                    project.name.clone(),
                ))
            })
        } else {
            None
        };
        if let Some(Err(error)) = persistence_result {
            self.project_picker_error = Some(format!("Could not save the project: {error:#}"));
            cx.notify();
            return;
        }

        self.refresh_known_state();
        let stored = self
            .known_projects
            .iter()
            .find(|stored| stored.path == project.root)
            .cloned();
        self.project_root = project.root.clone();
        self.project_path = project.root;
        self.project_name = stored
            .as_ref()
            .map(|stored| stored.name.clone())
            .unwrap_or(project.name);
        self.repo = RepoSnapshot::load(&self.project_path);
        if !self.repo.is_repository {
            self.close_project();
            self.project_picker_error =
                Some("Git validation changed while opening the project. Try again.".to_owned());
            cx.notify();
            return;
        }
        self.project_root = self.repo.root.clone();
        self.project_open = true;
        self.project_picker_error = None;
        let restored_thread_id = self
            .state_store
            .as_ref()
            .and_then(|store| store.load_active_thread(&self.project_root).ok().flatten())
            .map(|thread| thread.id);
        if let Some(thread_id) = restored_thread_id {
            self.project_open = false;
            self.switch_thread(&thread_id, cx);
        } else {
            self.save_active_project();
            self.start_new_thread(false, cx);
        }
    }

    fn repair_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.repairing_project = Some(path);
        self.open_folder_picker(window, cx);
    }

    fn remove_project(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if let Some(store) = self.state_store.as_mut()
            && let Err(error) = store.remove_project(&path)
        {
            self.project_picker_error = Some(format!("Could not remove the project: {error:#}"));
            cx.notify();
            return;
        }
        if self.project_root == path {
            self.close_project();
        }
        self.refresh_known_state();
        cx.notify();
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
        if !self.project_open || !self.repo.is_repository {
            return;
        }
        self.show_create_menu = false;
        self.start_new_thread(true, cx);
    }

    fn new_thread_in_project(
        &mut self,
        project_path: PathBuf,
        project_name: String,
        cx: &mut Context<Self>,
    ) {
        if !project_path.is_dir() {
            self.close_project();
            self.project_picker_error = Some("That saved project folder is missing.".to_owned());
            cx.notify();
            return;
        }
        self.persist_current_thread();
        self.project_root = project_path.clone();
        self.project_path = project_path;
        self.project_name = project_name;
        self.repo = RepoSnapshot::load(&self.project_path);
        self.project_open = self.repo.is_repository;
        if !self.project_open {
            self.close_project();
            self.project_picker_error =
                Some("That saved folder is no longer a Git repository.".to_owned());
            cx.notify();
            return;
        }
        self.reconcile_project_route();
        self.start_new_thread(false, cx);
    }

    fn start_new_thread(&mut self, persist_previous: bool, cx: &mut Context<Self>) {
        if !self.project_open || !self.repo.is_repository {
            return;
        }
        if persist_previous {
            self.save_current_draft(cx);
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
        self.composer
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
                let theme = self.theme;
                let terminal = cx.new(|cx| TerminalView::new(core, theme, window, cx));
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
            .border_color(rgb(theme::tokens(self.theme).colors.accent_hover))
            .bg(rgb(theme::tokens(self.theme).colors.raised))
            .text_sm()
            .text_color(rgb(theme::tokens(self.theme).colors.text))
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
                rgb(theme::tokens(self.theme).colors.raised)
            } else {
                rgb(theme::tokens(self.theme).colors.chrome)
            })
            .border_1()
            .border_color(if project_is_active {
                rgb(theme::tokens(self.theme).colors.strong_border)
            } else {
                rgb(theme::tokens(self.theme).colors.border)
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
                                        .text_color(rgb(theme::tokens(self.theme).colors.text))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(project.name.clone()),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
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
                                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                    .hover(|style| {
                                        style.bg(rgb(theme::tokens(self.theme).colors.overlay))
                                    })
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
                                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                    .hover(|style| {
                                        style.bg(rgb(theme::tokens(self.theme).colors.overlay))
                                    })
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
                            .bg(if active {
                                rgb(theme::tokens(self.theme).colors.overlay)
                            } else {
                                rgb(theme::tokens(self.theme).colors.panel)
                            })
                            .border_1()
                            .border_color(if active {
                                rgb(theme::tokens(self.theme).colors.accent_hover)
                            } else {
                                rgb(theme::tokens(self.theme).colors.border)
                            })
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
                                                .text_color(rgb(theme::tokens(self.theme)
                                                    .colors
                                                    .text))
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(thread.title.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(theme::tokens(self.theme)
                                                .colors
                                                .muted_text))
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
                                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                    .hover(|style| {
                                        style
                                            .bg(rgb(theme::tokens(self.theme).colors.strong_border))
                                    })
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
        let colors = theme::tokens(self.theme).colors;
        let (status_color, status_label) = match &self.codex_auth {
            CodexAuthState::Unavailable => (colors.error, "CLI not found"),
            CodexAuthState::Checking => (colors.warning, "Checking account"),
            CodexAuthState::SignedOut => (colors.info, "Ready to connect"),
            CodexAuthState::SigningIn => (colors.info, "Starting sign-in"),
            CodexAuthState::BrowserPending { .. } => (colors.info, "Waiting for browser"),
            CodexAuthState::Cancelling => (colors.warning, "Cancelling sign-in"),
            CodexAuthState::Error(_) => (colors.error, "Connection error"),
            CodexAuthState::SignedIn(_) => (colors.success, "Connected"),
        };
        let error = match &self.codex_auth {
            CodexAuthState::Error(error) => Some(error.clone()),
            _ => None,
        };

        div()
            .id("auth-onboarding")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.as_str() == "escape"
                    && matches!(this.codex_auth, CodexAuthState::BrowserPending { .. })
                {
                    this.cancel_codex_login(cx);
                    cx.stop_propagation();
                }
            }))
            .size_full()
            .min_w(px(720.))
            .flex()
            .flex_col()
            .bg(rgb(theme::tokens(self.theme).colors.root))
            .text_color(rgb(theme::tokens(self.theme).colors.text))
            .child(
                div()
                    .h(px(64.))
                    .flex_none()
                    .px_6()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.overlay))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::tokens(self.theme).colors.text))
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
                            .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                            .bg(rgb(theme::tokens(self.theme).colors.panel))
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
                                            .text_color(rgb(theme::tokens(self.theme).colors.text))
                                            .child("Connect Codex"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .line_height(px(21.))
                                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                            .child(
                                                "Rode uses your installed Codex CLI and OpenAI account. Authentication stays managed by Codex.",
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(rgb(theme::tokens(self.theme).colors.accent_hover))
                                    .bg(rgb(theme::tokens(self.theme).colors.panel))
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
                                                    .bg(rgb(theme::tokens(self.theme).colors.text))
                                                    .font_family("monospace")
                                                    .font_weight(gpui::FontWeight::BOLD)
                                                    .text_color(rgb(theme::tokens(self.theme).colors.root))
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
                                                            .text_color(rgb(theme::tokens(self.theme).colors.text))
                                                            .child("Codex"),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                                            .child("OpenAI coding agent"),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .px_3()
                                            .py_1()
                                            .rounded_full()
                                            .bg(rgb(theme::tokens(self.theme).colors.accent_soft))
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
                                                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                                    .child(status_label),
                                            ),
                                    ),
                            )
                            .when(!self.codex_available(), |content| {
                                content.child(
                                    div()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                        .bg(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                        .p_4()
                                        .text_sm()
                                        .line_height(px(20.))
                                        .text_color(rgb(theme::tokens(self.theme).colors.error))
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
                                        .border_color(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                        .bg(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                        .p_4()
                                        .text_sm()
                                        .text_color(rgb(theme::tokens(self.theme).colors.error))
                                        .child(error),
                                )
                            })
                            .child(match &self.codex_auth {
                                CodexAuthState::SignedOut => div()
                                    .id("onboarding-sign-in")
                                    .role(Role::Button)
                                    .aria_label("Sign in to OpenAI with Codex")
                                    .tab_index(0)
                                    .tab_stop(true)
                                    .w_full()
                                    .h(px(44.))
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(theme::tokens(self.theme).colors.accent))
                                    .hover(|style| style.bg(rgb(theme::tokens(self.theme).colors.accent_hover)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .text_color(rgb(theme::tokens(self.theme).colors.on_accent))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sign_in_codex(cx)
                                    }))
                                    .on_key_down(cx.listener(|this, event, _, cx| {
                                        if is_activation_key(event) {
                                            this.sign_in_codex(cx);
                                            cx.stop_propagation();
                                        }
                                    }))
                                    .child("Sign in with ChatGPT")
                                    .into_any_element(),
                                CodexAuthState::Error(_) => div()
                                    .id("onboarding-auth-retry")
                                    .role(Role::Button)
                                    .aria_label("Retry the Codex account check")
                                    .tab_index(0)
                                    .tab_stop(true)
                                    .w_full()
                                    .h(px(44.))
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(theme::tokens(self.theme).colors.strong_border))
                                    .hover(|style| style.bg(rgb(theme::tokens(self.theme).colors.strong_border)))
                                    .text_sm()
                                    .text_color(rgb(theme::tokens(self.theme).colors.text))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_codex_account(cx)
                                    }))
                                    .on_key_down(cx.listener(|this, event, _, cx| {
                                        if is_activation_key(event) {
                                            this.refresh_codex_account(cx);
                                            cx.stop_propagation();
                                        }
                                    }))
                                    .child("Try again")
                                    .into_any_element(),
                                CodexAuthState::Checking => {
                                    onboarding_status("Checking account…", self.theme)
                                }
                                CodexAuthState::SigningIn => {
                                    onboarding_status("Starting browser sign-in…", self.theme)
                                }
                                CodexAuthState::BrowserPending { .. } => div()
                                    .w_full()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        div()
                                            .id("onboarding-reopen-browser")
                                            .role(Role::Button)
                                            .aria_label("Open the ChatGPT sign-in page again")
                                            .tab_index(0)
                                            .tab_stop(true)
                                            .flex_1()
                                            .h(px(44.))
                                            .rounded_lg()
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .bg(rgb(theme::tokens(self.theme).colors.accent))
                                            .hover(|style| style.bg(rgb(theme::tokens(self.theme).colors.accent_hover)))
                                            .text_sm()
                                            .text_color(rgb(theme::tokens(self.theme).colors.on_accent))
                                            .on_click(cx.listener(|this, _, _, cx| this.reopen_codex_login(cx)))
                                            .on_key_down(cx.listener(|this, event, _, cx| {
                                                if is_activation_key(event) {
                                                    this.reopen_codex_login(cx);
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .child("Open browser again"),
                                    )
                                    .child(
                                        div()
                                            .id("onboarding-cancel-login")
                                            .role(Role::Button)
                                            .aria_label("Cancel ChatGPT sign-in")
                                            .tab_index(0)
                                            .tab_stop(true)
                                            .flex_1()
                                            .h(px(44.))
                                            .rounded_lg()
                                            .cursor_pointer()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .bg(rgb(theme::tokens(self.theme).colors.strong_border))
                                            .text_sm()
                                            .text_color(rgb(theme::tokens(self.theme).colors.text))
                                            .on_click(cx.listener(|this, _, _, cx| this.cancel_codex_login(cx)))
                                            .on_key_down(cx.listener(|this, event, _, cx| {
                                                if is_activation_key(event) {
                                                    this.cancel_codex_login(cx);
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .child("Cancel sign-in"),
                                    )
                                    .into_any_element(),
                                CodexAuthState::Cancelling => {
                                    onboarding_status("Cancelling sign-in…", self.theme)
                                }
                                CodexAuthState::Unavailable => {
                                    onboarding_status("Codex CLI required", self.theme)
                                }
                                CodexAuthState::SignedIn(_) => div().into_any_element(),
                            })
                            .child(
                                div()
                                    .text_center()
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
                                    .child(
                                        "Sign-in opens in your browser. Codex stores and refreshes the session.",
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_project_onboarding(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = theme::tokens(self.theme).colors;
        let account = match &self.codex_auth {
            CodexAuthState::SignedIn(account) => account.summary(),
            _ => "OpenAI account".to_owned(),
        };
        let operation = match &self.project_selection {
            ProjectSelectionState::Idle => None,
            ProjectSelectionState::ChoosingFolder => {
                Some("Waiting for the folder picker…".to_owned())
            }
            ProjectSelectionState::Validating(path) => {
                Some(format!("Validating {}…", path.display()))
            }
        };
        let recent_projects = self
            .known_projects
            .iter()
            .enumerate()
            .map(|(index, project)| {
                let path = project.path.clone();
                let select_path = path.clone();
                let select_path_key = path.clone();
                let repair_path = path.clone();
                let repair_path_key = path.clone();
                let remove_path = path.clone();
                let remove_path_key = path.clone();
                let available = path.is_dir();
                let status = if available {
                    "Git project"
                } else {
                    "Folder missing"
                };
                div()
                    .id(("recent-project", index))
                    .w_full()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(if available {
                        colors.border
                    } else {
                        colors.deletion
                    }))
                    .bg(rgb(colors.chrome))
                    .p_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .when(available, |row| {
                        row.role(Role::Button)
                            .aria_label(format!("Open project {}", project.name))
                            .tab_index(0)
                            .tab_stop(true)
                            .cursor_pointer()
                            .hover(move |style| style.bg(rgb(colors.overlay)))
                            .focus_visible(move |style| style.border_color(rgb(colors.focus_ring)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.validate_project_selection(select_path.clone(), None, cx)
                            }))
                            .on_key_down(cx.listener(move |this, event, _, cx| {
                                if is_activation_key(event) {
                                    this.validate_project_selection(
                                        select_path_key.clone(),
                                        None,
                                        cx,
                                    );
                                    cx.stop_propagation();
                                }
                            }))
                    })
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(colors.text))
                                    .child(project.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(colors.faint_text))
                                    .overflow_hidden()
                                    .child(path.display().to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if available {
                                        colors.success
                                    } else {
                                        colors.error
                                    }))
                                    .child(status),
                            )
                            .when(!available, |actions| {
                                actions
                                    .child(
                                        div()
                                            .id(("repair-project", index))
                                            .role(Role::Button)
                                            .aria_label(format!("Repair project {}", project.name))
                                            .tab_index(0)
                                            .tab_stop(true)
                                            .cursor_pointer()
                                            .rounded_md()
                                            .px_2()
                                            .py_1()
                                            .bg(rgb(colors.overlay))
                                            .text_xs()
                                            .text_color(rgb(colors.text))
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.repair_project(repair_path.clone(), window, cx)
                                            }))
                                            .on_key_down(cx.listener(
                                                move |this, event, window, cx| {
                                                    if is_activation_key(event) {
                                                        this.repair_project(
                                                            repair_path_key.clone(),
                                                            window,
                                                            cx,
                                                        );
                                                        cx.stop_propagation();
                                                    }
                                                },
                                            ))
                                            .child("Repair"),
                                    )
                                    .child(
                                        div()
                                            .id(("remove-project", index))
                                            .role(Role::Button)
                                            .aria_label(format!(
                                                "Remove project {} from Rode",
                                                project.name
                                            ))
                                            .tab_index(0)
                                            .tab_stop(true)
                                            .cursor_pointer()
                                            .rounded_md()
                                            .px_2()
                                            .py_1()
                                            .bg(rgb(colors.deletion_soft))
                                            .text_xs()
                                            .text_color(rgb(colors.error))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.remove_project(remove_path.clone(), cx)
                                            }))
                                            .on_key_down(cx.listener(move |this, event, _, cx| {
                                                if is_activation_key(event) {
                                                    this.remove_project(
                                                        remove_path_key.clone(),
                                                        cx,
                                                    );
                                                    cx.stop_propagation();
                                                }
                                            }))
                                            .child("Remove"),
                                    )
                            }),
                    )
                    .into_any_element()
            });

        div()
            .id("project-onboarding")
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key.as_str() == "escape"
                    && !matches!(this.project_selection, ProjectSelectionState::Idle)
                {
                    this.project_selection_generation =
                        this.project_selection_generation.wrapping_add(1);
                    this.project_selection = ProjectSelectionState::Idle;
                    this.repairing_project = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .size_full()
            .min_w(px(720.))
            .flex()
            .flex_col()
            .bg(rgb(colors.root))
            .text_color(rgb(colors.text))
            .child(
                div()
                    .h(px(64.))
                    .flex_none()
                    .px_6()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(colors.overlay))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("RODE"),
                    )
                    .child(div().text_xs().text_color(rgb(colors.muted_text)).child(account)),
            )
            .child(
                div()
                    .id("project-onboarding-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_8()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .w(px(640.))
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
                                            .child("Open a Git project"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .line_height(px(21.))
                                            .text_color(rgb(colors.muted_text))
                                            .child("Choose a repository to finish setup. Rode validates the Git root and restores it next time."),
                                    ),
                            )
                            .when_some(operation, |card, operation| {
                                card.child(
                                    div()
                                        .rounded_lg()
                                        .bg(rgb(colors.accent_soft))
                                        .p_3()
                                        .text_sm()
                                        .text_color(rgb(colors.info))
                                        .child(operation),
                                )
                            })
                            .when_some(self.project_picker_error.clone(), |card, error| {
                                card.child(
                                    div()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(colors.deletion))
                                        .bg(rgb(colors.deletion_soft))
                                        .p_3()
                                        .text_sm()
                                        .text_color(rgb(colors.error))
                                        .child(error),
                                )
                            })
                            .child(
                                div()
                                    .id("browse-project-folder")
                                    .role(Role::Button)
                                    .aria_label("Open a Git project folder")
                                    .tab_index(0)
                                    .tab_stop(matches!(self.project_selection, ProjectSelectionState::Idle))
                                    .h(px(44.))
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(rgb(colors.accent))
                                    .hover(move |style| style.bg(rgb(colors.accent_hover)))
                                    .focus_visible(move |style| style.border_color(rgb(colors.focus_ring)))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_sm()
                                    .text_color(rgb(colors.on_accent))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.repairing_project = None;
                                        this.open_folder_picker(window, cx)
                                    }))
                                    .on_key_down(cx.listener(|this, event, window, cx| {
                                        if is_activation_key(event) {
                                            this.repairing_project = None;
                                            this.open_folder_picker(window, cx);
                                            cx.stop_propagation();
                                        }
                                    }))
                                    .child("Open project folder…"),
                            )
                            .when(!self.known_projects.is_empty(), |card| {
                                card.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(rgb(colors.muted_text))
                                                .child("RECENT PROJECTS"),
                                        )
                                        .children(recent_projects),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let colors = theme::tokens(self.theme).colors;
        let (codex_color, codex_label) = match &self.codex_auth {
            CodexAuthState::Unavailable => (colors.faint_text, "Codex · missing".to_owned()),
            CodexAuthState::Checking => (colors.warning, "Codex · checking account".to_owned()),
            CodexAuthState::SignedOut => (colors.warning, "Codex · sign in required".to_owned()),
            CodexAuthState::SignedIn(account) => {
                (colors.success, format!("Codex · {}", account.summary()))
            }
            CodexAuthState::SigningIn => (colors.info, "Codex · waiting for browser".to_owned()),
            CodexAuthState::BrowserPending { .. } => {
                (colors.info, "Codex · waiting for browser".to_owned())
            }
            CodexAuthState::Cancelling => (colors.warning, "Codex · cancelling sign-in".to_owned()),
            CodexAuthState::Error(_) => (colors.error, "Codex · authentication error".to_owned()),
        };
        let codex_error = match &self.codex_auth {
            CodexAuthState::Error(error) => Some(error.clone()),
            _ => None,
        };
        let codex_status = div()
            .rounded_md()
            .p_2()
            .bg(rgb(theme::tokens(self.theme).colors.panel))
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
                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(codex_label),
                    ),
            )
            .when_some(codex_error, |status, error| {
                status.child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme::tokens(self.theme).colors.error))
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
                            .bg(rgb(theme::tokens(self.theme).colors.accent))
                            .hover(|style| {
                                style.bg(rgb(theme::tokens(self.theme).colors.accent_hover))
                            })
                            .text_xs()
                            .text_color(rgb(theme::tokens(self.theme).colors.on_accent))
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
                            .bg(rgb(theme::tokens(self.theme).colors.strong_border))
                            .hover(|style| {
                                style.bg(rgb(theme::tokens(self.theme).colors.strong_border))
                            })
                            .text_xs()
                            .text_color(rgb(theme::tokens(self.theme).colors.text))
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_codex_account(cx)))
                            .child("Retry account check"),
                    )
                },
            );

        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(theme::tokens(self.theme).colors.root))
            .border_r_1()
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .child(
                div()
                    .h(px(58.))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::tokens(self.theme).colors.text))
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
                            .bg(rgb(theme::tokens(self.theme).colors.overlay))
                            .hover(|style| {
                                style.bg(rgb(theme::tokens(self.theme).colors.strong_border))
                            })
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
                        .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                        .bg(rgb(theme::tokens(self.theme).colors.raised))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(selectable_row::selectable_row(
                            "create-thread",
                            "New thread",
                            false,
                            false,
                            self.theme,
                            cx.listener(|this, _, _, cx| this.new_thread(cx)),
                        ))
                        .child(selectable_row::selectable_row(
                            "add-project-folder",
                            "Add project folder…",
                            false,
                            false,
                            self.theme,
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
                            .child("PROJECTS"),
                    )
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
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
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
                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                            .hover(|style| style.bg(rgb(theme::tokens(self.theme).colors.overlay)))
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
                                .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                                .bg(rgb(theme::tokens(self.theme).colors.panel))
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(rgb(theme::tokens(self.theme).colors.text))
                                        .child("New thread workspace"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .line_height(px(16.))
                                        .text_color(rgb(theme::tokens(self.theme)
                                            .colors
                                            .faint_text))
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
                                            rgb(theme::tokens(self.theme).colors.accent)
                                        } else {
                                            rgb(theme::tokens(self.theme).colors.overlay)
                                        })
                                        .text_xs()
                                        .text_color(rgb(theme::tokens(self.theme).colors.text))
                                        .hover(|style| {
                                            style.bg(rgb(theme::tokens(self.theme)
                                                .colors
                                                .accent_hover))
                                        })
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
            .bg(rgb(theme::tokens(self.theme).colors.chrome))
            .border_b_1()
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::tokens(self.theme).colors.text))
                            .child(self.thread_title.clone()),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(rgb(theme::tokens(self.theme).colors.raised))
                            .text_xs()
                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
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
                        self.theme,
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
                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                            .hover(|style| style.bg(rgb(theme::tokens(self.theme).colors.overlay)))
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
                        self.theme,
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
                            self.theme,
                            cx.listener(|this, _, _, cx| this.cancel_turn(cx)),
                        ))
                    }),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::tokens(self.theme).colors;
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
                MessageRole::User => ("YOU", colors.accent_soft, colors.focus_ring, colors.text),
                MessageRole::Agent => ("CODEX", colors.panel, colors.border, colors.text),
                MessageRole::Tool => ("TOOL", colors.raised, colors.strong_border, colors.text),
                MessageRole::System => ("RODE", colors.addition_soft, colors.success, colors.text),
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
                            .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
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
                    .border_color(rgb(theme::tokens(self.theme).colors.warning))
                    .bg(rgb(colors.warning_soft))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(theme::tokens(self.theme).colors.warning))
                            .child(kind),
                    )
                    .child(
                        div()
                            .font_family("monospace")
                            .text_sm()
                            .whitespace_normal()
                            .text_color(rgb(theme::tokens(self.theme).colors.warning))
                            .child(request.title.clone()),
                    )
                    .when(!request.detail.is_empty(), |card| {
                        card.child(
                            div()
                                .text_xs()
                                .whitespace_normal()
                                .text_color(rgb(theme::tokens(self.theme).colors.warning))
                                .child(request.detail.clone()),
                        )
                    })
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(theme::tokens(self.theme).colors.warning))
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
                                    .bg(rgb(theme::tokens(self.theme).colors.addition_soft))
                                    .text_xs()
                                    .text_color(rgb(theme::tokens(self.theme).colors.success))
                                    .hover(|style| {
                                        style.bg(rgb(theme::tokens(self.theme).colors.success))
                                    })
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
                                    .bg(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                    .text_xs()
                                    .text_color(rgb(theme::tokens(self.theme).colors.error))
                                    .hover(|style| {
                                        style
                                            .bg(rgb(theme::tokens(self.theme).colors.deletion_soft))
                                    })
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
                    .border_color(rgb(theme::tokens(self.theme).colors.overlay))
                    .bg(rgb(theme::tokens(self.theme).colors.panel))
                    .p_4()
                    .text_sm()
                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
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
            .min_w_0()
            .overflow_hidden()
            .border_t_1()
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .bg(rgb(theme::tokens(self.theme).colors.chrome))
            .child(
                div()
                    .id("composer")
                    .key_context("Composer")
                    .track_focus(&focus_handle)
                    .map(standard_actions(self.composer.clone()))
                    .cursor(CursorStyle::IBeam)
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .h(px(112.))
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                    .bg(rgb(theme::tokens(self.theme).colors.raised))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .line_height(px(20.))
                    .text_size(px(14.))
                    .text_color(rgb(theme::tokens(self.theme).colors.text))
                    .child(
                        div().w_full().min_w_0().h(px(72.)).overflow_hidden().child(
                            self.composer
                                .clone()
                                .cached(StyleRefinement::default().w_full().h(px(72.))),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_xs()
                            .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
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

    fn render_terminal(&self, expanded: bool, cx: &mut Context<Self>) -> Div {
        let terminal = self.terminal_sessions.get(&self.thread_id).cloned();
        let (title, exited) = terminal.as_ref().map_or_else(
            || ("Preparing terminal…".to_owned(), false),
            |terminal| {
                let terminal = terminal.read(cx);
                (terminal.title().to_owned(), terminal.exited())
            },
        );
        let terminal_style = if expanded {
            StyleRefinement::default().w_full().h_full()
        } else {
            StyleRefinement::default().w_full().h(px(250.))
        };
        div()
            .when(expanded, |panel| panel.flex_1().min_h_0())
            .when(!expanded, |panel| {
                panel.h(px(300.)).min_h(px(180.)).flex_none()
            })
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .bg(rgb(theme::tokens(self.theme).colors.root))
            .child(
                div()
                    .h(px(34.))
                    .flex_none()
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(theme::tokens(self.theme).colors.chrome))
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .child(
                        div()
                            .min_w_0()
                            .text_xs()
                            .text_color(if exited {
                                rgb(theme::tokens(self.theme).colors.error)
                            } else {
                                rgb(theme::tokens(self.theme).colors.muted_text)
                            })
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
                            .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                            .hover(|style| {
                                style
                                    .bg(rgb(theme::tokens(self.theme).colors.overlay))
                                    .text_color(rgb(theme::tokens(self.theme).colors.text))
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.route == AppRoute::Terminal {
                                    this.navigate_to(AppRoute::Workspace, window, cx);
                                } else {
                                    this.toggle_terminal(&ToggleTerminal, window, cx);
                                }
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
                        panel.child(terminal.cached(terminal_style))
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
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .bg(rgb(theme::tokens(self.theme).colors.root))
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
                    .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                    .bg(rgb(theme::tokens(self.theme).colors.chrome))
                    .text_sm()
                    .text_color(rgb(theme::tokens(self.theme).colors.text))
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
                        self.theme,
                        cx.listener(|this, _, _, cx| this.commit_changes(cx)),
                    ))
                    .child(publish_button(
                        "push-branch",
                        "Push",
                        busy,
                        self.theme,
                        cx.listener(|this, _, _, cx| this.push_changes(cx)),
                    ))
                    .child(publish_button(
                        "create-pr",
                        "Create PR",
                        busy,
                        self.theme,
                        cx.listener(|this, _, _, cx| this.create_pr(cx)),
                    ))
                    .when_some(self.git_operation, |row, operation| {
                        row.child(
                            div()
                                .ml_auto()
                                .text_xs()
                                .text_color(rgb(theme::tokens(self.theme).colors.info))
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
                            rgb(theme::tokens(self.theme).colors.error)
                        } else {
                            rgb(theme::tokens(self.theme).colors.success)
                        })
                        .child(status),
                )
            })
    }

    fn render_diff(&self, width: Option<f32>, cx: &mut Context<Self>) -> Div {
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
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .bg(rgb(theme::tokens(self.theme).colors.panel))
                    .text_sm()
                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                    .child("No uncommitted diff"),
            );
        } else {
            for (file_index, file) in document.files.iter().enumerate() {
                files = files.child(self.render_diff_file(file_index, file));
            }
        }

        div()
            .when_some(width, |panel, width| panel.w(px(width)).flex_none())
            .when(width.is_none(), |panel| panel.flex_1().min_w_0())
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(theme::tokens(self.theme).colors.root))
            .border_l_1()
            .border_color(rgb(theme::tokens(self.theme).colors.border))
            .child(
                div()
                    .h(px(58.))
                    .px_4()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme::tokens(self.theme).colors.text))
                                    .child("Working tree"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(rgb(theme::tokens(self.theme).colors.raised))
                                    .text_xs()
                                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
                                    .child(format!("{} files", self.repo.changed_files)),
                            ),
                    )
                    .child(
                        tabs::tab_list()
                            .p_1()
                            .rounded_md()
                            .bg(rgb(theme::tokens(self.theme).colors.chrome))
                            .border_1()
                            .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
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
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .text_xs()
                    .whitespace_normal()
                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
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
            self.theme,
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
                    .bg(rgb(theme::tokens(self.theme).colors.panel))
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.border))
                    .text_xs()
                    .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
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
                            .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                            .child(format!("Modified · {}", file.new_path)),
                    ),
            );
        }

        for hunk in &file.hunks {
            let hidden = hunk.old_start.saturating_sub(previous_old_end);
            if hidden > 0 {
                body = body.child(render_unchanged_band(hidden, self.theme));
            }
            body = body.child(match self.diff_view {
                DiffViewMode::Stack => render_stack_hunk(hunk, self.theme),
                DiffViewMode::Split => render_split_hunk(hunk, self.theme),
            });
            previous_old_end = hunk.old_start.saturating_add(hunk.old_count);
        }

        if file.hunks.is_empty() {
            body = body.child(
                div()
                    .p_4()
                    .bg(rgb(theme::tokens(self.theme).colors.root))
                    .text_xs()
                    .text_color(rgb(theme::tokens(self.theme).colors.muted_text))
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
            .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
            .bg(rgb(theme::tokens(self.theme).colors.root))
            .child(
                div()
                    .h(px(42.))
                    .px_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(theme::tokens(self.theme).colors.panel))
                    .border_b_1()
                    .border_color(rgb(theme::tokens(self.theme).colors.strong_border))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(theme::tokens(self.theme).colors.faint_text))
                                    .text_xs()
                                    .child("▾"),
                            )
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_sm()
                                    .text_color(rgb(theme::tokens(self.theme).colors.text))
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
                                        .bg(rgb(theme::tokens(self.theme).colors.overlay))
                                        .text_xs()
                                        .text_color(rgb(theme::tokens(self.theme)
                                            .colors
                                            .muted_text))
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
                                    .text_color(rgb(theme::tokens(self.theme).colors.addition))
                                    .child(format!("+{}", file.additions)),
                            )
                            .child(
                                div()
                                    .text_color(rgb(theme::tokens(self.theme).colors.deletion))
                                    .child(format!("−{}", file.deletions)),
                            ),
                    ),
            )
            .child(body)
    }

    fn render_rail_item(
        &self,
        id: &'static str,
        glyph: &'static str,
        route: AppRoute,
        disabled: bool,
        tab_index: isize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let colors = theme::tokens(self.theme).colors;
        let selected = match (self.route, route) {
            (
                AppRoute::Settings(SettingsSection::Account),
                AppRoute::Settings(SettingsSection::Account),
            ) => true,
            (AppRoute::Settings(current), AppRoute::Settings(SettingsSection::Appearance)) => {
                current != SettingsSection::Account
            }
            (current, candidate) => current.same_surface(candidate),
        };
        div()
            .id(id)
            .key_context("Rail")
            .role(Role::Button)
            .aria_label(route.label())
            .tab_index(tab_index)
            .tab_stop(!disabled)
            .size(px(36.))
            .rounded_md()
            .flex()
            .items_center()
            .justify_center()
            .border_1()
            .border_color(rgb(if selected {
                colors.focus_ring
            } else {
                colors.chrome
            }))
            .bg(rgb(if selected {
                colors.accent_soft
            } else {
                colors.chrome
            }))
            .text_sm()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(rgb(if selected {
                colors.text
            } else {
                colors.muted_text
            }))
            .focus_visible(move |style| style.border_color(rgb(colors.focus_ring)))
            .when(!disabled, |item| {
                item.cursor_pointer()
                    .hover(move |style| style.bg(rgb(colors.overlay)))
                    .active(move |style| style.bg(rgb(colors.accent_soft)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.navigate_to(route, window, cx)),
                    )
                    .on_action(cx.listener(move |this, _: &ActivateRailItem, window, cx| {
                        this.navigate_to(route, window, cx)
                    }))
            })
            .when(disabled, |item| item.opacity(0.4))
            .child(glyph)
    }

    fn render_app_rail(&self, cx: &mut Context<Self>) -> Div {
        let colors = theme::tokens(self.theme).colors;
        let project_disabled = !self.repo.is_repository;
        div()
            .w(px(split_pane::RAIL_WIDTH))
            .h_full()
            .flex_none()
            .py_3()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .border_r_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.chrome))
            .child(
                div()
                    .size(px(36.))
                    .rounded_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(colors.accent))
                    .text_color(rgb(colors.on_accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("R"),
            )
            .child(div().h(px(8.)))
            .child(self.render_rail_item("rail-workspace", "W", AppRoute::Workspace, false, 1, cx))
            .child(self.render_rail_item(
                "rail-source-control",
                "G",
                AppRoute::SourceControl,
                project_disabled,
                2,
                cx,
            ))
            .child(self.render_rail_item(
                "rail-terminal",
                ">_",
                AppRoute::Terminal,
                project_disabled,
                3,
                cx,
            ))
            .child(self.render_rail_item(
                "rail-settings",
                "S",
                AppRoute::Settings(SettingsSection::Appearance),
                false,
                4,
                cx,
            ))
            .child(div().flex_1())
            .child(self.render_rail_item(
                "rail-account",
                "A",
                AppRoute::Settings(SettingsSection::Account),
                false,
                5,
                cx,
            ))
    }

    fn render_route_header(&self, title: &'static str, hint: &'static str) -> Div {
        let colors = theme::tokens(self.theme).colors;
        div()
            .h(px(58.))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.chrome))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(colors.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .text_color(rgb(colors.muted_text))
                            .child(self.project_name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(colors.faint_text))
                    .child(hint),
            )
    }

    fn render_workspace_route(&self, inspector_width: Option<f32>, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .overflow_hidden()
            .child(self.render_sidebar(self.panel_layout.sidebar_width, cx))
            .child(split_pane::divider(
                "workspace-sidebar-divider",
                self.active_split == Some(split_pane::SplitTarget::Sidebar),
                self.theme,
                cx.listener(|this, _, _, cx| {
                    this.active_split = Some(split_pane::SplitTarget::Sidebar);
                    cx.notify();
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.render_header(cx))
                    .child(self.render_messages(cx))
                    .when(self.show_terminal, |column| {
                        column.child(self.render_terminal(false, cx))
                    })
                    .child(self.render_composer(cx)),
            )
            .when_some(inspector_width.filter(|_| self.show_diff), |root, width| {
                root.child(split_pane::divider(
                    "workspace-inspector-divider",
                    self.active_split == Some(split_pane::SplitTarget::Inspector),
                    self.theme,
                    cx.listener(|this, _, _, cx| {
                        this.active_split = Some(split_pane::SplitTarget::Inspector);
                        cx.notify();
                    }),
                ))
                .child(self.render_diff(Some(width), cx))
            })
    }

    fn render_source_control_route(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.render_route_header("Source control", "Ctrl+2"))
            .child(self.render_diff(None, cx))
    }

    fn render_terminal_route(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.render_route_header("Terminal", "Ctrl+3"))
            .child(self.render_terminal(true, cx))
    }

    fn render_settings_route(&self, cx: &mut Context<Self>) -> Div {
        let colors = theme::tokens(self.theme).colors;
        let section = match self.route {
            AppRoute::Settings(section) => section,
            _ => SettingsSection::Appearance,
        };
        let section_list = div()
            .w(px(220.))
            .h_full()
            .flex_none()
            .p_3()
            .flex()
            .flex_col()
            .gap_1()
            .border_r_1()
            .border_color(rgb(colors.border))
            .bg(rgb(colors.panel))
            .children(SettingsSection::ALL.into_iter().map(|candidate| {
                let id = match candidate {
                    SettingsSection::Appearance => "settings-appearance",
                    SettingsSection::AgentsAndModels => "settings-agents",
                    SettingsSection::Terminal => "settings-terminal",
                    SettingsSection::GitAndWorktrees => "settings-git",
                    SettingsSection::Keybindings => "settings-keybindings",
                    SettingsSection::Account => "settings-account",
                };
                selectable_row::selectable_row(
                    id,
                    candidate.label(),
                    candidate == section,
                    false,
                    self.theme,
                    cx.listener(move |this, _, _, cx| this.set_settings_section(candidate, cx)),
                )
            }));

        let content = if section == SettingsSection::Appearance {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.text))
                        .child("Theme"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted_text))
                        .child("Choose Rode's native workspace palette."),
                )
                .child(
                    div().flex().flex_wrap().gap_3().children(
                        ThemeKind::ALL
                            .into_iter()
                            .enumerate()
                            .map(|(index, candidate)| {
                                let candidate_tokens = theme::tokens(candidate);
                                div()
                                    .id(("theme-choice", index))
                                    .role(Role::Button)
                                    .aria_label(format!("Use {} theme", candidate_tokens.name))
                                    .tab_index((index + 10) as isize)
                                    .tab_stop(true)
                                    .w(px(180.))
                                    .p_3()
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .border_1()
                                    .border_color(rgb(if self.theme == candidate {
                                        colors.focus_ring
                                    } else {
                                        colors.border
                                    }))
                                    .bg(rgb(candidate_tokens.colors.raised))
                                    .text_color(rgb(candidate_tokens.colors.text))
                                    .hover(move |style| style.border_color(rgb(colors.focus_ring)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_theme(candidate, cx)
                                    }))
                                    .child(
                                        div()
                                            .h(px(36.))
                                            .mb_3()
                                            .rounded_md()
                                            .bg(rgb(candidate_tokens.colors.accent)),
                                    )
                                    .child(candidate_tokens.name)
                            }),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .w_full()
                .p_5()
                .rounded_lg()
                .border_1()
                .border_color(rgb(colors.border))
                .bg(rgb(colors.raised))
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.text))
                        .child(section.label()),
                )
                .child(
                    div()
                        .mt_2()
                        .text_sm()
                        .text_color(rgb(colors.muted_text))
                        .child("This settings section is reserved for its milestone feature."),
                )
                .into_any_element()
        };

        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.render_route_header("Settings", "Ctrl+4"))
            .child(
                div().flex_1().min_h_0().flex().child(section_list).child(
                    div()
                        .id("settings-content-scroll")
                        .flex_1()
                        .min_w_0()
                        .overflow_y_scroll()
                        .p_6()
                        .bg(rgb(colors.root))
                        .child(content),
                ),
            )
    }
}

fn publish_button(
    id: &'static str,
    label: &'static str,
    busy: bool,
    theme_kind: ThemeKind,
    listener: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    button::button(
        id,
        label,
        button::ButtonStyle::Primary,
        busy,
        theme_kind,
        listener,
    )
}

fn render_unchanged_band(count: u32, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .h(px(25.))
        .px_2()
        .flex()
        .items_center()
        .bg(rgb(colors.overlay))
        .border_b_1()
        .border_color(rgb(colors.strong_border))
        .font_family("monospace")
        .text_xs()
        .text_color(rgb(colors.muted_text))
        .child(format!("▾ {count} unchanged lines"))
}

fn render_hunk_header(header: &str, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .h(px(25.))
        .px_2()
        .flex()
        .items_center()
        .bg(rgb(colors.overlay))
        .border_b_1()
        .border_color(rgb(colors.strong_border))
        .font_family("monospace")
        .text_xs()
        .text_color(rgb(colors.muted_text))
        .child(header.to_owned())
}

fn render_stack_hunk(hunk: &DiffHunk, theme_kind: ThemeKind) -> Div {
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .child(render_hunk_header(&hunk.header, theme_kind));
    for line in &hunk.lines {
        element = element.child(render_stack_line(line, theme_kind));
    }
    element
}

fn render_stack_line(line: &DiffLine, theme_kind: ThemeKind) -> Div {
    let (background, foreground, marker) = diff_line_style(line.kind, theme_kind);
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
        .child(render_line_number(line.old_line, theme_kind))
        .child(render_line_number(line.new_line, theme_kind))
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

fn render_split_hunk(hunk: &DiffHunk, theme_kind: ThemeKind) -> Div {
    let mut element = div()
        .w_full()
        .flex()
        .flex_col()
        .child(render_hunk_header(&hunk.header, theme_kind));
    for row in split_rows(hunk) {
        element = element.child(
            div()
                .min_w(px(680.))
                .h(px(21.))
                .flex()
                .child(render_split_cell(row.left.as_ref(), true, theme_kind))
                .child(render_split_cell(row.right.as_ref(), false, theme_kind)),
        );
    }
    element
}

fn render_split_cell(line: Option<&DiffLine>, left: bool, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    let (background, foreground, marker) = line
        .map(|line| diff_line_style(line.kind, theme_kind))
        .unwrap_or((colors.root, colors.faint_text, ""));
    let number = line.and_then(|line| if left { line.old_line } else { line.new_line });
    let text = line.map(|line| line.text.clone()).unwrap_or_default();

    div()
        .w_1_2()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .bg(rgb(background))
        .when(!left, |cell| {
            cell.border_l_1().border_color(rgb(colors.strong_border))
        })
        .font_family("monospace")
        .text_xs()
        .line_height(px(21.))
        .text_color(rgb(foreground))
        .child(render_line_number(number, theme_kind))
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

fn render_line_number(number: Option<u32>, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .w(px(42.))
        .h_full()
        .flex_none()
        .pr_2()
        .flex()
        .items_center()
        .justify_end()
        .bg(rgb(colors.panel))
        .border_r_1()
        .border_color(rgb(colors.border))
        .text_color(rgb(colors.faint_text))
        .child(number.map(|number| number.to_string()).unwrap_or_default())
}

fn diff_line_style(kind: DiffLineKind, theme_kind: ThemeKind) -> (u32, u32, &'static str) {
    let colors = theme::tokens(theme_kind).colors;
    match kind {
        DiffLineKind::Context => (colors.root, colors.text, " "),
        DiffLineKind::Addition => (colors.addition_soft, colors.text, "+"),
        DiffLineKind::Deletion => (colors.deletion_soft, colors.text, "−"),
        DiffLineKind::Meta => (colors.panel, colors.muted_text, ""),
    }
}

impl Drop for RodeApp {
    fn drop(&mut self) {
        if let Some(cancellation) = self.pending_codex_login.take() {
            let _ = cancellation.cancel();
        }
    }
}

impl Render for RodeApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::tokens(self.theme).colors;
        if self.codex_auth.requires_onboarding() {
            return self.render_auth_onboarding(cx);
        }
        if !self.project_open {
            return self.render_project_onboarding(cx);
        }
        if self.show_terminal || self.route == AppRoute::Terminal {
            self.ensure_terminal(window, cx);
        }
        let inspector_width = self
            .panel_layout
            .inspector_width_for_viewport(f32::from(window.viewport_size().width));
        let route = match self.route {
            AppRoute::Workspace => self
                .render_workspace_route(inspector_width, cx)
                .into_any_element(),
            AppRoute::SourceControl => self.render_source_control_route(cx).into_any_element(),
            AppRoute::Terminal => self.render_terminal_route(cx).into_any_element(),
            AppRoute::Settings(_) => self.render_settings_route(cx).into_any_element(),
            AppRoute::Login => self
                .render_workspace_route(inspector_width, cx)
                .into_any_element(),
        };
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
            .on_action(cx.listener(Self::open_workspace))
            .on_action(cx.listener(Self::open_source_control))
            .on_action(cx.listener(Self::open_terminal_route))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::cycle_theme))
            .on_mouse_move(cx.listener(Self::resize_panels))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::finish_panel_resize))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_panel_resize))
            .size_full()
            .min_w_0()
            .relative()
            .flex()
            .overflow_hidden()
            .bg(rgb(colors.root))
            .text_color(rgb(colors.text))
            .child(self.render_app_rail(cx))
            .child(route)
            .when_some(self.modal, |root, active_modal| {
                root.child(modal::modal_frame(
                    active_modal.title(),
                    div()
                        .text_sm()
                        .text_color(rgb(colors.muted_text))
                        .child("This dialog is ready for its feature-specific content."),
                    self.theme,
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
                    .children(
                        self.toasts
                            .iter()
                            .map(|notice| toast::toast(notice, self.theme)),
                    ),
            )
            .into_any_element()
    }
}

fn onboarding_status(label: &'static str, theme_kind: ThemeKind) -> gpui::AnyElement {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .w_full()
        .h(px(44.))
        .rounded_lg()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(colors.overlay))
        .text_sm()
        .text_color(rgb(colors.muted_text))
        .child(label)
        .into_any_element()
}

fn is_activation_key(event: &KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "enter" | "space")
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

fn select_startup_project(
    requested: Option<PathBuf>,
    recent: &[StoredProject],
    active_project_id: Option<&str>,
) -> (Option<PathBuf>, Option<String>) {
    if let Some(path) = requested {
        return (Some(path), None);
    }

    if let Some(active_project_id) = active_project_id {
        return recent
            .iter()
            .find(|project| project.id == active_project_id)
            .map(|project| (Some(project.path.clone()), None))
            .unwrap_or_else(|| {
                (
                    None,
                    Some(
                        "The last active project record is unavailable. Choose a project below."
                            .to_owned(),
                    ),
                )
            });
    }

    (recent.first().map(|project| project.path.clone()), None)
}

fn route_after_auth(
    current: AppRoute,
    last_authenticated: AppRoute,
    requires_onboarding: bool,
) -> AppRoute {
    if requires_onboarding {
        AppRoute::Login
    } else if current == AppRoute::Login {
        last_authenticated
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppRoute, CodexAccount, CodexAuthState, INSPECTOR_WIDTH_SETTING, ROUTE_SETTING,
        SIDEBAR_WIDTH_SETTING, SettingsSection, THEME_SETTING, UiPreferences, route_after_auth,
        select_startup_project,
    };
    use crate::persistence::{StateStore, StoredProject};
    use crate::theme::ThemeKind;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn workspace_is_only_available_after_codex_authentication() {
        assert!(CodexAuthState::Unavailable.requires_onboarding());
        assert!(CodexAuthState::Checking.requires_onboarding());
        assert!(CodexAuthState::SignedOut.requires_onboarding());
        assert!(CodexAuthState::SigningIn.requires_onboarding());
        assert!(
            CodexAuthState::BrowserPending {
                auth_url: "https://example.com".to_owned()
            }
            .requires_onboarding()
        );
        assert!(CodexAuthState::Cancelling.requires_onboarding());
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
    fn startup_restores_only_the_explicit_active_project() {
        let first = StoredProject::new("/tmp/first".into(), "First".to_owned());
        let active = StoredProject::new("/tmp/active".into(), "Active".to_owned());
        let selected =
            select_startup_project(None, &[first.clone(), active.clone()], Some(&active.id));
        assert_eq!(selected, (Some(active.path), None));

        let missing_identity = select_startup_project(None, &[first], Some("removed-id"));
        assert!(missing_identity.0.is_none());
        assert!(missing_identity.1.is_some());
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
    fn application_routes_round_trip_through_stable_names() {
        for route in [
            AppRoute::Workspace,
            AppRoute::SourceControl,
            AppRoute::Terminal,
            AppRoute::Settings(SettingsSection::Appearance),
        ] {
            assert!(AppRoute::from_storage_name(route.storage_name()).same_surface(route));
        }
        assert_eq!(AppRoute::from_storage_name("unknown"), AppRoute::Workspace);
    }

    #[test]
    fn authentication_routes_to_onboarding_without_clobbering_signed_in_navigation() {
        assert_eq!(
            route_after_auth(AppRoute::SourceControl, AppRoute::SourceControl, true),
            AppRoute::Login
        );
        assert_eq!(
            route_after_auth(AppRoute::Login, AppRoute::SourceControl, false),
            AppRoute::SourceControl
        );
        assert_eq!(
            route_after_auth(AppRoute::Terminal, AppRoute::Workspace, false),
            AppRoute::Terminal
        );
    }

    #[test]
    fn ui_preferences_restore_route_theme_and_bounded_panel_widths() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("rode-ui-preferences-{nonce}"));
        fs::create_dir_all(&root).expect("create settings fixture");
        let mut store = StateStore::open(&root.join("state.sqlite3")).expect("open settings store");
        store
            .save_string_setting(ROUTE_SETTING, AppRoute::Terminal.storage_name())
            .unwrap();
        store
            .save_string_setting(THEME_SETTING, ThemeKind::Daylight.storage_name())
            .unwrap();
        store
            .save_f32_setting(SIDEBAR_WIDTH_SETTING, 300.0)
            .unwrap();
        store
            .save_f32_setting(INSPECTOR_WIDTH_SETTING, 500.0)
            .unwrap();

        let preferences = UiPreferences::load(&store);
        assert_eq!(preferences.route, AppRoute::Terminal);
        assert_eq!(preferences.theme, ThemeKind::Daylight);
        assert_eq!(preferences.panels.sidebar_width, 300.0);
        assert_eq!(preferences.panels.inspector_width, 500.0);

        store
            .save_string_setting(SIDEBAR_WIDTH_SETTING, "NaN")
            .unwrap();
        assert!(UiPreferences::load(&store).panels.sidebar_width.is_finite());
        drop(store);
        fs::remove_dir_all(root).expect("clean settings fixture");
    }
}
