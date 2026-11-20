//! The menu the app draws itself, where the system has none to pop up or
//! no menu bar to show the app's menus in (`platform::CHROME.menu_bar`):
//! the context menus, through `platform::pop_up_menu`, and the app's menus
//! from the title bar's button (`app_menus`). It behaves as a system's
//! menu: ↑ and ↓ move through it, → or Return opens a submenu and ← or Esc
//! closes it, Return or a click chooses, and a press outside it, Esc or the
//! window losing focus puts it away. While it is open it has the keys, and
//! nothing behind it takes the pointer.
//!
//! It is drawn over the window it is in by that window's root, which puts
//! `layer` last among its children.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::oneshot;
use gpui::{
    Action, Anchor, AnyElement, AnyWindowHandle, App, AppContext, Bounds, Context, Entity,
    FocusHandle, FontWeight, Global, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    OwnedMenuItem, ParentElement, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, anchored, canvas, deferred, div,
    point, prelude::FluentBuilder, px, size,
};

use crate::icons::icon;
use crate::platform::{self, MenuItem};
use crate::theme;

/// One line of a drawn menu: the system's `MenuItem`, with the key that
/// does the same, as a menu bar shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Separator,
    Action {
        title: SharedString,
        enabled: bool,
        checked: bool,
        key: Option<SharedString>,
    },
    Submenu {
        title: SharedString,
        items: Vec<Item>,
    },
}

impl From<MenuItem> for Item {
    fn from(item: MenuItem) -> Self {
        match item {
            MenuItem::Separator => Self::Separator,
            MenuItem::Action {
                title,
                enabled,
                checked,
            } => Self::Action {
                title: title.into(),
                enabled,
                checked,
                key: None,
            },
            MenuItem::Submenu { title, items } => Self::Submenu {
                title: title.into(),
                items: items.into_iter().map(Self::from).collect(),
            },
        }
    }
}

impl Item {
    /// Whether the pointer and the arrows stop on it.
    fn lights(&self) -> bool {
        match self {
            Self::Separator => false,
            Self::Action { enabled, .. } => *enabled,
            Self::Submenu { items, .. } => !items.is_empty(),
        }
    }

    /// How many actions it numbers.
    fn actions(&self) -> usize {
        match self {
            Self::Separator => 0,
            Self::Action { .. } => 1,
            Self::Submenu { items, .. } => items.iter().map(Self::actions).sum(),
        }
    }
}

/// A row's height, a separator's, and the padding round a menu's rows.
const ROW: f32 = 24.;
const RULE: f32 = 9.;
const PAD: f32 = 5.;

/// The number of the action at `path` (its row at each level), counting
/// the actions in order, those in a submenu where it stands, as the
/// system's menu numbers them (`platform::MenuItem`).
fn number(items: &[Item], path: &[usize]) -> Option<usize> {
    let (&row, rest) = path.split_first()?;
    let before: usize = items.get(..row)?.iter().map(Item::actions).sum();
    match items.get(row)? {
        Item::Action { .. } if rest.is_empty() => Some(before),
        Item::Submenu { items, .. } => number(items, rest).map(|number| before + number),
        _ => None,
    }
}

/// The row of the action numbered `number` in a menu with no submenus.
fn row_of(items: &[Item], number: usize) -> Option<usize> {
    let mut seen = 0;
    items.iter().position(|item| {
        let here = matches!(item, Item::Action { .. }) && seen == number;
        seen += item.actions();
        here
    })
}

/// How far down a menu's rows the row `row` begins.
fn top_of(items: &[Item], row: usize) -> f32 {
    items
        .iter()
        .take(row)
        .map(|item| match item {
            Item::Separator => RULE,
            _ => ROW,
        })
        .sum()
}

/// The menu open, if one is: there is one at a time.
struct Open(Option<Entity<Popup>>);

impl Global for Open {}

