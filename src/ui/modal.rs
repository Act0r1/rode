use gpui::{IntoElement, SharedString, div, prelude::*, px, rgb, rgba};

use crate::theme::{self, ThemeKind};

pub(crate) fn modal_frame(
    title: impl Into<SharedString>,
    body: impl IntoElement,
    theme_kind: ThemeKind,
) -> impl IntoElement {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .id("modal-backdrop")
        .key_context("Modal")
        .tab_index(0)
        .tab_stop(true)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(colors.shadow))
        .child(
            div()
                .id("modal-surface")
                .w(px(480.))
                .max_w_full()
                .rounded_lg()
                .border_1()
                .border_color(rgb(colors.strong_border))
                .bg(rgb(colors.raised))
                .text_color(rgb(colors.text))
                .child(
                    div()
                        .h(px(48.))
                        .px_4()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(rgb(colors.border))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title.into()),
                )
                .child(div().p_4().child(body)),
        )
}
