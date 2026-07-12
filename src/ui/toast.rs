use std::collections::VecDeque;

use gpui::{App, ClickEvent, Div, Role, SharedString, Window, div, prelude::*, px, rgb};

use crate::theme::{self, ThemeKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: SharedString,
}

#[derive(Debug, Default)]
pub(crate) struct ToastQueue {
    next_id: u64,
    items: VecDeque<Toast>,
}

impl ToastQueue {
    pub fn push(&mut self, kind: ToastKind, message: impl Into<SharedString>) -> u64 {
        if self.items.len() >= 4
            && let Some(oldest) = self.items.front().map(|toast| toast.id)
        {
            self.dismiss(oldest);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.items.push_back(Toast {
            id,
            kind,
            message: message.into(),
        });
        id
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        let Some(index) = self.items.iter().position(|toast| toast.id == id) else {
            return false;
        };
        self.items.remove(index);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }
}

pub(crate) fn toast(
    toast: &Toast,
    theme_kind: ThemeKind,
    dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    let accent = match toast.kind {
        ToastKind::Info => colors.info,
        ToastKind::Success => colors.success,
        ToastKind::Warning => colors.warning,
        ToastKind::Error => colors.error,
    };
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .border_1()
        .border_color(rgb(accent))
        .bg(rgb(colors.raised))
        .text_color(rgb(colors.text))
        .flex()
        .items_start()
        .gap_2()
        .child(div().min_w_0().flex_1().child(toast.message.clone()))
        .child(
            div()
                .id(("dismiss-toast", toast.id))
                .role(Role::Button)
                .aria_label("Close notification")
                .flex_none()
                .w(px(24.))
                .h(px(24.))
                .rounded_sm()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .text_color(rgb(colors.muted_text))
                .hover(move |style| style.bg(rgb(colors.overlay)).text_color(rgb(colors.text)))
                .on_click(dismiss)
                .child("×"),
        )
}

#[cfg(test)]
mod tests {
    use super::{ToastKind, ToastQueue};

    #[test]
    fn queue_preserves_order_and_dismisses_by_stable_id() {
        let mut queue = ToastQueue::default();
        let first = queue.push(ToastKind::Info, "first");
        let second = queue.push(ToastKind::Error, "second");
        let third = queue.push(ToastKind::Success, "third");
        let fourth = queue.push(ToastKind::Warning, "fourth");
        assert_eq!(
            queue.iter().map(|toast| toast.id).collect::<Vec<_>>(),
            vec![first, second, third, fourth]
        );
        let fifth = queue.push(ToastKind::Info, "fifth");
        assert_eq!(
            queue.iter().map(|toast| toast.id).collect::<Vec<_>>(),
            vec![second, third, fourth, fifth]
        );
        assert!(queue.dismiss(second));
        assert_eq!(
            queue.iter().map(|toast| toast.id).collect::<Vec<_>>(),
            vec![third, fourth, fifth]
        );
        assert!(!queue.dismiss(first));
    }
}