pub(crate) struct Popup {
    window: AnyWindowHandle,
    items: Vec<Item>,
    at: Point<Pixels>,
    /// The row lit at each level open, the menu's own first: a level after
    /// it is a submenu open from the row lit before it.
    trail: Vec<Option<usize>>,
    focus: FocusHandle,
    /// What had the keys before, for them to go back to.
    restore: Option<FocusHandle>,
    answer: Option<oneshot::Sender<Option<usize>>>,
    /// Where each level was last drawn in the window, for its submenu to
    /// open beside the row it is from.
    drawn: Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>,
    /// A long menu scrolls, as a list of every time zone does.
    scroll: ScrollHandle,
    _active: Subscription,
    _blur: Subscription,
}

/// A context menu of `items` at `at` in the window, the action numbered
/// `under` lit and beneath the pointer: the number of the action chosen,
/// or `None`.
pub(crate) async fn pop_up(
    window: AnyWindowHandle,
    items: Vec<MenuItem>,
    under: Option<usize>,
    at: Point<Pixels>,
    cx: &mut gpui::AsyncApp,
) -> Option<usize> {
    let (answer, chosen) = oneshot::channel();
    let items = items.into_iter().map(Item::from).collect();
    window
        .update(cx, |_, window, cx| {
            open(items, under, at, answer, window, cx)
        })
        .ok()?;
    chosen.await.ok().flatten()
}

/// Opens `items` at `at`, answering through `answer` once it closes.
fn open(
    items: Vec<Item>,
    under: Option<usize>,
    at: Point<Pixels>,
    answer: oneshot::Sender<Option<usize>>,
    window: &mut Window,
    cx: &mut App,
) {
    // A menu already open is put away first.
    if let Some(open) = cx.try_global::<Open>().and_then(|open| open.0.clone()) {
        if open.read(cx).window == window.window_handle() {
            open.update(cx, |open, cx| open.close(None, window, cx));
        } else {
            dismiss(cx);
        }
    }
    let lit = under.and_then(|under| row_of(&items, under));
    // Under the pointer: the lit row's middle where the press was.
    let at = match lit {
        Some(row) => point(
            at.x - px(PAD),
            at.y - px(PAD + top_of(&items, row) + ROW / 2.),
        ),
        None => at,
    };
    crate::debug(format_args!("drawn menu: open, {} rows", items.len()));
    let restore = window.focused(cx);
    let handle = window.window_handle();
    let popup = cx.new(|cx| {
        let focus = cx.focus_handle();
        let active = cx.observe_window_activation(window, |popup: &mut Popup, window, cx| {
            if !window.is_window_active() {
                popup.close(None, window, cx);
            }
        });
        let blur = cx.on_blur(&focus, window, |popup: &mut Popup, window, cx| {
            popup.close(None, window, cx);
        });
        Popup {
            window: handle,
            items,
            at,
            trail: vec![lit],
            focus,
            restore,
            answer: Some(answer),
            drawn: Rc::default(),
            scroll: ScrollHandle::new(),
            _active: active,
            _blur: blur,
        }
    });
    if let Some(row) = lit {
        popup.read(cx).scroll.scroll_to_item(row);
    }
    let focus = popup.read(cx).focus.clone();
    cx.set_global(Open(Some(popup)));
    window.focus(&focus, cx);
    window.refresh();
}

/// Puts away the menu open, if one is.
pub(crate) fn dismiss(cx: &mut App) {
    let Some(popup) = cx.try_global::<Open>().and_then(|open| open.0.clone()) else {
        return;
    };
    let window = popup.read(cx).window;
    let _ = window.update(cx, |_, window, cx| {
        popup.update(cx, |popup, cx| popup.close(None, window, cx));
    });
}

/// The menu open over this window, for its root to draw last.
pub(crate) fn layer(window: &Window, cx: &App) -> Option<AnyElement> {
    let popup = cx.try_global::<Open>()?.0.clone()?;
    (popup.read(cx).window == window.window_handle()).then(|| popup.into_any_element())
}

/// The app's menus, from the title bar's button where there is no menu
/// bar: each a submenu, as the menu bar's are, with the keys that do the
/// same. What is chosen is done as the menu bar does it.
pub(crate) fn app_menus(at: Point<Pixels>, window: &mut Window, cx: &mut App) {
    let Some(menus) = cx.get_menus() else {
        return;
    };
    let mut actions = Vec::new();
    let items = menus
        .into_iter()
        .map(|menu| Item::Submenu {
            title: menu.name,
            items: owned(menu.items, &mut actions, window, cx),
        })
        .collect();
    let (answer, chosen) = oneshot::channel();
    open(items, None, at, answer, window, cx);
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Some(number)) = chosen.await else {
            return;
        };
        if let Some(action) = actions.get(number) {
            let _ = handle.update(cx, |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx);
            });
        }
    })
    .detach();
}

