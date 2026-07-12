use gpui::{App, IntoElement, MouseButton, MouseDownEvent, Window, div, prelude::*, px, rgb};

use crate::theme::{self, ThemeKind};

pub(crate) const RAIL_WIDTH: f32 = 52.0;
pub(crate) const DIVIDER_WIDTH: f32 = 5.0;
pub(crate) const MIN_CENTER_WIDTH: f32 = 560.0;
pub(crate) const MIN_SIDEBAR_WIDTH: f32 = 210.0;
pub(crate) const MAX_SIDEBAR_WIDTH: f32 = 420.0;
pub(crate) const MIN_INSPECTOR_WIDTH: f32 = 320.0;
pub(crate) const MAX_INSPECTOR_WIDTH: f32 = 720.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SplitTarget {
    Sidebar,
    Inspector,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanelLayout {
    pub sidebar_width: f32,
    pub inspector_width: f32,
}

impl Default for PanelLayout {
    fn default() -> Self {
        Self {
            sidebar_width: 228.0,
            inspector_width: 360.0,
        }
    }
}

impl PanelLayout {
    pub fn sanitized(self) -> Self {
        Self {
            sidebar_width: finite_clamp(
                self.sidebar_width,
                Self::default().sidebar_width,
                MIN_SIDEBAR_WIDTH,
                MAX_SIDEBAR_WIDTH,
            ),
            inspector_width: finite_clamp(
                self.inspector_width,
                Self::default().inspector_width,
                MIN_INSPECTOR_WIDTH,
                MAX_INSPECTOR_WIDTH,
            ),
        }
    }

    pub fn inspector_width_for_viewport(self, viewport_width: f32) -> Option<f32> {
        let available = viewport_width
            - RAIL_WIDTH
            - self.sidebar_width
            - MIN_CENTER_WIDTH
            - DIVIDER_WIDTH * 2.0;
        if available < MIN_INSPECTOR_WIDTH {
            None
        } else {
            Some(self.inspector_width.min(available).max(MIN_INSPECTOR_WIDTH))
        }
    }
}

fn finite_clamp(value: f32, default: f32, minimum: f32, maximum: f32) -> f32 {
    if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        default
    }
}

pub(crate) fn divider(
    id: &'static str,
    active: bool,
    theme_kind: ThemeKind,
    listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let colors = theme::tokens(theme_kind).colors;
    div()
        .id(id)
        .w(px(DIVIDER_WIDTH))
        .h_full()
        .flex_none()
        .cursor_col_resize()
        .bg(rgb(if active {
            colors.focus_ring
        } else {
            colors.border
        }))
        .hover(move |style| style.bg(rgb(colors.focus_ring)))
        .on_mouse_down(MouseButton::Left, listener)
}

#[cfg(test)]
mod tests {
    use super::{
        DIVIDER_WIDTH, MAX_INSPECTOR_WIDTH, MAX_SIDEBAR_WIDTH, MIN_CENTER_WIDTH,
        MIN_INSPECTOR_WIDTH, MIN_SIDEBAR_WIDTH, PanelLayout, RAIL_WIDTH,
    };

    #[test]
    fn panel_widths_are_clamped_and_inspector_yields_to_center_content() {
        let layout = PanelLayout {
            sidebar_width: 10.0,
            inspector_width: 5_000.0,
        }
        .sanitized();
        assert_eq!(layout.sidebar_width, MIN_SIDEBAR_WIDTH);
        assert_eq!(layout.inspector_width, MAX_INSPECTOR_WIDTH);
        assert!(layout.inspector_width_for_viewport(900.0).is_none());
        let wide = PanelLayout {
            sidebar_width: MAX_SIDEBAR_WIDTH,
            inspector_width: MAX_INSPECTOR_WIDTH,
        };
        assert!(
            wide.inspector_width_for_viewport(1_800.0)
                .is_some_and(|width| width >= MIN_INSPECTOR_WIDTH)
        );
        let screenshot_width = 1_348.0;
        let default_layout = PanelLayout::default();
        let inspector = default_layout
            .inspector_width_for_viewport(screenshot_width)
            .expect("inspector fits at the reported screenshot width");
        let center = screenshot_width
            - RAIL_WIDTH
            - default_layout.sidebar_width
            - inspector
            - DIVIDER_WIDTH * 2.0;
        assert!(center >= MIN_CENTER_WIDTH);
        assert!(center > inspector);
        let invalid = PanelLayout {
            sidebar_width: f32::NAN,
            inspector_width: f32::INFINITY,
        }
        .sanitized();
        assert_eq!(invalid, PanelLayout::default());
    }
}
