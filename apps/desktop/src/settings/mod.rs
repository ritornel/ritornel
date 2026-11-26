//! The Settings window ("Queue, context menus, and Settings" in
//! docs/internals/desktop.md): Ritornel › Settings… (⌘,) opens it, a second
//! window on the app's one connection. There is only ever one; opening it
//! again brings it forward, and ⌘W closes it for good.
//!
//! It has its title, a toolbar of sections, and the section chosen, as a
//! Mac settings window has them. It is 640 points wide and as tall as its
//! section, keeping its top where it is as the section changes; it opens on
//! the section it was last left on, once what the section reads is in, or
//! after a moment without it, so that it does not open at one height and
//! move to another.

mod about;
mod appearance;
mod controls;
mod lastfm;
mod library;
mod model;
pub(crate) mod words;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, AppContext, Bounds, ClickEvent, Context, Entity, FocusHandle, FontWeight, Global,
    InteractiveElement, IntoElement, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowControlArea, WindowHandle,
    WindowOptions, actions, canvas, div, prelude::FluentBuilder, px, size,
};
use jiff::tz::TimeZone;

use crate::link::{Link, State};
use crate::{platform, remember, theme};
use model::Model;
use words::{Clock, Style};

actions!(ritornel, [OpenSettings, CloseSettings]);

/// The window's width, in points. Its height follows the section shown.
const WIDTH: f32 = 640.;

/// Its height before it has been fitted to the section.
const FIRST_HEIGHT: f32 = 460.;

/// How long the window waits for what its section reads before it is
/// fitted, and shown, at the height it has.
const SHOW_WITHIN: Duration = Duration::from_millis(1000);

/// A section of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Library,
    Appearance,
    Lastfm,
    About,
}

impl Section {
    pub(crate) const ALL: [Self; 4] = [Self::Library, Self::Appearance, Self::Lastfm, Self::About];

    /// Its name, as remembered and as a script names it.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Appearance => "appearance",
            Self::Lastfm => "lastfm",
            Self::About => "about",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Appearance => "Appearance",
            Self::Lastfm => "Last.fm",
            Self::About => "About",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Library => "icons/settings/library.svg",
            Self::Appearance => "icons/settings/appearance.svg",
            Self::Lastfm => "icons/settings/lastfm.svg",
            Self::About => "icons/settings/about.svg",
        }
    }

    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.id() == name)
    }
}

