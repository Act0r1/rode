use crate::conversation::{
    CardKind, CardStatus, ConversationAttachment, ConversationCard, NoticeTone,
};
use crate::theme::{self, ThemeKind};
use gpui::{Div, ObjectFit, div, img, prelude::*, px, rgb};

pub(crate) fn card(card: &ConversationCard, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    let (label, background, border) = match &card.kind {
        CardKind::UserMessage { .. } => ("YOU", colors.accent_soft, colors.focus_ring),
        CardKind::AssistantMessage { .. } => ("CODEX", colors.panel, colors.border),
        CardKind::Reasoning { .. } => ("REASONING", colors.raised, colors.info),
        CardKind::Command { .. } => ("COMMAND", colors.raised, colors.strong_border),
        CardKind::FileChange { .. } => ("FILE CHANGE", colors.addition_soft, colors.success),
        CardKind::ToolResult { .. } => ("TOOL", colors.raised, colors.strong_border),
        CardKind::Approval { .. } => ("APPROVAL", colors.warning_soft, colors.warning),
        CardKind::Notice { tone, .. } => match tone {
            NoticeTone::Info => ("RODE", colors.accent_soft, colors.info),
            NoticeTone::Warning => ("WARNING", colors.warning_soft, colors.warning),
            NoticeTone::Error => ("ERROR", colors.deletion_soft, colors.error),
        },
        CardKind::TurnBoundary { .. } => ("TURN", colors.chrome, colors.border),
        CardKind::CancelledTurn { .. } => ("CANCELLED", colors.warning_soft, colors.warning),
    };
    let status_color = match card.status {
        CardStatus::Pending | CardStatus::Running => colors.info,
        CardStatus::Success => colors.success,
        CardStatus::Failed => colors.error,
        CardStatus::Cancelled => colors.warning,
        CardStatus::Complete => colors.muted_text,
    };
    let status = match card.status {
        CardStatus::Pending => "Pending",
        CardStatus::Running => "Running",
        CardStatus::Success => "Success",
        CardStatus::Failed => "Failed",
        CardStatus::Cancelled => "Cancelled",
        CardStatus::Complete => "Complete",
    };

    let image_paths = match &card.kind {
        CardKind::UserMessage { attachments, .. } => attachments
            .iter()
            .filter_map(|attachment| match attachment {
                ConversationAttachment::Image { path } => Some(path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };

    div()
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
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(rgb(colors.faint_text))
                        .child(label),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().size(px(7.)).rounded_full().bg(rgb(status_color)))
                        .child(div().text_xs().text_color(rgb(status_color)).child(status)),
                ),
        )
        .child(card_body(card, theme_kind))
        .children(image_paths.into_iter().enumerate().map(|(index, path)| {
            let open_path = path.clone();
            img(path)
                .id(format!("chat-image-{}-{index}", card.id))
                .role(gpui::Role::Button)
                .aria_label("Open image")
                .tab_index(0)
                .tab_stop(true)
                .cursor_pointer()
                .w_full()
                .max_h(px(320.))
                .rounded_md()
                .object_fit(ObjectFit::ScaleDown)
                .on_click(move |_, _, cx| cx.open_with_system(&open_path))
        }))
}

fn card_body(card: &ConversationCard, theme_kind: ThemeKind) -> Div {
    let colors = theme::tokens(theme_kind).colors;
    let body = match &card.kind {
        CardKind::UserMessage {
            text, attachments, ..
        } => format!(
            "{text}{}",
            if attachments.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nContext: {}",
                    attachments
                        .iter()
                        .map(ConversationAttachment::label)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ),
        CardKind::AssistantMessage { text } | CardKind::Reasoning { text } => text.clone(),
        CardKind::Command {
            command,
            cwd,
            output,
            exit_code,
            ..
        } => format!(
            "$ {command}\n{cwd}{}{}",
            exit_code
                .map(|code| format!("\nexit {code}"))
                .unwrap_or_default(),
            if output.is_empty() {
                String::new()
            } else {
                format!("\n\n{output}")
            }
        ),
        CardKind::FileChange { summary, .. } => summary.clone(),
        CardKind::ToolResult { title, output } => format!("{title}\n\n{output}"),
        CardKind::Approval { title, detail, .. } => format!("{title}\n{detail}"),
        CardKind::Notice { text, .. } => text.clone(),
        CardKind::TurnBoundary { label, detail } => detail
            .as_ref()
            .map(|detail| format!("{label}\n{detail}"))
            .unwrap_or_else(|| label.clone()),
        CardKind::CancelledTurn { detail } => detail.clone(),
    };
    let visible = if card.collapsed {
        body.lines().take(3).collect::<Vec<_>>().join("\n")
    } else {
        body
    };
    div()
        .w_full()
        .whitespace_normal()
        .line_height(px(21.))
        .text_sm()
        .font_family(
            if matches!(
                card.kind,
                CardKind::Command { .. } | CardKind::ToolResult { .. }
            ) {
                "monospace"
            } else {
                "system-ui"
            },
        )
        .text_color(rgb(colors.text))
        .child(visible)
}
