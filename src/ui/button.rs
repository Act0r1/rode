use gpui::{App, ClickEvent, IntoElement, Role, SharedString, Window, div, prelude::*, px, rgb};

use crate::theme;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ButtonStyle {
    #[default]
    Primary,
    Secondary,
    Destructive,
}

pub(crate) fn button(
    id: &'static str,
    label: impl Into<SharedString>,
    style: ButtonStyle,
    disabled: bool,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let colors = &theme::current().colors;
    let label = label.into();
    let (background, foreground, hover) = match style {
        ButtonStyle::Primary => (colors.accent, colors.text, colors.accent_hover),
        ButtonStyle::Secondary => (colors.overlay, colors.text, colors.strong_border),
        ButtonStyle::Destructive => (colors.deletion_soft, colors.text, colors.deletion),
    };

    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .h(px(30.))
        .px_3()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(background))
        .text_sm()
        .text_color(rgb(foreground))
        .when(!disabled, |element| {
            element
                .cursor_pointer()
                .hover(move |style| style.bg(rgb(hover)))
                .on_click(listener)
        })
        .when(disabled, |element| element.opacity(0.5))
        .child(label)
}
