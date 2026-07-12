// Adapted from Zed's GPUI `view_example` editor (Apache-2.0).

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Bounds, Context, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable,
    InteractiveElement, LayoutId, PaintQuad, Pixels, ShapedLine, SharedString, Subscription, Task,
    TextRun, UTF16Selection, Window, fill, hsla, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::actions::{
    Backspace, Delete, DeleteWordBackward, DeleteWordForward, End, Home, InsertNewline, Left,
    Right, SelectAll,
};

#[derive(Clone, Copy, Debug)]
pub enum EditorEvent {
    Changed,
}

impl gpui::EventEmitter<EditorEvent> for Editor {}

pub struct Editor {
    pub focus_handle: FocusHandle,
    value: String,
    cursor: usize,
    selection: Option<Range<usize>>,
    cursor_visible: bool,
    placeholder: SharedString,
    blink_task: Task<()>,
    subscriptions: Vec<Subscription>,
}

impl Editor {
    pub fn new(
        text: impl Into<String>,
        placeholder: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_subscription = cx.on_focus(&focus_handle, window, |this, _, cx| {
            this.start_blink(cx);
        });
        let blur_subscription = cx.on_blur(&focus_handle, window, |this, _, cx| {
            this.stop_blink(cx);
        });
        let value = text.into();
        let cursor = value.len();

        Self {
            focus_handle,
            value,
            cursor,
            selection: None,
            cursor_visible: false,
            placeholder: placeholder.into(),
            blink_task: Task::ready(()),
            subscriptions: vec![focus_subscription, blur_subscription],
        }
    }

    pub fn text(&self) -> String {
        self.value.clone()
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.value.clear();
        self.cursor = 0;
        self.selection = None;
        self.reset_blink(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    pub fn set_text(&mut self, value: impl Into<String>, cx: &mut Context<Self>) {
        self.value = value.into();
        self.cursor = self.value.len();
        self.selection = None;
        self.reset_blink(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn start_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.blink_task = Self::spawn_blink_task(cx);
    }

    fn stop_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = false;
        self.blink_task = Task::ready(());
        cx.notify();
    }

    fn spawn_blink_task(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                if this
                    .update(cx, |editor, cx| {
                        editor.cursor_visible = !editor.cursor_visible;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    fn reset_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.blink_task = Self::spawn_blink_task(cx);
    }

    pub fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection.take() {
            self.cursor = selection.start;
        } else if self.cursor > 0 {
            self.cursor = previous_boundary(&self.value, self.cursor);
        }
        self.reset_blink(cx);
        cx.notify();
    }

    pub fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(selection) = self.selection.take() {
            self.cursor = selection.end;
        } else if self.cursor < self.value.len() {
            self.cursor = next_boundary(&self.value, self.cursor);
        }
        self.reset_blink(cx);
        cx.notify();
    }

    pub fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = None;
        self.cursor = line_start(&self.value, self.cursor);
        self.reset_blink(cx);
        cx.notify();
    }

    pub fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.selection = None;
        self.cursor = line_end(&self.value, self.cursor);
        self.reset_blink(cx);
        cx.notify();
    }

    pub fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let mut changed = self.delete_selection();
        if !changed && self.cursor > 0 {
            let previous = previous_boundary(&self.value, self.cursor);
            self.value.drain(previous..self.cursor);
            self.cursor = previous;
            changed = true;
        }
        self.reset_blink(cx);
        if changed {
            cx.emit(EditorEvent::Changed);
        }
        cx.notify();
    }

    pub fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        let mut changed = self.delete_selection();
        if !changed && self.cursor < self.value.len() {
            let next = next_boundary(&self.value, self.cursor);
            self.value.drain(self.cursor..next);
            changed = true;
        }
        self.reset_blink(cx);
        if changed {
            cx.emit(EditorEvent::Changed);
        }
        cx.notify();
    }

    pub fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        if !self.value.is_empty() {
            self.selection = Some(0..self.value.len());
            self.cursor = self.value.len();
        }
        self.reset_blink(cx);
        cx.notify();
    }

    pub fn delete_word_backward(
        &mut self,
        _: &DeleteWordBackward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = self.delete_selection();
        if !changed && self.cursor > 0 {
            let start = previous_word_boundary(&self.value, self.cursor);
            self.value.drain(start..self.cursor);
            self.cursor = start;
            changed = true;
        }
        self.finish_edit(changed, cx);
    }

    pub fn delete_word_forward(
        &mut self,
        _: &DeleteWordForward,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut changed = self.delete_selection();
        if !changed && self.cursor < self.value.len() {
            let end = next_word_boundary(&self.value, self.cursor);
            self.value.drain(self.cursor..end);
            changed = true;
        }
        self.finish_edit(changed, cx);
    }

    fn delete_selection(&mut self) -> bool {
        let Some(selection) = self.selection.take() else {
            return false;
        };
        self.value.drain(selection.clone());
        self.cursor = selection.start;
        true
    }

    fn finish_edit(&mut self, changed: bool, cx: &mut Context<Self>) {
        self.reset_blink(cx);
        if changed {
            cx.emit(EditorEvent::Changed);
        }
        cx.notify();
    }

    pub fn insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
        self.delete_selection();
        self.value.insert(self.cursor, '\n');
        self.cursor += 1;
        self.reset_blink(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }
}

