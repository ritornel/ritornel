//! A GPUI client for Ritornel, as a spike: the Albums wall, an album's
//! page, the travelling cover and the system's menus, on the real server.
//! See NOTES.md.

mod album;
mod artist;
mod banner;
mod bar;
mod blank;
mod blur;
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
mod platform;
mod player;
mod pretty;
mod record;
mod remember;
mod script;
mod search;
mod settings;
mod shell;
mod stage;
mod stream;
mod swipe;
mod theme;
mod thisday;
mod tracked;
mod tracks;
mod upnext;
mod wall;
mod words;

use std::path::PathBuf;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};

use crate::measure::Fling;
use crate::shell::Shell;
use crate::wall::Wall;

struct Args {
    /// `None`: the data directory `RITORNEL_DATA_DIR` or the system's default names, as the Tauri app's.
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
    if std::env::var_os("RITORNEL_GPUI_DEBUG").is_some() {
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
    gpui_platform::application()
        .with_assets(icons::Icons)
        .run(move |cx: &mut App| {
            frame::menus(cx);
            input::bind_keys(cx);
            // Whatever the system needs set up: on a Mac, reading what a
            // touchpad's scroll says that GPUI does not pass on.
            platform::start();
            let link = link::new(args.data_dir.as_deref(), cx);
            // How the app looks, as remembered, for every window to follow.
            cx.set_global(remember::Looks {
                letters: remember::load().letters,
            });
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
            // (`tools/offscreen.m` makes one), out of the Dock (developer-only).
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
            let bounds = Bounds::centered(display, size(px(args.width), px(args.height)), cx);
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
                // A measure runs whether or not the window has focus.
                inactive_frame_interval: if args.measure.is_some() || args.script.is_some() {
                    None
                } else {
                    WindowOptions::default().inactive_frame_interval
                },
                ..Default::default()
            };
            let measure = args.measure;
            let out = args.out.clone();
            let seed = args.seed;
            let label = args.label.clone();
            let stay = args.script.is_some();
            let window = cx.open_window(options, move |window, cx| {
                // Light and dark follow the system, as the real app's do.
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
                if let Some(seconds) = measure {
                    let wall = wall.clone();
                    // Started once the first rows are in.
                    window.on_next_frame(move |_, cx| {
                        cx.spawn(async move |cx| {
                            cx.background_executor().timer(measure::wait()).await;
                            wall.update(cx, |wall, cx| {
                                let fling = Fling::new(seconds, seed);
                                wall.stats.out = out.clone();
                                wall.stats.label = label.clone();
                                wall.stats.stay = stay;
                                wall.start_fling(fling);
                                cx.notify();
                            });
                        })
                        .detach();
                    });
                }
                cx.new(|cx| Shell::new(link, wall, player, window, cx))
            });
            let Ok(window) = window else {
                complain(format_args!("the window could not be opened"));
                cx.quit();
                return;
            };
            match args.script.clone() {
                // A script must not take the keyboard from whoever is at the machine.
                Some(script) => {
                    let _ =
                        window.update(cx, |_, window, _| platform::bring_forward_quietly(window));
                    script::run(script, window.into(), &mut cx.to_async()).detach();
                }
                None => cx.activate(true),
            }
        });
}
