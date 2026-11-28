// Derived from Zed's GPUI example `crates/gpui/examples/input.rs`
// (https://github.com/zed-industries/zed), and changed for this app: its
// state renamed, movement and deletion by word, the field's events, and
// the app's styling. What is Zed's stays under the Apache License 2.0
// (LICENSES/Apache-2.0.txt); the rest is under this project's (LICENSE).
//
// Copyright 2022 - 2025 Zed Industries, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A field of one line of text, for search: GPUI has none of its own, and
//! this is Zed's GPUI example (`examples/input.rs`) made the app's. It
//! takes typing through the system's input handler, so accents, dead keys
//! and input methods work as in any Mac field; it selects with the pointer
//! and Shift, and cuts, copies and pastes through the system's clipboard.
//! The arrows up and down, Return and Esc are not its own: it says so
//! (`FieldEvent`), for what it searches to act on.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InteractiveElement,
    IntoElement, KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, ParentElement, Pixels, Point, Render, ShapedLine, SharedString, Style, Styled,
    TextRun, UTF16Selection, UnderlineStyle, Window, actions, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::theme;

actions!(
    field,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        SelectHome,
        SelectEnd,
        DeleteWordLeft,
        DeleteToStart,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Up,
        Down,
        Enter,
        Escape,
    ]
);

/// The keys a field takes, as the system's own fields do (`platform::KEYS`).
pub(crate) fn bind_keys(cx: &mut App) {
    use crate::frame::{key, keys};
    let here = Some("Field");
    let system = crate::platform::KEYS;
    let mut bindings: Vec<KeyBinding> = vec![KeyBinding::new("backspace", Backspace, here)];
    bindings.extend(keys(system.delete_word_left, DeleteWordLeft, here));
    bindings.extend(keys(system.delete_to_start, DeleteToStart, here));
    bindings.extend([
        KeyBinding::new("delete", Delete, here),
        KeyBinding::new("left", Left, here),
        KeyBinding::new("right", Right, here),
    ]);
    bindings.extend(keys(system.word_left, WordLeft, here));
    bindings.extend(keys(system.word_right, WordRight, here));
    bindings.extend([
        KeyBinding::new("shift-left", SelectLeft, here),
        KeyBinding::new("shift-right", SelectRight, here),
    ]);
    bindings.extend(keys(system.select_word_left, SelectWordLeft, here));
    bindings.extend(keys(system.select_word_right, SelectWordRight, here));
    bindings.extend(key("secondary-a", SelectAll, here, cx));
    bindings.extend([
        KeyBinding::new("home", Home, here),
        KeyBinding::new("end", End, here),
    ]);
    bindings.extend(keys(system.line_start, Home, here));
    bindings.extend(keys(system.line_end, End, here));
    bindings.extend(keys(system.select_line_start, SelectHome, here));
    bindings.extend(keys(system.select_line_end, SelectEnd, here));
    bindings.extend(key("secondary-v", Paste, here, cx));
    bindings.extend(key("secondary-c", Copy, here, cx));
    bindings.extend(key("secondary-x", Cut, here, cx));
    bindings.extend(keys(system.character_palette, ShowCharacterPalette, here));
    bindings.extend([
        KeyBinding::new("up", Up, here),
        KeyBinding::new("down", Down, here),
        KeyBinding::new("enter", Enter, here),
        KeyBinding::new("escape", Escape, here),
    ]);
    cx.bind_keys(bindings);
}

/// What a field tells what it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FieldEvent {
    /// The text is now this.
    Changed(SharedString),
    /// The arrows: up is -1, down 1.
    Moved(i32),
    Entered,
    Escaped,
}

pub(crate) struct Field {
    focus: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_line: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
}

impl EventEmitter<FieldEvent> for Field {}

