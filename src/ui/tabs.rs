use gpui::{App, ClickEvent, Div, IntoElement, Role, SharedString, Window, div, prelude::*, rgb};

use crate::theme::{self, ThemeKind};

pub(crate) fn tab_list() -> Div {
    div().flex().items_center().gap_1()
}

pub(crate) fn tab(
    id: &'static str,
    label: impl Into<SharedString>,
    selected: bool,
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
        .tab_stop(true)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(if selected {
            colors.accent_soft
        } else {
            colors.chrome
        }))
        .text_color(rgb(if selected {
            colors.text
        } else {
            colors.muted_text
        }))
        .hover(move |style| style.bg(rgb(colors.overlay)).text_color(rgb(colors.text)))
        .active(move |style| style.bg(rgb(colors.accent_soft)))
        .focus_visible(move |style| style.border_1().border_color(rgb(colors.focus_ring)))
        .on_click(listener)
        .child(label)
}