/// The menu bar's items as drawn ones, and their actions in the order they
/// are numbered. The system's own menus (a Mac's Services) are left out.
fn owned(
    items: Vec<OwnedMenuItem>,
    actions: &mut Vec<Box<dyn Action>>,
    window: &Window,
    cx: &mut App,
) -> Vec<Item> {
    items
        .into_iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Separator => Some(Item::Separator),
            OwnedMenuItem::Submenu(menu) => Some(Item::Submenu {
                title: menu.name,
                items: owned(menu.items, actions, window, cx),
            }),
            OwnedMenuItem::SystemMenu(_) => None,
            OwnedMenuItem::Action {
                name,
                action,
                checked,
                disabled,
                ..
            } => {
                let key = window
                    .highest_precedence_binding_for_action(action.as_ref())
                    .and_then(|binding| binding.keystrokes().first().map(platform::key_text))
                    .map(SharedString::from);
                // Done by this window, or by the app whatever window is
                // in front (Settings…, Quit), as the menu bar asks.
                let enabled = !disabled
                    && (window.is_action_available(action.as_ref(), cx)
                        || cx.is_action_available(action.as_ref()));
                actions.push(action);
                Some(Item::Action {
                    title: name.into(),
                    enabled,
                    checked,
                    key,
                })
            }
        })
        .collect()
}

impl Popup {
    /// The rows of the level `depth`.
    fn level(&self, depth: usize) -> &[Item] {
        let mut items = self.items.as_slice();
        for lit in self.trail.iter().take(depth) {
            match lit.and_then(|row| items.get(row)) {
                Some(Item::Submenu { items: inner, .. }) => items = inner,
                _ => return &[],
            }
        }
        items
    }

