use gpui::{App, KeyBinding, actions};

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
        SubmitRename,
        CancelRename,
        DismissModal,
        OpenWorkspace,
        OpenSourceControl,
        OpenTerminalRoute,
        OpenSettings,
        CycleTheme,
        ActivateRailItem,
        ToggleTerminal,
        ToggleDiff,
        ToggleDiffLayout,
        RefreshRepo,
        Quit
    ]
);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ActionId {
    ComposerBackspace,
    ComposerDelete,
    ComposerLeft,
    ComposerRight,
    ComposerHome,
    ComposerEnd,
    ComposerInsertNewline,
    ComposerSend,
    RenameBackspace,
    RenameDelete,
    RenameLeft,
    RenameRight,
    RenameHome,
    RenameEnd,
    RenameSubmit,
    RenameCancel,
    DismissModal,
    OpenWorkspace,
    OpenSourceControl,
    OpenTerminalRoute,
    OpenSettings,
    CycleTheme,
    ActivateRailItem,
    ActivateRailItemSpace,
    ToggleTerminal,
    ToggleDiff,
    ToggleDiffLayout,
    RefreshRepository,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ActionDescriptor {
    pub id: ActionId,
    pub label: &'static str,
    pub shortcut: &'static str,
    pub context: Option<&'static str>,
}

pub(crate) const ACTION_REGISTRY: &[ActionDescriptor] = &[
    action(
        ActionId::ComposerBackspace,
        "Delete backward",
        "backspace",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerDelete,
        "Delete forward",
        "delete",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerLeft,
        "Move left",
        "left",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerRight,
        "Move right",
        "right",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerHome,
        "Move to line start",
        "home",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerEnd,
        "Move to line end",
        "end",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerInsertNewline,
        "Insert newline",
        "shift-enter",
        Some("Composer"),
    ),
    action(
        ActionId::ComposerSend,
        "Send prompt",
        "enter",
        Some("Composer"),
    ),
    action(
        ActionId::RenameBackspace,
        "Delete backward",
        "backspace",
        Some("Rename"),
    ),
    action(
        ActionId::RenameDelete,
        "Delete forward",
        "delete",
        Some("Rename"),
    ),
    action(ActionId::RenameLeft, "Move left", "left", Some("Rename")),
    action(ActionId::RenameRight, "Move right", "right", Some("Rename")),
    action(
        ActionId::RenameHome,
        "Move to line start",
        "home",
        Some("Rename"),
    ),
    action(
        ActionId::RenameEnd,
        "Move to line end",
        "end",
        Some("Rename"),
    ),
    action(
        ActionId::RenameSubmit,
        "Apply rename",
        "enter",
        Some("Rename"),
    ),
    action(
        ActionId::RenameCancel,
        "Cancel rename",
        "escape",
        Some("Rename"),
    ),
    action(
        ActionId::DismissModal,
        "Close dialog",
        "escape",
        Some("Modal"),
    ),
    action(ActionId::OpenWorkspace, "Open workspace", "ctrl-1", None),
    action(
        ActionId::OpenSourceControl,
        "Open source control",
        "ctrl-2",
        None,
    ),
    action(ActionId::OpenTerminalRoute, "Open terminal", "ctrl-3", None),
    action(ActionId::OpenSettings, "Open settings", "ctrl-4", None),
    action(ActionId::CycleTheme, "Cycle theme", "ctrl-shift-t", None),
    action(
        ActionId::ActivateRailItem,
        "Activate navigation item",
        "enter",
        Some("Rail"),
    ),
    action(
        ActionId::ActivateRailItemSpace,
        "Activate navigation item",
        "space",
        Some("Rail"),
    ),
    action(ActionId::ToggleTerminal, "Toggle terminal", "ctrl-j", None),
    action(ActionId::ToggleDiff, "Toggle diff", "ctrl-d", None),
    action(
        ActionId::ToggleDiffLayout,
        "Toggle diff layout",
        "ctrl-shift-d",
        None,
    ),
    action(
        ActionId::RefreshRepository,
        "Refresh repository",
        "ctrl-r",
        None,
    ),
    action(ActionId::Quit, "Quit Rode", "ctrl-q", None),
];

const fn action(
    id: ActionId,
    label: &'static str,
    shortcut: &'static str,
    context: Option<&'static str>,
) -> ActionDescriptor {
    ActionDescriptor {
        id,
        label,
        shortcut,
        context,
    }
}

pub(crate) fn register(cx: &mut App) {
    cx.bind_keys(ACTION_REGISTRY.iter().map(binding));
}

fn binding(action: &ActionDescriptor) -> KeyBinding {
    match action.id {
        ActionId::ComposerBackspace => KeyBinding::new(action.shortcut, Backspace, action.context),
        ActionId::ComposerDelete => KeyBinding::new(action.shortcut, Delete, action.context),
        ActionId::ComposerLeft => KeyBinding::new(action.shortcut, Left, action.context),
        ActionId::ComposerRight => KeyBinding::new(action.shortcut, Right, action.context),
        ActionId::ComposerHome => KeyBinding::new(action.shortcut, Home, action.context),
        ActionId::ComposerEnd => KeyBinding::new(action.shortcut, End, action.context),
        ActionId::ComposerInsertNewline => {
            KeyBinding::new(action.shortcut, InsertNewline, action.context)
        }
        ActionId::ComposerSend => KeyBinding::new(action.shortcut, SendPrompt, action.context),
        ActionId::RenameBackspace => KeyBinding::new(action.shortcut, Backspace, action.context),
        ActionId::RenameDelete => KeyBinding::new(action.shortcut, Delete, action.context),
        ActionId::RenameLeft => KeyBinding::new(action.shortcut, Left, action.context),
        ActionId::RenameRight => KeyBinding::new(action.shortcut, Right, action.context),
        ActionId::RenameHome => KeyBinding::new(action.shortcut, Home, action.context),
        ActionId::RenameEnd => KeyBinding::new(action.shortcut, End, action.context),
        ActionId::RenameSubmit => KeyBinding::new(action.shortcut, SubmitRename, action.context),
        ActionId::RenameCancel => KeyBinding::new(action.shortcut, CancelRename, action.context),
        ActionId::DismissModal => KeyBinding::new(action.shortcut, DismissModal, action.context),
        ActionId::OpenWorkspace => KeyBinding::new(action.shortcut, OpenWorkspace, action.context),
        ActionId::OpenSourceControl => {
            KeyBinding::new(action.shortcut, OpenSourceControl, action.context)
        }
        ActionId::OpenTerminalRoute => {
            KeyBinding::new(action.shortcut, OpenTerminalRoute, action.context)
        }
        ActionId::OpenSettings => KeyBinding::new(action.shortcut, OpenSettings, action.context),
        ActionId::CycleTheme => KeyBinding::new(action.shortcut, CycleTheme, action.context),
        ActionId::ActivateRailItem => {
            KeyBinding::new(action.shortcut, ActivateRailItem, action.context)
        }
        ActionId::ActivateRailItemSpace => {
            KeyBinding::new(action.shortcut, ActivateRailItem, action.context)
        }
        ActionId::ToggleTerminal => {
            KeyBinding::new(action.shortcut, ToggleTerminal, action.context)
        }
        ActionId::ToggleDiff => KeyBinding::new(action.shortcut, ToggleDiff, action.context),
        ActionId::ToggleDiffLayout => {
            KeyBinding::new(action.shortcut, ToggleDiffLayout, action.context)
        }
        ActionId::RefreshRepository => {
            KeyBinding::new(action.shortcut, RefreshRepo, action.context)
        }
        ActionId::Quit => KeyBinding::new(action.shortcut, Quit, action.context),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::ACTION_REGISTRY;

    #[test]
    fn action_ids_are_unique_and_metadata_is_complete() {
        let mut ids = HashSet::new();
        for action in ACTION_REGISTRY {
            assert!(
                ids.insert(action.id),
                "duplicate action id: {:?}",
                action.id
            );
            assert!(!action.label.is_empty());
            assert!(!action.shortcut.is_empty());
        }
    }
}
