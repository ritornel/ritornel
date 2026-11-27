//! The GPUI desktop client for Ritornel. See docs/internals/desktop.md.

// A release build on Windows is a windowed program, so starting it from the
// Start menu opens no console beside it, as a Mac app opens no terminal. A
// debug build keeps its console for the developer lines; a release build
// prints them only where its output is redirected.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod accent;
mod album;
mod artist;
mod away;
mod banner;
mod bar;
mod blank;
mod blur;
mod button;
mod carry;
mod covers;
mod drawn;
mod elastic;
mod find;
mod frame;
mod frost;
mod history;
mod icons;
mod input;
mod layout;
mod lean;
mod link;
mod marks;
mod measure;
mod menu;
mod motion;
mod moves;
mod net;
mod places;
mod platform;
mod player;
mod pretty;
mod record;
mod remember;
mod script;
mod search;
mod settings;
mod shell;
mod speaker;
mod stage;
mod stream;
mod swipe;
mod theme;
mod thisday;
mod tracked;
mod tracks;
mod upnext;
mod waits;
mod wall;
mod words;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};

use crate::measure::Fling;
use crate::shell::Shell;
use crate::wall::Wall;

struct Args {
    /// `None`: use `RITORNEL_DATA_DIR` or the system default data directory.
    data_dir: Option<PathBuf>,
    width: f32,
    height: f32,
    /// Run the measure's fling for this many seconds, then quit.
    measure: Option<f32>,
    out: Option<PathBuf>,
    seed: u64,
    label: String,
    script: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut args = Args {
        data_dir: None,
        width: 1400.,
        height: 900.,
        measure: None,
        out: None,
        seed: 7,
        label: String::new(),
        script: None,
    };
    let mut list = std::env::args().skip(1);
    while let Some(arg) = list.next() {
        let mut value = || list.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--data-dir" => args.data_dir = Some(PathBuf::from(value()?)),
            "--size" => {
                let text = value()?;
                let (w, h) = text.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                args.width = w.parse().map_err(|_| "--size is WIDTHxHEIGHT")?;
                args.height = h.parse().map_err(|_| "--size is WIDTHxHEIGHT")?;
            }
            "--measure" => {
                args.measure = Some(value()?.parse().map_err(|_| "--measure takes seconds")?)
            }
            "--out" => args.out = Some(PathBuf::from(value()?)),
            "--seed" => args.seed = value()?.parse().map_err(|_| "--seed takes a number")?,
            "--label" => args.label = value()?,
            "--script" => args.script = Some(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(args)
}

/// A line on standard error for a developer, when `RITORNEL_GPUI_DEBUG` is set.
#[allow(clippy::print_stderr)]
pub(crate) fn debug(line: std::fmt::Arguments<'_>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("RITORNEL_GPUI_DEBUG").is_some()) {
        eprintln!("{line}");
    }
}

/// What the program says when it cannot start, on standard error.
#[allow(clippy::print_stderr)]
fn complain(line: std::fmt::Arguments<'_>) {
    eprintln!("ritornel-gpui: {line}");
}

fn main() {
    let args = match args() {
        Ok(args) => args,
        Err(problem) => {
            complain(format_args!("{problem}"));
            std::process::exit(2);
        }
    };
    // What the Dock's icon does when the app is running, set once the app
    // has started (GPUI takes the handler before then).
    type Reopen = Box<dyn FnMut(&mut App)>;
    let reopen: Rc<RefCell<Option<Reopen>>> = Rc::default();
    let on_reopen = reopen.clone();
    // GPUI starts its sprite clock inside with_platform. Sample the time after
    // platform setup so that resuming the bars preserves their held phase.
    let platform = gpui_platform::current_platform(false);
    let scene_clock = marks::SceneClock(std::time::Instant::now());
    let app = gpui::Application::with_platform(platform).with_assets(icons::Icons);
    app.on_reopen(move |cx| {
        if let Some(reopen) = on_reopen.borrow_mut().as_mut() {
            reopen(cx);
        }
    });
    app.run(move |cx: &mut App| {
        cx.set_global(scene_clock);
        frame::menus(cx);
        input::bind_keys(cx);
        // Whatever the system needs set up: on a Mac, reading what a
        // touchpad's scroll says that GPUI does not pass on.
        platform::start();
        // The accent follows the system's: its first reading before the
        // first frame where the system gives one at once (and an accent
        // held for testing even where it gives none), and each change as it
        // comes, every window drawn again for it.
        let mut accents = platform::accents(cx);
        theme::follow_accent(accents.now().flatten());
        cx.spawn(async move |cx| {
            while let Some(accent) = accents.next().await {
                if theme::follow_accent(accent) {
                    cx.update(|cx| cx.refresh_windows());
                }
            }
        })
        .detach();
        let link = link::new(args.data_dir.as_deref(), cx);
        // How the app looks, as remembered, for every window to follow.
        cx.set_global(remember::Looks::remembered());
        settings::install(link.clone(), args.script.is_some(), cx);
        // Quit stops a server the app started; one it found it leaves.
        let quitting = link.clone();
        cx.on_action(move |_: &frame::Quit, cx| {
            quitting.read(cx).stop_started();
            cx.quit();
        });
        // The app ending stops a server it holds (`platform::SERVER_APART`
        // says whether it holds one): where closing the window ends it,
        // the server is apart, and plays on.
        let stopping = link.clone();
        cx.on_app_quit(move |cx| {
            stopping.read(cx).stop_owned();
            async {}
        })
        .detach();
        // A test that must not be seen opens on the display it names
        // (`scripts/desktop.sh` resolves one when `RITORNEL_OFFSCREEN=1`),
        // out of the Dock (developer-only).
        let display = match std::env::var("RITORNEL_TEST_DISPLAY") {
            Err(_) => None,
            Ok(wanted) => {
                let displays = cx.displays();
                // By the system's number for it (on a Mac, its
                // `CGDirectDisplayID`), which GPUI keeps as its ID.
                let found = displays
                    .iter()
                    .find(|display| u64::from(display.id()).to_string() == wanted.trim());
                // Never on the screen in front instead: a test that cannot
                // be hidden does not run.
                let Some(found) = found else {
                    let seen: Vec<String> = displays
                        .iter()
                        .map(|display| u64::from(display.id()).to_string())
                        .collect();
                    complain(format_args!(
                        "no display at {wanted}; there are {}",
                        seen.join(", ")
                    ));
                    cx.quit();
                    return;
                };
                platform::keep_out_of_the_dock();
                Some(found.id())
            }
        };
        cx.set_global(TestDisplay(display));
        let first = Opening {
            display,
            width: args.width,
            height: args.height,
            // A measure and a script run whether or not the window has focus.
            unfocused_frames: args.measure.is_some() || args.script.is_some(),
            measure: args.measure.map(|seconds| Measure {
                seconds,
                out: args.out.clone(),
                seed: args.seed,
                label: args.label.clone(),
                stay: args.script.is_some(),
            }),
        };
        // On a Mac, closing the window leaves the app running, and the
        // Dock's icon brings a window back: a new one when it was closed,
        // the one there is otherwise (a measure never opens a second).
        let reopening = link.clone();
        *reopen.borrow_mut() = Some(Box::new(move |cx| {
            let shown = cx
                .windows()
                .into_iter()
                .find(|window| window.downcast::<Shell>().is_some());
            match shown {
                Some(window) => {
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                }
                None => {
                    let again = Opening {
                        display,
                        width: args.width,
                        height: args.height,
                        unfocused_frames: false,
                        measure: None,
                    };
                    if open_main(again, reopening.clone(), cx).is_err() {
                        complain(format_args!("the window could not be opened again"));
                    }
                }
            }
            cx.activate(true);
        }));
        let window = open_main(first, link, cx);
        let Ok(window) = window else {
            complain(format_args!("the window could not be opened"));
            cx.quit();
            return;
        };
        match args.script.clone() {
            // A script must not take the keyboard from whoever is at the machine.
            Some(script) => {
                let _ = window.update(cx, |_, window, _| platform::bring_forward_quietly(window));
                script::run(script, window.into(), &mut cx.to_async()).detach();
            }
            None => cx.activate(true),
        }
    });
}

/// The display no one sees that a test runs on (`RITORNEL_TEST_DISPLAY`),
/// where every window of the app opens, or `None` outside a test.
pub(crate) struct TestDisplay(pub Option<gpui::DisplayId>);

impl gpui::Global for TestDisplay {}

/// How the main window opens.
struct Opening {
    display: Option<gpui::DisplayId>,
    width: f32,
    height: f32,
    /// Draw at full rate when the window does not have focus.
    unfocused_frames: bool,
    /// The measure's fling, on the first window only.
    measure: Option<Measure>,
}

struct Measure {
    seconds: f32,
    out: Option<PathBuf>,
    seed: u64,
    label: String,
    stay: bool,
}

/// Opens the main window, at its first start and whenever the Dock asks for
/// one again after it was closed.
fn open_main(
    opening: Opening,
    link: gpui::Entity<link::Link>,
    cx: &mut App,
) -> gpui::Result<gpui::WindowHandle<Shell>> {
    let Opening {
        display,
        width,
        height,
        unfocused_frames,
        measure,
    } = opening;
    let bounds = Bounds::centered(display, size(px(width), px(height)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id: display,
        window_min_size: Some(size(px(860.), px(560.))),
        // The name the system knows the window by, for its launcher
        // entry and icon on Linux (Wayland's app ID, X11's class).
        app_id: Some("dev.ritornel.desktop".into()),
        titlebar: Some(TitlebarOptions {
            title: Some("Ritornel".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(20.), px(19.))),
        }),
        inactive_frame_interval: if unfocused_frames {
            None
        } else {
            WindowOptions::default().inactive_frame_interval
        },
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        // Light and dark follow the system appearance.
        theme::follow(window.appearance());
        window
            .observe_window_appearance(|window, _| {
                theme::follow(window.appearance());
                window.refresh();
            })
            .detach();
        let scale = window.scale_factor();
        let player = cx.new(|cx| player::Player::new(&link, cx));
        let wall = cx.new(|cx| Wall::new(link.clone(), player.clone(), scale, cx));
        if let Some(measure) = measure {
            let wall = wall.clone();
            // Started once the first rows are in.
            window.on_next_frame(move |_, cx| {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(measure::wait()).await;
                    wall.update(cx, |wall, cx| {
                        let fling = Fling::new(measure.seconds, measure.seed);
                        wall.stats.out = measure.out.clone();
                        wall.stats.label = measure.label.clone();
                        wall.stats.stay = measure.stay;
                        wall.start_fling(fling);
                        cx.notify();
                    });
                })
                .detach();
            });
        }
        cx.new(|cx| Shell::new(link, wall, player, window, cx))
    })
}