    /// Answers with `choice` and puts the menu away, the keys going back to
    /// what had them.
    fn close(&mut self, choice: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answer) = self.answer.take() else {
            return;
        };
        crate::debug(format_args!("drawn menu: closed, {choice:?} chosen"));
        let this = cx.entity_id();
        if let Some(open) = cx.try_global::<Open>()
            && open.0.as_ref().is_some_and(|open| open.entity_id() == this)
        {
            cx.set_global(Open(None));
        }
        if self.focus.is_focused(window) {
            match self.restore.take() {
                Some(restore) => window.focus(&restore, cx),
                None => window.blur(cx),
            }
        }
        let _ = answer.send(choice);
        window.refresh();
    }

    /// The pointer is on the row `row` of the level `depth`: it is lit, and
    /// a submenu opens.
    fn hover(&mut self, depth: usize, row: usize, cx: &mut Context<Self>) {
        let opens = matches!(self.level(depth).get(row), Some(Item::Submenu { .. }));
        if self.trail.get(depth) == Some(&Some(row)) && (self.trail.len() > depth + 1) == opens {
            return;
        }
        self.trail.truncate(depth + 1);
        if let Some(lit) = self.trail.get_mut(depth) {
            *lit = Some(row);
        }
        if opens {
            self.trail.push(None);
        }
        cx.notify();
    }

    /// Chooses the row `row` of the level `depth`: an action is done, a
    /// submenu opens with its first row lit.
    fn choose(&mut self, depth: usize, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.level(depth).get(row) {
            Some(Item::Action { enabled: true, .. }) => {
                let mut path: Vec<usize> =
                    self.trail.iter().take(depth).flatten().copied().collect();
                path.push(row);
                let choice = number(&self.items, &path);
                self.close(choice, window, cx);
            }
            Some(Item::Submenu { items, .. }) => {
                let first = items.iter().position(Item::lights);
                self.trail.truncate(depth + 1);
                if let Some(lit) = self.trail.get_mut(depth) {
                    *lit = Some(row);
                }
                self.trail.push(first);
                cx.notify();
            }
            _ => {}
        }
    }

    /// Lights the next row that can be, or the one before, around the ends.
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let depth = self.trail.len() - 1;
        let items = self.level(depth);
        let count = items.len();
        if count == 0 {
            return;
        }
        let from = self.trail.get(depth).copied().flatten();
        let mut row = from;
        for _ in 0..count {
            let next = match (row, down) {
                (None, true) => 0,
                (None, false) => count - 1,
                (Some(row), true) => (row + 1) % count,
                (Some(row), false) => (row + count - 1) % count,
            };
            row = Some(next);
            if items.get(next).is_some_and(Item::lights) {
                break;
            }
        }
        if row != from
            && let Some(row) = row
            && items.get(row).is_some_and(Item::lights)
        {
            if let Some(lit) = self.trail.get_mut(depth) {
                *lit = Some(row);
            }
            if depth == 0 {
                self.scroll.scroll_to_item(row);
            }
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing behind the menu takes a key while it is open.
        cx.stop_propagation();
        crate::debug(format_args!("drawn menu: {}", event.keystroke.key));
        let depth = self.trail.len() - 1;
        let lit = self.trail.last().copied().flatten();
        match event.keystroke.key.as_str() {
            "down" => self.step(true, cx),
            "up" => self.step(false, cx),
            "right" => {
                if let Some(row) = lit
                    && matches!(self.level(depth).get(row), Some(Item::Submenu { .. }))
                {
                    self.choose(depth, row, window, cx);
                }
            }
            "left" if depth > 0 => {
                self.trail.pop();
                cx.notify();
            }
            "escape" if depth > 0 => {
                self.trail.pop();
                cx.notify();
            }
            "escape" => self.close(None, window, cx),
            "enter" | "space" => {
                if let Some(row) = lit {
                    self.choose(depth, row, window, cx);
                }
            }
            _ => {}
        }
    }

    /// One level of the menu: its rows on a raised card, as the search
    /// field's results are.
    fn panel(&self, depth: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let items = self.level(depth);
        let lit = self.trail.get(depth).copied().flatten();
        let ticks = items
            .iter()
            .any(|item| matches!(item, Item::Action { checked: true, .. }));
        let rows = items
            .iter()
            .enumerate()
            .map(|(row, item)| Self::row(depth, row, item, lit == Some(row), ticks, cx));
        let drawn = Rc::clone(&self.drawn);
        let tall = f32::from(window.viewport_size().height) - 16.;
        div()
            .relative()
            .occlude()
            .min_w(px(180.))
            .max_w(px(420.))
            .rounded(px(10.))
            .bg(theme::raised())
            .shadow(crate::search::shadow())
            .child(
                div()
                    .id(("menu-level", depth))
                    .max_h(px(tall))
                    .when(depth == 0, |rows| {
                        rows.overflow_y_scroll().track_scroll(&self.scroll)
                    })
                    .p(px(PAD))
                    .children(rows),
            )
            .child(
                canvas(
                    move |bounds, _, _| {
                        let mut drawn = drawn.borrow_mut();
                        if drawn.len() <= depth {
                            drawn.resize(depth + 1, None);
                        }
                        if let Some(slot) = drawn.get_mut(depth) {
                            *slot = Some(bounds);
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .into_any_element()
    }

    fn row(
        depth: usize,
        row: usize,
        item: &Item,
        lit: bool,
        ticks: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, enabled, checked, key, submenu) = match item {
            Item::Separator => {
                return div()
                    .h(px(RULE))
                    .px(px(4.))
                    .flex()
                    .items_center()
                    .child(div().h(px(1.)).flex_1().bg(theme::line()))
                    .into_any_element();
            }
            Item::Action {
                title,
                enabled,
                checked,
                key,
            } => (title.clone(), *enabled, *checked, key.clone(), false),
            Item::Submenu { title, .. } => (title.clone(), item.lights(), false, None, true),
        };
        div()
            .id(("menu-row", depth * 100_000 + row))
            .h(px(ROW))
            .px(px(8.))
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(6.))
            .whitespace_nowrap()
            .when(lit && enabled, |row| row.bg(theme::fill2()))
            .when(!enabled, |row| row.opacity(0.4))
            .when(enabled, |element| {
                element
                    .on_mouse_move(cx.listener(move |popup, _, _, cx| popup.hover(depth, row, cx)))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |popup, _, window, cx| {
                            popup.choose(depth, row, window, cx);
                        }),
                    )
            })
            .when(ticks, |element| {
                element.child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .when(checked, |tick| tick.child(icon("check", 12., theme::ink()))),
                )
            })
            .child(div().flex_1().min_w_0().truncate().child(title))
            .children(key.map(|key| {
                div()
                    .pl(px(24.))
                    .flex_none()
                    .text_color(theme::faint())
                    .child(key)
            }))
            .when(submenu, |element| {
                element.child(
                    div()
                        .pl(px(16.))
                        .child(icon("chevronRight", 12., theme::dim())),
                )
            })
            .into_any_element()
    }
}

