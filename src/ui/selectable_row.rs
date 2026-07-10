use gpui::{App, ClickEvent, IntoElement, Role, SharedString, Window, div, prelude::*, rgb};

use crate::theme::{self, ThemeKind};

pub(crate) fn selectable_row(
    id: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
    disabled: bool,
    theme_kind: ThemeKind,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let colors = theme::tokens(theme_kind).colors;
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .tab_index(0)
        .tab_stop(!disabled)
        .w_full()
        .px_3()
        .py_2()
        .rounded_md()
        .flex()
        .items_center()
        .bg(rgb(if selected {
            colors.accent_soft
        } else {
            colors.panel
        }))
        .border_1()
        .border_color(rgb(if selected {
            colors.focus_ring
        } else {
            colors.border
        }))
        .text_color(rgb(if disabled {
            colors.faint_text
        } else {
            colors.text
        }))
        .when(!disabled, |row| {
            row.cursor_pointer()
                .hover(move |style| style.bg(rgb(colors.overlay)))
                .active(move |style| style.bg(rgb(colors.accent_soft)))
                .focus_visible(move |style| style.border_color(rgb(colors.focus_ring)))
                .on_click(listener)
        })
        .when(disabled, |row| row.opacity(0.6))
        .child(label)
}