/// A click that does `then` to the model.
fn act(
    model: &Entity<Model>,
    then: impl Fn(&mut Model, &mut Window, &mut Context<Model>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let model = model.clone();
    move |_, window, cx| model.update(cx, |model, cx| then(model, window, cx))
}

/// What opening Settings needs, for the app's life: the connection, and
/// the window while it is open.
struct Opener {
    link: Entity<Link>,
    /// Whether a script runs the app: then nothing takes the keyboard or
    /// opens outside it.
    quiet: bool,
    window: Option<WindowHandle<Settings>>,
    /// The section to open on, while the window waits to open.
    opening: Option<Section>,
}

impl Global for Opener {}

/// Sets up Ritornel › Settings… and its key, on the app's connection.
pub(crate) fn install(link: Entity<Link>, quiet: bool, cx: &mut App) {
    cx.set_global(Opener {
        link,
        quiet,
        window: None,
        opening: None,
    });
    cx.on_action(|_: &OpenSettings, cx| open(None, cx));
    let bindings = [
        crate::frame::key("secondary-,", OpenSettings, None, cx),
        crate::frame::key("secondary-w", CloseSettings, Some("Settings"), cx),
    ];
    cx.bind_keys(bindings.into_iter().flatten());
}

/// Opens the Settings window, on `section` or the one it was last left on,
/// or brings it forward if it is open. A new window waits for what its
/// section reads, a second at most, so that its first frame is drawn at
/// the height it keeps: a window not yet on screen is drawn only once.
pub(crate) fn open(section: Option<Section>, cx: &mut App) {
    let Some(opener) = cx.try_global::<Opener>() else {
        return;
    };
    let (link, quiet, window, waiting) = (
        opener.link.clone(),
        opener.quiet,
        opener.window,
        opener.opening.is_some(),
    );
    if let Some(open) = window
        && open
            .update(cx, |settings, window, cx| {
                if let Some(section) = section {
                    settings.go(section, cx);
                }
                if quiet {
                    platform::bring_forward_quietly(window);
                } else {
                    window.activate_window();
                }
            })
            .is_ok()
    {
        return;
    }
    let section = section
        .or_else(|| Section::named(&remember::load().settings_section))
        .unwrap_or(Section::Library);
    cx.global_mut::<Opener>().opening = Some(section);
    if waiting {
        return;
    }
    let model = model::new(link, quiet, cx);
    cx.spawn(async move |cx| {
        let asked = Instant::now();
        while asked.elapsed() < SHOW_WITHIN && !cx.update(|cx| model.read(cx).ready(cx)) {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
        }
        cx.update(|cx| open_window(model, quiet, cx));
    })
    .detach();
}

fn open_window(model: Entity<Model>, quiet: bool, cx: &mut App) {
    let Some(section) = cx.global_mut::<Opener>().opening.take() else {
        return;
    };
    // In a test, on its display no one sees, as the main window is.
    let display = cx
        .try_global::<crate::TestDisplay>()
        .and_then(|test| test.0);
    let bounds = Bounds::centered(display, size(px(WIDTH), px(FIRST_HEIGHT)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display,
        titlebar: Some(TitlebarOptions {
            title: Some(section.title().into()),
            appears_transparent: true,
            traffic_light_position: None,
        }),
        // Shown once it is fitted to its section, where the system can.
        show: !platform::FITTING_SHOWS,
        focus: !quiet,
        is_resizable: false,
        is_minimizable: false,
        // A script poses it whether or not it has focus.
        inactive_frame_interval: if quiet {
            None
        } else {
            WindowOptions::default().inactive_frame_interval
        },
        ..Default::default()
    };
    let opened = cx.open_window(options, |window, cx| {
        cx.new(|cx| Settings::new(model, section, quiet, window, cx))
    });
    cx.global_mut::<Opener>().window = opened.ok();
}

/// The Settings window open now, if one is.
pub(crate) fn window(cx: &App) -> Option<WindowHandle<Settings>> {
    cx.try_global::<Opener>()?.window
}

pub(crate) struct Settings {
    model: Entity<Model>,
    section: Section,
    focus: FocusHandle,
    zones_focus: FocusHandle,
    zone: TimeZone,
    style: Style,
    quiet: bool,
    /// The height of what the window holds, as last laid out.
    measured: Rc<Cell<f32>>,
    /// The height last asked of the window.
    fitted: Rc<Cell<f32>>,
    title: &'static str,
    _ticking: Task<()>,
    _model: Subscription,
    _appearance: Subscription,
    _active: Subscription,
    _looks: Subscription,
}

impl Settings {
    fn new(
        model: Entity<Model>,
        section: Section,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let followed = cx.observe(&model, |_, _, cx| cx.notify());
        let appearance = cx.observe_window_appearance(window, |_, window, _| {
            theme::follow(window.appearance());
            window.refresh();
        });
        let active = cx.observe_window_activation(window, |_, _, cx| cx.notify());
        // A look chosen here, from a menu that answers later, shows at once.
        let looks = cx.observe_global::<crate::remember::Looks>(|_, cx| cx.notify());
        if let Ok(held) = std::env::var("RITORNEL_GPUI_APPEARANCE") {
            platform::hold_appearance(window, held == "dark");
        }
        // What the server does on its own shows in the Library section: a
        // disk that goes away, a folder polled, a rescan. Times are said
        // again as they pass.
        let ticking = cx.spawn(async move |settings, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(15))
                    .await;
                let alive = settings.update(cx, |settings, cx| {
                    if settings.section == Section::Library {
                        settings.model.update(cx, |model, cx| model.read_status(cx));
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        let focus = cx.focus_handle();
        if !quiet {
            window.focus(&focus, cx);
        }
        let style = Style::of_locale(&platform::locale().unwrap_or_else(|| "en-US".to_owned()));
        Self {
            model,
            section,
            focus,
            zones_focus: cx.focus_handle(),
            zone: TimeZone::system(),
            style,
            quiet,
            measured: Rc::default(),
            fitted: Rc::default(),
            title: "",
            _ticking: ticking,
            _model: followed,
            _appearance: appearance,
            _active: active,
            _looks: looks,
        }
    }

    /// Shows a section, and remembers it.
    pub(crate) fn go(&mut self, to: Section, cx: &mut Context<Self>) {
        if self.section != to {
            // Reset the zones list state whenever the section is revisited.
            self.model.update(cx, |model, _| {
                model.chosen = None;
                model.adding = None;
            });
        }
        // What the server says of the library and the record is read
        // again on the way in, not only on the timer: a sign out in the
        // Last.fm section changes it.
        if to == Section::Library && self.section != to {
            self.model.update(cx, |model, cx| model.read_status(cx));
        }
        self.section = to;
        let mut remembered = remember::load();
        remembered.settings_section = to.id().to_owned();
        remember::save(&remembered);
        cx.notify();
    }

    /// Opens the form for a zone, as its + does, for a script.
    pub(crate) fn start_adding(&mut self, cx: &mut Context<Self>) {
        let clock = self.clock();
        self.model
            .update(cx, |model, cx| model.start_adding(&clock, cx));
    }

    /// The model, for a script.
    pub(crate) fn model(&self) -> &Entity<Model> {
        &self.model
    }

    fn clock(&self) -> Clock {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(0));
        Clock {
            now,
            zone: self.zone.clone(),
            style: self.style,
        }
    }

    /// The arrows move between sections, as a toolbar's tabs do.
    fn arrows(&mut self, event: &gpui::KeyDownEvent, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        if keys.modifiers.modified() {
            return;
        }
        let at = Section::ALL
            .iter()
            .position(|section| *section == self.section)
            .unwrap_or(0);
        let next = match keys.key.as_str() {
            "right" => Section::ALL.get(at + 1),
            "left" => at.checked_sub(1).and_then(|at| Section::ALL.get(at)),
            _ => None,
        };
        if let Some(next) = next {
            self.go(*next, cx);
            cx.stop_propagation();
        }
    }

    fn section(&self, window: &Window, cx: &App) -> gpui::AnyElement {
        let model = self.model.read(cx);
        let active = window.is_window_active();
        if self.section == Section::Appearance {
            return appearance::render(active, cx).into_any_element();
        }
        let link = model.link(cx);
        match &link.state {
            State::Connected(_) => {}
            state => {
                let said = if link.preparing.is_some() {
                    "Ritornel is updating your library. Settings come back when it is done."
                } else if matches!(state, State::Looking) {
                    "Looking for the server…"
                } else {
                    "Ritornel cannot reach its server. Settings come back when it does."
                };
                return div()
                    .my(px(12.))
                    .mx(px(2.))
                    .text_color(theme::dim())
                    .child(said)
                    .into_any_element();
            }
        }
        let clock = self.clock();
        match self.section {
            Section::Library => {
                library::render(model, &self.model, &clock, active, cx).into_any_element()
            }
            Section::Lastfm => lastfm::render(
                model,
                &self.model,
                &clock,
                active,
                &self.zones_focus,
                self.zones_focus.is_focused(window),
            )
            .into_any_element(),
            Section::About | Section::Appearance => about::render(model, cx).into_any_element(),
        }
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self.section.title();
        if self.title != title {
            // Named after the section shown, for the Window menu.
            window.set_window_title(title);
            self.title = title;
        }
        let (measured, fitted, quiet) = (
            Rc::clone(&self.measured),
            Rc::clone(&self.fitted),
            self.quiet,
        );
        // What the window holds, measured as it is laid out, and the
        // window fitted to it.
        let measure = canvas(
            move |bounds, _, _| measured.set(f32::from(bounds.size.height)),
            {
                let measured = Rc::clone(&self.measured);
                move |_, (), window, _| {
                    let height = measured.get().ceil();
                    crate::debug(format_args!("settings: drawn, {height} points tall"));
                    if (height - fitted.get()).abs() >= 0.5 && height >= 120. {
                        fitted.set(height);
                        platform::fit_window(window, height, quiet);
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let settings = cx.entity().clone();
        let header = header(
            title,
            self.section,
            platform::CHROME.app_buttons,
            Rc::new(move |section, _, cx| {
                settings.update(cx, |settings, cx| settings.go(section, cx));
            }),
        );
        let section = div()
            .pt(px(20.))
            .px(px(24.))
            .pb(px(24.))
            .child(self.section(window, cx));
        div()
            .id("settings")
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(|_: &CloseSettings, window, _| window.remove_window())
            .on_key_down(cx.listener(|settings, event, _, cx| settings.arrows(event, cx)))
            .size_full()
            .bg(theme::bg())
            .font_family(".SystemUIFont")
            .text_size(px(13.))
            .line_height(px(controls::LINE))
            .text_color(theme::ink())
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(header)
                    .child(section)
                    .child(measure),
            )
            .children(crate::drawn::layer(window, cx))
    }
}

/// The title row's height where the system draws its own window buttons.
const TITLE_HEIGHT: f32 = 28.;

type OnSection = Rc<dyn Fn(Section, &mut Window, &mut App)>;

/// The window's header: title, section toolbar, and on systems where the
/// app draws its own caption buttons, a close button flush in the top-right
/// corner (Settings is neither resizable nor minimizable).
fn header(
    title: &'static str,
    current: Section,
    app_buttons: bool,
    on_select: OnSection,
) -> impl IntoElement {
    let title_height = if app_buttons {
        crate::frame::CAPTION_HEIGHT
    } else {
        TITLE_HEIGHT
    };
    div()
        .id("header")
        .relative()
        .px(px(12.))
        .border_b_1()
        .border_color(theme::line())
        // On Windows the area answers the system's hit test as the caption.
        .window_control_area(WindowControlArea::Drag)
        // Elsewhere a press here moves the window, as a title bar's does.
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
        .child(
            div()
                .h(px(title_height))
                .font_weight(FontWeight(600.))
                .line_height(px(title_height))
                .text_center()
                .debug_selector(|| "settings-title".into())
                .child(title),
        )
        .child(toolbar(current, on_select))
        .when(app_buttons, |header| {
            header.child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .debug_selector(|| "settings-close".into())
                    .child(crate::frame::close_button(title_height)),
            )
        })
}

fn toolbar(current: Section, on_select: OnSection) -> impl IntoElement {
    let tab = move |section: Section| {
        let on = section == current;
        let group = format!("tab-{}", section.id());
        let go = Rc::clone(&on_select);
        div()
            .id(section.id())
            .group(group.clone())
            .debug_selector(move || format!("settings-tab-{}", section.id()))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(2.))
            .min_w(px(64.))
            .pt(px(5.))
            .px(px(8.))
            .pb(px(4.))
            .rounded(px(6.))
            .text_size(px(11.))
            .line_height(px(controls::SMALL_LINE))
            .text_color(theme::dim())
            .when(on, |tab| tab.bg(theme::fill()))
            .when(!on, |tab| {
                tab.hover(theme::hovered_ink).active(theme::pressed)
            })
            // The press must not start a window drag.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_: &ClickEvent, window, cx| go(section, window, cx))
            .child(
                gpui::svg()
                    .path(section.icon())
                    .size(px(22.))
                    .flex_none()
                    .text_color(if on { theme::acc() } else { theme::dim() })
                    .when(!on, |icon| icon.group_hover(group, theme::hovered_ink)),
            )
            .child(
                div()
                    .when(on, |label| label.text_color(theme::ink()))
                    .child(section.title()),
            )
    };
    div()
        .flex()
        .justify_center()
        .gap(px(4.))
        .pt(px(2.))
        .pb(px(7.))
        .children(Section::ALL.map(tab))
}

#[cfg(test)]
mod tests {
    use gpui::VisualContext;

    use super::*;

    struct HeaderHost {
        section: Section,
        app_buttons: bool,
    }

    impl Render for HeaderHost {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let host = cx.entity().clone();
            div().w(px(WIDTH)).child(header(
                self.section.title(),
                self.section,
                self.app_buttons,
                Rc::new(move |section, _, cx| {
                    host.update(cx, |host, cx| {
                        host.section = section;
                        cx.notify();
                    });
                }),
            ))
        }
    }

    #[gpui::test]
    fn app_buttons_lays_out_close_button_flush_top_right_and_switches_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let (host, cx) = cx.add_window_view(|_, _| HeaderHost {
            section: Section::Library,
            app_buttons: true,
        });
        cx.simulate_window_resize(cx.window_handle(), size(px(WIDTH), px(FIRST_HEIGHT)));
        assert!(cx.refresh().is_ok());

        assert_eq!(
            cx.debug_bounds("settings-close").map(|bounds| (
                bounds.origin.y,
                bounds.right(),
                bounds.size
            )),
            Some((
                px(0.),
                px(WIDTH),
                size(
                    px(crate::frame::CAPTION_BUTTON_WIDTH),
                    px(crate::frame::CAPTION_HEIGHT),
                ),
            )),
        );
        assert_eq!(
            cx.debug_bounds("settings-title")
                .map(|bounds| bounds.size.height),
            Some(px(crate::frame::CAPTION_HEIGHT)),
        );

        let appearance = cx.debug_bounds("settings-tab-appearance");
        assert!(appearance.is_some(), "Appearance tab should render");
        if let Some(appearance) = appearance {
            cx.simulate_click(appearance.center(), gpui::Modifiers::none());
        }
        assert_eq!(
            host.read_with(cx, |host, _| host.section),
            Section::Appearance
        );
    }

    #[gpui::test]
    fn without_app_buttons_draws_no_close_button(cx: &mut gpui::TestAppContext) {
        let (_host, cx) = cx.add_window_view(|_, _| HeaderHost {
            section: Section::Library,
            app_buttons: false,
        });
        cx.simulate_window_resize(cx.window_handle(), size(px(WIDTH), px(FIRST_HEIGHT)));
        assert!(cx.refresh().is_ok());

        assert!(cx.debug_bounds("settings-close").is_none());
        assert_eq!(
            cx.debug_bounds("settings-title")
                .map(|bounds| bounds.size.height),
            Some(px(TITLE_HEIGHT)),
        );
    }
}