fn previous_word_boundary(content: &str, offset: usize) -> usize {
    let before = &content[..offset];
    let trimmed = before.trim_end_matches(char::is_whitespace);
    trimmed
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            character
                .is_whitespace()
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0)
}

fn next_word_boundary(content: &str, offset: usize) -> usize {
    let after = &content[offset..];
    let word_end = after
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(after.len());
    offset
        + word_end
        + after[word_end..]
            .chars()
            .take_while(|character| character.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
}

fn previous_boundary(content: &str, offset: usize) -> usize {
    content
        .grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

fn next_boundary(content: &str, offset: usize) -> usize {
    content
        .grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(content.len())
}

fn line_start(content: &str, offset: usize) -> usize {
    content[..offset].rfind('\n').map_or(0, |index| index + 1)
}

fn line_end(content: &str, offset: usize) -> usize {
    content[offset..]
        .find('\n')
        .map_or(content.len(), |index| offset + index)
}

fn offset_from_utf16(content: &str, offset: usize) -> usize {
    content
        .chars()
        .scan((0usize, 0usize), |(utf8, utf16), character| {
            let before = (*utf8, *utf16);
            *utf8 += character.len_utf8();
            *utf16 += character.len_utf16();
            Some(before)
        })
        .find_map(|(utf8, utf16)| (utf16 >= offset).then_some(utf8))
        .unwrap_or(content.len())
}

fn offset_to_utf16(content: &str, offset: usize) -> usize {
    content[..offset].encode_utf16().count()
}

fn range_to_utf16(content: &str, range: &Range<usize>) -> Range<usize> {
    offset_to_utf16(content, range.start)..offset_to_utf16(content, range.end)
}

fn range_from_utf16(content: &str, range: &Range<usize>) -> Range<usize> {
    offset_from_utf16(content, range.start)..offset_from_utf16(content, range.end)
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = range_from_utf16(&self.value, &range);
        actual_range.replace(range_to_utf16(&self.value, &range));
        Some(self.value[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selection = self.selection.clone().unwrap_or(self.cursor..self.cursor);
        Some(UTF16Selection {
            range: range_to_utf16(&self.value, &selection),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .as_ref()
            .map(|range| range_from_utf16(&self.value, range))
            .unwrap_or_else(|| self.selection.clone().unwrap_or(self.cursor..self.cursor));
        self.value.replace_range(range.clone(), new_text);
        self.cursor = range.start + new_text.len();
        self.selection = None;
        self.reset_blink(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range(range, new_text, window, cx);
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl gpui::Render for Editor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        EditorText {
            editor: cx.entity(),
        }
    }
}

struct EditorText {
    editor: Entity<Editor>,
}

struct EditorTextPrepaint {
    lines: Vec<ShapedLine>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for EditorText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorText {
    type RequestLayoutState = ();
    type PrepaintState = EditorTextPrepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.editor.read(cx).value.split('\n').count().max(1);
        let mut style = gpui::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = (window.line_height() * line_count as f32).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let editor = self.editor.read(cx);
        let content = editor.value.clone();
        let cursor_offset = editor.cursor;
        let selection = editor.selection.clone();
        let cursor_visible = editor.cursor_visible;
        let is_focused = editor.focus_handle.is_focused(window);
        let placeholder = editor.placeholder.clone();
        let style = window.text_style();
        let text_color = style.color;
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let is_placeholder = content.is_empty();

        let lines = if is_placeholder {
            let run = TextRun {
                len: placeholder.len(),
                font: style.font(),
                color: hsla(0., 0., 0.62, 1.),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            vec![
                window
                    .text_system()
                    .shape_line(placeholder, font_size, &[run], None),
            ]
        } else {
            content
                .split('\n')
                .map(|line| {
                    let text: SharedString = line.to_owned().into();
                    let run = TextRun {
                        len: text.len(),
                        font: style.font(),
                        color: text_color,
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    window
                        .text_system()
                        .shape_line(text, font_size, &[run], None)
                })
                .collect()
        };

        let selections = if is_focused {
            selection.as_ref().map_or_else(Vec::new, |selection| {
                selection_quads(&content, &lines, selection.clone(), bounds, line_height)
            })
        } else {
            Vec::new()
        };
        let cursor = if is_focused && cursor_visible && selection.is_none() {
            let (cursor_line, offset) = cursor_line_and_offset(&content, cursor_offset);
            let x = if is_placeholder {
                px(0.)
            } else {
                lines[cursor_line].x_for_index(offset)
            };
            Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + x,
                        bounds.top() + line_height * cursor_line as f32,
                    ),
                    size(px(1.5), line_height),
                ),
                text_color,
            ))
        } else {
            None
        };

        EditorTextPrepaint {
            lines,
            selections,
            cursor,
        }
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.editor.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.editor.clone()),
            cx,
        );

        let line_height = window.line_height();
        for selection in prepaint.selections.drain(..) {
            window.paint_quad(selection);
        }
        for (index, line) in prepaint.lines.iter().enumerate() {
            line.paint(
                point(bounds.left(), bounds.top() + line_height * index as f32),
                line_height,
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            )
            .expect("painting shaped editor text");
        }
        if let Some(cursor) = prepaint.cursor.take() {
            window.paint_quad(cursor);
        }
    }
}

fn selection_quads(
    content: &str,
    lines: &[ShapedLine],
    selection: Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<PaintQuad> {
    let (start_line, start_offset) = cursor_line_and_offset(content, selection.start);
    let (end_line, end_offset) = cursor_line_and_offset(content, selection.end);
    (start_line..=end_line)
        .map(|line_index| {
            let start = if line_index == start_line {
                start_offset
            } else {
                0
            };
            let end = if line_index == end_line {
                end_offset
            } else {
                lines[line_index].len()
            };
            let left = lines[line_index].x_for_index(start);
            let right = lines[line_index].x_for_index(end);
            fill(
                Bounds::new(
                    point(
                        bounds.left() + left,
                        bounds.top() + line_height * line_index as f32,
                    ),
                    size((right - left).max(px(1.)), line_height),
                ),
                hsla(0.58, 0.75, 0.45, 0.35),
            )
        })
        .collect()
}

fn cursor_line_and_offset(content: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut line_start = 0;
    for (index, character) in content.char_indices() {
        if index >= cursor {
            break;
        }
        if character == '\n' {
            line += 1;
            line_start = index + 1;
        }
    }
    (line, cursor - line_start)
}

#[cfg(test)]
mod tests {
    use super::{next_word_boundary, previous_word_boundary};

    #[test]
    fn word_boundaries_follow_linux_style_deletion() {
        let text = "alpha  beta gamma";
        assert_eq!(previous_word_boundary(text, text.len()), 12);
        assert_eq!(previous_word_boundary(text, 12), 7);
        assert_eq!(next_word_boundary(text, 0), 7);
        assert_eq!(next_word_boundary(text, 7), 12);
    }

    #[test]
    fn word_boundaries_are_valid_for_unicode() {
        let text = "hello  мир  世界";
        let world_start = text.find('世').expect("world start");
        assert_eq!(previous_word_boundary(text, text.len()), world_start);
        assert_eq!(
            next_word_boundary(text, 0),
            text.find('м').expect("Cyrillic start")
        );
    }
}

pub fn standard_actions<E: InteractiveElement>(editor: Entity<Editor>) -> impl FnOnce(E) -> E {
    move |element| {
        element
            .on_action({
                let editor = editor.clone();
                move |action: &Left, window, cx| {
                    editor.update(cx, |editor, cx| editor.left(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &Right, window, cx| {
                    editor.update(cx, |editor, cx| editor.right(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &Home, window, cx| {
                    editor.update(cx, |editor, cx| editor.home(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &End, window, cx| {
                    editor.update(cx, |editor, cx| editor.end(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &Backspace, window, cx| {
                    editor.update(cx, |editor, cx| editor.backspace(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &Delete, window, cx| {
                    editor.update(cx, |editor, cx| editor.delete(action, window, cx))
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &DeleteWordBackward, window, cx| {
                    editor.update(cx, |editor, cx| {
                        editor.delete_word_backward(action, window, cx)
                    })
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &DeleteWordForward, window, cx| {
                    editor.update(cx, |editor, cx| {
                        editor.delete_word_forward(action, window, cx)
                    })
                }
            })
            .on_action({
                let editor = editor.clone();
                move |action: &SelectAll, window, cx| {
                    editor.update(cx, |editor, cx| editor.select_all(action, window, cx))
                }
            })
            .on_action(move |action: &InsertNewline, window, cx| {
                editor.update(cx, |editor, cx| editor.insert_newline(action, window, cx))
            })
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        // Make the ownership of platform subscriptions explicit and silence a
        // false-positive unused-field warning while retaining them for life.
        self.subscriptions.clear();
    }
}