impl Render for Popup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut levels = vec![
            anchored()
                .position(self.at)
                .child(self.panel(0, window, cx))
                .into_any_element(),
        ];
        // Each submenu beside the row it is from, or on the other side
        // where there is no room.
        for depth in 1..self.trail.len() {
            let parent = self.level(depth - 1);
            let (Some(row), Some(drawn)) = (
                self.trail.get(depth - 1).copied().flatten(),
                self.drawn.borrow().get(depth - 1).copied().flatten(),
            ) else {
                // Drawn beside its row once that row has been drawn.
                window.request_animation_frame();
                break;
            };
            let scrolled = if depth == 1 {
                self.scroll.offset().y
            } else {
                px(0.)
            };
            let top = drawn.top() + px(PAD + top_of(parent, row)) + scrolled;
            let beside = Bounds::new(point(drawn.left(), top), size(drawn.size.width, px(ROW)));
            levels.push(
                anchored()
                    .attach(beside, Anchor::TopRight)
                    .anchor(Anchor::TopLeft)
                    .offset(point(px(-2.), px(-PAD)))
                    .child(self.panel(depth, window, cx))
                    .into_any_element(),
            );
        }
        let close = |popup: &mut Self,
                     _: &gpui::MouseDownEvent,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            popup.close(None, window, cx);
        };
        deferred(
            div()
                .id("menu")
                .key_context("Menu")
                .track_focus(&self.focus)
                .on_key_down(cx.listener(Self::key))
                // A press outside the menu puts it away and goes no further.
                .occlude()
                .on_mouse_down(MouseButton::Left, cx.listener(close))
                .on_mouse_down(MouseButton::Right, cx.listener(close))
                .on_mouse_down(MouseButton::Middle, cx.listener(close))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .font_family(".SystemUIFont")
                .font_weight(FontWeight::NORMAL)
                .text_size(px(13.))
                .line_height(px(ROW))
                .text_color(theme::ink())
                .children(levels),
        )
        .with_priority(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(title: &'static str) -> Item {
        Item::Action {
            title: title.into(),
            enabled: true,
            checked: false,
            key: None,
        }
    }

    #[test]
    fn actions_are_numbered_as_the_systems_menu_numbers_them() {
        let items = vec![
            action("Go to Album"),
            Item::Separator,
            Item::Submenu {
                title: "Rating".into(),
                items: vec![action("★★"), Item::Separator, action("None")],
            },
            action("After"),
        ];
        assert_eq!(number(&items, &[0]), Some(0));
        assert_eq!(number(&items, &[2, 0]), Some(1));
        assert_eq!(number(&items, &[2, 2]), Some(2));
        assert_eq!(number(&items, &[3]), Some(3));
        assert_eq!(number(&items, &[1]), None);
        assert_eq!(number(&items, &[2]), None);
    }

    #[test]
    fn the_action_under_the_pointer_is_found_by_its_number() {
        let items = vec![action("A"), Item::Separator, action("B"), action("C")];
        assert_eq!(row_of(&items, 0), Some(0));
        assert_eq!(row_of(&items, 1), Some(2));
        assert_eq!(row_of(&items, 2), Some(3));
        assert_eq!(row_of(&items, 3), None);
        assert!((top_of(&items, 2) - (ROW + RULE)).abs() < f32::EPSILON);
    }
}