impl Focusable for Field {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Field {
    pub(crate) fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            selected: 0..0,
            reversed: false,
            marked: None,
            last_line: None,
            last_bounds: None,
            selecting: false,
        }
    }

    pub(crate) fn focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    /// Takes the keys, with what is in it selected, as ⌘F does.
    pub(crate) fn focus_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify();
    }

    /// What is typed in it.
    pub(crate) fn text(&self) -> &str {
        &self.content
    }

    /// Puts `text` in it, as if typed, without saying so: for a field that
    /// opens on what it is to change.
    pub(crate) fn set(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = text.len()..text.len();
        self.content = text.into();
        self.reversed = false;
        self.marked = None;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    /// Empties it, and says so.
    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.content.is_empty() {
            return;
        }
        self.content = SharedString::default();
        self.selected = 0..0;
        self.reversed = false;
        self.marked = None;
        window.notify_text_input_changed(&self.focus);
        cx.emit(FieldEvent::Changed(self.content.clone()));
        cx.notify();
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Changed(self.content.clone()));
        cx.notify();
    }

    fn left(&mut self, _: &Left, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.previous_boundary(self.cursor()), window, cx);
        } else {
            self.move_to(self.selected.start, window, cx);
        }
    }

    fn right(&mut self, _: &Right, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.move_to(self.next_boundary(self.selected.end), window, cx);
        } else {
            self.move_to(self.selected.end, window, cx);
        }
    }

    fn word_left(&mut self, _: &WordLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor()), window, cx);
    }

    fn word_right(&mut self, _: &WordRight, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor()), window, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor()), window, cx);
    }

    fn select_right(&mut self, _: &SelectRight, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor()), window, cx);
    }

    fn select_word_left(
        &mut self,
        _: &SelectWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.previous_word(self.cursor()), window, cx);
    }

    fn select_word_right(
        &mut self,
        _: &SelectWordRight,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_to(self.next_word(self.cursor()), window, cx);
    }

    fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
        self.select_to(self.content.len(), window, cx);
    }

    fn home(&mut self, _: &Home, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, window, cx);
    }

    fn end(&mut self, _: &End, window: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), window, cx);
    }

    fn select_home(&mut self, _: &SelectHome, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, window, cx);
    }

    fn select_end(&mut self, _: &SelectEnd, window: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), window, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let previous = self.previous_boundary(self.cursor());
            if previous == self.cursor() {
                return;
            }
            self.select_to(previous, window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.is_empty() {
            self.select_to(self.previous_word(self.cursor()), window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_to_start(&mut self, _: &DeleteToStart, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            self.select_to(0, window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let next = self.next_boundary(self.cursor());
            if next == self.cursor() {
                return;
            }
            self.select_to(next, window, cx);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Moved(-1));
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Moved(1));
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Entered);
    }

    fn escape(&mut self, _: &Escape, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(FieldEvent::Escaped);
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selecting = true;
        window.focus(&self.focus, cx);
        let at = self.index_for(event.position);
        if event.modifiers.shift {
            self.select_to(at, window, cx);
        } else if event.click_count >= 2 {
            // A double click selects all of it, as the field is short.
            self.selected = 0..self.content.len();
            cx.notify();
        } else {
            self.move_to(at, window, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selecting {
            self.select_to(self.index_for(event.position), window, cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace('\n', " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .content
            .get(self.selected.clone())
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .content
            .get(self.selected.clone())
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn move_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn index_for(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_line.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for character in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += character.len_utf16();
            utf8 += character.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16 = 0;
        let mut utf8 = 0;
        for character in self.content.chars() {
            if utf8 >= offset {
                break;
            }
            utf8 += character.len_utf8();
            utf16 += character.len_utf16();
        }
        utf16
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(at, _)| (at < offset).then_some(at))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(at, _)| (at > offset).then_some(at))
            .unwrap_or(self.content.len())
    }

    fn previous_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .rev()
            .find_map(|(at, _)| (at < offset).then_some(at))
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .unicode_word_indices()
            .find_map(|(at, word)| (at + word.len() > offset).then_some(at + word.len()))
            .unwrap_or(self.content.len())
    }

    /// The text with `new` in place of `range`, and where the cursor goes.
    fn spliced(&self, range: &Range<usize>, new: &str) -> SharedString {
        let before = self.content.get(..range.start).unwrap_or_default();
        let after = self.content.get(range.end..).unwrap_or_default();
        format!("{before}{new}{after}").into()
    }
}

impl EntityInputHandler for Field {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        self.content.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected),
            reversed: self.reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = self.range_from_utf16(&range_utf16);
        self.reversed = false;
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }

    fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        Some(self.content.encode_utf16().count())
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.content = self.spliced(&range, new_text);
        let at = range.start + new_text.len();
        self.selected = at..at;
        self.reversed = false;
        self.marked = None;
        window.notify_text_input_changed(&self.focus);
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range| self.range_from_utf16(range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.content = self.spliced(&range, new_text);
        self.marked = (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        let start_utf16 = self.offset_to_utf16(range.start);
        let length_utf16 = new_text.encode_utf16().count();
        self.selected = new_selected_range_utf16
            .map(|selected| {
                self.range_from_utf16(
                    &(start_utf16 + selected.start.min(length_utf16)
                        ..start_utf16 + selected.end.min(length_utf16)),
                )
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.reversed = false;
        window.notify_text_input_changed(&self.focus);
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_line.as_ref()?;
        if line.text != self.content {
            return None;
        }
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn selection_bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range_utf16, bounds, window, cx)
            .into_iter()
            .collect()
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let local = self.last_bounds?.localize(&point)?;
        let line = self.last_line.as_ref()?;
        if line.text != self.content {
            return None;
        }
        Some(self.offset_to_utf16(line.closest_index_for_x(local.x)))
    }
}

/// The line of text itself, with its caret and selection.
struct Text {
    field: gpui::Entity<Field>,
}

struct Prepainted {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl gpui::Element for Text {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let field = self.field.read(cx);
        let content = field.content.clone();
        let selected = field.selected.clone();
        let cursor = field.cursor();
        let style = window.text_style();
        let (shown, color) = if content.is_empty() {
            (field.placeholder.clone(), theme::faint().into())
        } else {
            (content, theme::ink().into())
        };
        let run = TextRun {
            len: shown.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match field.marked.as_ref() {
            Some(marked) => [
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: shown.len().saturating_sub(marked.end),
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        let size_px = style.font_size.to_pixels(window.rem_size());
        let line = window.text_system().shape_line(shown, size_px, &runs, None);
        let (selection, cursor) = if selected.is_empty() || field.content.is_empty() {
            let x = if field.content.is_empty() {
                px(0.)
            } else {
                line.x_for_index(cursor)
            };
            // The caret, a point and a half wide, in the ink.
            let height = size_px * 1.2;
            let top = bounds.top() + (bounds.size.height - height) / 2.;
            (
                None,
                Some(fill(
                    Bounds::new(point(bounds.left() + x, top), size(px(1.5), height)),
                    theme::ink(),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(selected.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(selected.end),
                            bounds.bottom(),
                        ),
                    ),
                    gpui::Rgba {
                        a: 0.35,
                        ..theme::acc()
                    },
                )),
                None,
            )
        };
        Prepainted {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepainted: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        if let Some(selection) = prepainted.selection.take() {
            window.paint_quad(selection);
        }
        let Some(line) = prepainted.line.take() else {
            return;
        };
        let _ = line.paint(
            bounds.origin,
            window.line_height(),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        );
        if focus.is_focused(window)
            && let Some(cursor) = prepainted.cursor.take()
        {
            window.paint_quad(cursor);
        }
        self.field.update(cx, |field, _| {
            field.last_line = Some(line);
            field.last_bounds = Some(bounds);
        });
    }
}

impl Render for Field {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .overflow_hidden()
            .key_context("Field")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_to_start))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(Self::escape))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .child(Text { field: cx.entity() })
    }
}
