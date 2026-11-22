//! The player bar (`frame/PlayerBar.svelte`): what is playing and its
//! rating, the transport and the seek bar, Up next and Lean back. There is
//! no volume control, by design (docs/design/ui.md, "The frame").
//!
//! A new song's cover settles into place. The seek bar follows the pointer
//! while it is held, anywhere in the window, and the song moves when it is
//! let go.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    Bounds, Context, Entity, FontWeight, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement, Pixels, Render,
    RenderImage, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task,
    Window, canvas, div, img, prelude::FluentBuilder, px,
};
use ritornel_contract::{Id, Repeat, SkipDirection};

use crate::icons::icon;
use crate::link::{Link, LinkEvent};
use crate::net::NoArt;
use crate::player::{Player, stars_of};
use crate::theme;
use crate::wall::PLAYER_BAR;

/// The real `clock`: minutes and seconds, the seconds rounded.
pub(crate) fn clock(ms: u64) -> String {
    let seconds = (ms + 500) / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub(crate) fn tabular() -> gpui::FontFeatures {
    gpui::FontFeatures(Arc::new(vec![("tnum".to_owned(), 1)]))
}

/// What the bar asks of the window.
pub(crate) enum BarEvent {
    /// Open the album of the song playing.
    OpenAlbum(Arc<crate::wall::TileRow>),
    /// Open Lean back, or close it.
    LeanBack,
}

/// Where the cover of the song playing rests, in a window `height` tall:
/// 50 points square, 14 in from the left, in the middle of the bar under
/// its hairline. Lean back's cover travels from it and back to it.
pub(crate) fn cover_bounds(height: f32) -> Bounds<Pixels> {
    Bounds::new(
        gpui::point(
            px(14.),
            px(height - PLAYER_BAR + 1. + (PLAYER_BAR - 1. - 50.) / 2.),
        ),
        gpui::size(px(50.), px(50.)),
    )
}

impl gpui::EventEmitter<BarEvent> for PlayerBar {}

#[derive(Clone, Copy)]
struct SeekDrag {
    entry: Id,
    duration: u64,
    fraction: f32,
}

impl SeekDrag {
    fn position(self) -> u64 {
        (self.fraction * self.duration as f32).round() as u64
    }

    fn for_entry(self, entry: Option<Id>) -> bool {
        Some(self.entry) == entry
    }
}

pub(crate) struct PlayerBar {
    link: Entity<Link>,
    player: Entity<Player>,
    /// The cover of the song playing, by its art, and when it came.
    cover: Option<(Id, Arc<RenderImage>, Instant)>,
    /// The art being read, so that it is read once.
    reading: Option<(Id, Task<()>)>,
    /// A cover the server does not have, not asked for again.
    missing: Option<Id>,
    /// Covers that could not be read, asked for again after a while: the
    /// bar is drawn often, and would otherwise ask at every frame.
    retries: crate::covers::Retries,
    /// When the song playing changed, for its cover to settle into place.
    arrived: Option<(Id, Instant)>,
    /// The entry and position held by the pointer.
    held: Option<SeekDrag>,
    /// Where the seek bar is, as last drawn.
    seek_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The star the pointer is on.
    star: Option<u32>,
    /// Whether the pointer is on the album's name.
    album_hovered: bool,
    /// Whether the page behind has a colour that runs on behind the bar.
    pub washed: bool,
    /// Whether the cover is hidden while a copy of it travels (Lean back's).
    pub cover_away: bool,
    _player: Subscription,
    _link: Subscription,
}

impl PlayerBar {
    pub(crate) fn new(link: Entity<Link>, player: Entity<Player>, cx: &mut Context<Self>) -> Self {
        let followed = cx.observe(&player, |_, _, cx| cx.notify());
        // A new session: the cover is asked for again at once, and a read
        // through the session before is let go.
        let session = cx.subscribe(&link, |bar: &mut Self, _, event: &LinkEvent, cx| {
            if let LinkEvent::Connected(_) = event {
                bar.reading = None;
                bar.retries = crate::covers::Retries::default();
                cx.notify();
            }
        });
        cx.on_release(|bar, cx| {
            if let Some((_, image, _)) = bar.cover.take() {
                crate::covers::drop_unshared(image, None, cx);
            }
        })
        .detach();
        Self {
            link,
            player,
            cover: None,
            reading: None,
            missing: None,
            retries: crate::covers::Retries::default(),
            arrived: None,
            held: None,
            seek_bounds: Rc::default(),
            star: None,
            album_hovered: false,
            washed: false,
            cover_away: false,
            _player: followed,
            _link: session,
        }
    }

    /// Reads the cover of the song playing, at the pixels it is drawn at.
    fn want_cover(&mut self, art: Option<Id>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(art) = art else {
            if let Some((_, old, _)) = self.cover.take() {
                crate::covers::drop_unshared(old, Some(window), cx);
            }
            self.reading = None;
            return;
        };
        // A read for the song before is let go: it would land in its place.
        if self.reading.as_ref().is_some_and(|(id, _)| *id != art) {
            self.reading = None;
        }
        if self.cover.as_ref().is_some_and(|(id, _, _)| *id == art)
            || self.reading.as_ref().is_some_and(|(id, _)| *id == art)
            || self.missing == Some(art)
            || self.retries.waits(art, Instant::now())
        {
            return;
        }
        let Some(net) = self.link.read(cx).net() else {
            return;
        };
        let px = (50. * window.scale_factor()).ceil() as u32;
        let sizes = net.art_sizes.clone();
        let http = Arc::clone(&net.art);
        let read = net
            .rt()
            .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, px).await });
        let task = cx.spawn(async move |this, cx| {
            let image = read.await.unwrap_or(Err(NoArt::Failed));
            this.update(cx, |bar, cx| {
                bar.reading = None;
                match image {
                    Ok((image, _, _)) => {
                        bar.retries.read(art);
                        if let Some((_, old, _)) =
                            bar.cover.replace((art, Arc::new(image), Instant::now()))
                        {
                            crate::covers::drop_unshared(old, None, cx);
                        }
                    }
                    Err(NoArt::Missing) => bar.missing = Some(art),
                    Err(NoArt::Failed) => {
                        let wait = bar.retries.failed(art, Instant::now());
                        crate::covers::notify_after(wait, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        });
        self.reading = Some((art, task));
    }

    fn fraction_at(&self, x: Pixels) -> Option<f32> {
        let bounds = self.seek_bounds.get()?;
        let width = f32::from(bounds.size.width);
        (width > 0.).then(|| (f32::from(x - bounds.origin.x) / width).clamp(0., 1.))
    }
}

impl PlayerBar {
    /// Up next's button: lit while the card is open. With the card closed
    /// it shows that it takes what is being carried, and a drop on it puts
    /// that at the end of Up next.
    fn up_next_button(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let open = self.player.read(cx).open;
        let carried = crate::carry::carried(cx);
        let carrying = carried.is_some() && !open;
        let takes = carried.is_some_and(|carried| !carried.entries());
        let acc = theme::acc();
        let ring = move |alpha: f32| {
            vec![
                gpui::BoxShadow::new(
                    px(0.),
                    px(0.),
                    gpui::Hsla::from(gpui::Rgba { a: alpha, ..acc }),
                )
                .spread_radius(px(1.5))
                .inset(),
            ]
        };
        let color = if open {
            acc
        } else if carrying {
            theme::ink()
        } else {
            theme::dim()
        };
        let player = self.player.clone();
        div()
            .id("up-next")
            .group("up-next")
            .size(px(32.))
            .rounded(px(8.))
            .flex()
            .items_center()
            .justify_center()
            .when(open, |button| button.bg(gpui::Rgba { a: 0.16, ..acc }))
            .when(carrying, |button| button.shadow(ring(0.6)))
            .when(takes, |button| {
                button.drag_over::<crate::carry::Carried>(move |style, _, _, _| {
                    style.bg(gpui::Rgba { a: 0.3, ..acc }).shadow(ring(1.))
                })
            })
            .on_click(cx.listener(|bar, _, _, cx| {
                bar.player
                    .update(cx, |player, cx| player.toggle_card(None, cx));
            }))
            .on_drop(move |carried: &crate::carry::Carried, _, cx| {
                if !carried.entries() {
                    crate::carry::drop(carried, ritornel_contract::QueuePlace::End, &player, cx);
                }
            })
            .child(
                icon("upNext", 15., color)
                    .group_hover("up-next", |style| style.text_color(theme::ink())),
            )
    }
}

impl Render for PlayerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        theme::flush_soft(window, cx);
        let player = self.player.read(cx);
        let state = player.state.clone();
        let track = state.as_ref().and_then(|state| state.track.clone());
        let on = player.on();
        let playing = player.playing();
        let duration = state
            .as_ref()
            .and_then(|state| state.duration_ms)
            .unwrap_or(0);
        let position = player.position();
        let shuffle = state.as_ref().is_some_and(|state| state.shuffle);
        let repeat = state.as_ref().map_or(Repeat::Off, |state| state.repeat);
        let own = track.as_ref().and_then(|track| {
            track
                .recording_id
                .map(|recording| player.own(recording, track.rating))
        });
        let entry = state.as_ref().and_then(|state| state.entry_id);
        self.held = self.held.filter(|drag| drag.for_entry(entry));
        let shown_ms = self.held.map_or(position, SeekDrag::position);
        let fraction = if duration > 0 {
            (shown_ms as f32 / duration as f32).min(1.)
        } else {
            0.
        };
        let live = on && duration > 0;

        if let Some(entry) = entry
            && self.arrived.as_ref().is_none_or(|(id, _)| *id != entry)
        {
            self.arrived = Some((entry, Instant::now()));
        }
        self.want_cover(track.as_ref().and_then(|track| track.art_id), window, cx);
        crate::covers::flush_dropped(window, cx);
        let mut animating = false;

        // What is playing: the cover, the words, and the rating in a place
        // of its own at the end, which does not move with the length of a title.
        let now_playing = match &track {
            Some(track) => {
                let title = track.title.clone().unwrap_or_else(|| "Untitled".to_owned());
                // A new song's cover settles into place (`arrive`, 0.22 s).
                let settled = self
                    .arrived
                    .as_ref()
                    .map_or(1., |(_, at)| (at.elapsed().as_secs_f32() / 0.22).min(1.));
                animating |= settled < 1.;
                let eased = 1. - (1. - settled).powi(3);
                let side = 50. * (0.94 + 0.06 * eased);
                let mut cover = div()
                    .relative()
                    .size(px(side))
                    .rounded(px(8.))
                    .opacity(0.35 + 0.65 * eased)
                    .bg(theme::fill())
                    .border_1()
                    .border_color(theme::line());
                // The cover, or for a song with none its album's soft colours.
                let image = match (&self.cover, track.art_id) {
                    (Some((art, image, _)), Some(wanted)) if *art == wanted => {
                        Some(Arc::clone(image))
                    }
                    (_, Some(_)) => None,
                    (_, None) => Some(theme::soft_cached(
                        &format!(
                            "{}{}",
                            track.album.as_deref().unwrap_or(&title),
                            track.artist.as_deref().unwrap_or("")
                        ),
                        (50. * window.scale_factor()).ceil() as u32,
                    )),
                };
                if let Some(image) = image {
                    cover = cover.child(
                        img(image)
                            .absolute()
                            .top_0()
                            .left_0()
                            .size(px(side))
                            .rounded(px(8.))
                            .object_fit(ObjectFit::Cover),
                    );
                }
                // The artist, and the album, which opens its page, as the
                // cover does.
                let artist = track
                    .artist
                    .clone()
                    .unwrap_or_else(|| "Unknown artist".to_owned());
                let mut text = artist.clone();
                let mut highlights = Vec::new();
                let mut album_at = None;
                if let Some(album) = &track.album {
                    let dot = text.len()..text.len() + " · ".len();
                    text.push_str(" · ");
                    highlights.push((
                        dot,
                        gpui::HighlightStyle {
                            color: Some(theme::faint().into()),
                            ..Default::default()
                        },
                    ));
                    let range = text.len()..text.len() + album.len();
                    text.push_str(album);
                    if self.album_hovered && track.release_id.is_some() {
                        highlights.push((
                            range.clone(),
                            gpui::HighlightStyle {
                                color: Some(theme::ink().into()),
                                underline: Some(gpui::UnderlineStyle {
                                    thickness: px(1.),
                                    ..Default::default()
                                }),
                                ..Default::default()
                            },
                        ));
                    }
                    album_at = Some(range);
                }
                let opens = track.release_id.map(|release| {
                    Arc::new(crate::wall::TileRow::of_album(
                        ritornel_contract::AlbumTile {
                            album: ritornel_contract::AlbumRef::Release { id: release },
                            title: track.album.clone(),
                            artist_credit: Some(ritornel_contract::Credit {
                                text: artist.clone(),
                                artist_ids: Vec::new(),
                            }),
                            year: None,
                            track_count: 0,
                            duration_ms: 0,
                            version_count: 0,
                            art_id: track.art_id,
                            quality: None,
                        },
                    ))
                });
                let this = cx.entity().downgrade();
                let mut line = gpui::InteractiveText::new(
                    "bar-line",
                    gpui::StyledText::new(text).with_highlights(highlights),
                );
                if let (Some(range), Some(row)) = (album_at.clone(), opens.clone()) {
                    let hovered = this.clone();
                    let opened = this.clone();
                    line = line
                        .on_hover(move |at, _, _, cx| {
                            let over = at.is_some_and(|at| range.contains(&at));
                            let _ = hovered.update(cx, |bar, cx| {
                                if bar.album_hovered != over {
                                    bar.album_hovered = over;
                                    cx.notify();
                                }
                            });
                        })
                        .on_click(
                            vec![album_at.clone().unwrap_or_default()],
                            move |_, _, cx| {
                                let row = Arc::clone(&row);
                                let _ =
                                    opened.update(cx, |_, cx| cx.emit(BarEvent::OpenAlbum(row)));
                            },
                        );
                }
                let recording = track.recording_id;
                let said = track.rating;
                let stars = crate::marks::stars(
                    "bar".into(),
                    stars_of(own.flatten(), track.loved),
                    self.star,
                    recording.is_some(),
                    14.,
                    cx,
                    |bar: &mut Self, star, over, _| {
                        if over {
                            bar.star = Some(star);
                        } else if bar.star == Some(star) {
                            bar.star = None;
                        }
                    },
                    move |bar: &mut Self, star, cx| {
                        let Some(recording) = recording else { return };
                        bar.player.update(cx, |player, cx| {
                            player.click_star(recording, said, star, cx);
                        });
                    },
                );
                // The song playing's menu: where it is from, and its stars
                // (`nowMenu`). While it is open the block wears the menu's
                // fill, reaching a little past it.
                let shown = crate::menu::mark("now", window, cx);
                let summary = track.clone();
                div()
                    .id("now-playing")
                    .flex_1()
                    .min_w(px(240.))
                    .flex()
                    .items_center()
                    .gap(px(13.))
                    .whitespace_nowrap()
                    .when(shown > 0., |block| {
                        block
                            .rounded(px(9.))
                            .bg(crate::menu::fill(shown))
                            .shadow(vec![
                                gpui::BoxShadow::new(
                                    px(0.),
                                    px(0.),
                                    gpui::Hsla::from(crate::menu::fill(shown)),
                                )
                                .spread_radius(px(6.)),
                            ])
                    })
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |_, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            let entries = crate::menu::now_menu(&summary, cx);
                            crate::menu::popup(
                                "now".to_owned(),
                                entries,
                                event.position,
                                window,
                                cx,
                            );
                        }),
                    )
                    .child(
                        div()
                            .id("bar-cover")
                            .size(px(50.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(cover)
                            .when(self.cover_away, |button| button.invisible())
                            .when_some(opens, |button, row| {
                                button.on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(BarEvent::OpenAlbum(Arc::clone(&row)));
                                }))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().font_weight(FontWeight(600.)).child(title))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(theme::dim())
                                    .child(line),
                            ),
                    )
                    .child(div().ml(px(10.)).mr(px(4.)).child(stars))
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .min_w(px(240.))
                .pl(px(6.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .font_weight(FontWeight(600.))
                        .text_color(theme::dim())
                        .child("Nothing playing"),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme::faint())
                        .child("Double-click a song, or press Play"),
                )
                .into_any_element(),
        };

        let accent_back = |share: f32| gpui::Rgba {
            a: share,
            ..theme::acc()
        };
        // Pointed at, a control that works turns to the ink.
        let toggle = |id: &'static str, name: &'static str, lit: bool| {
            div()
                .id(id)
                .group(id)
                .relative()
                .size(px(28.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .when(lit, |button| button.bg(accent_back(0.14)))
                .child(
                    icon(name, 16., if lit { theme::acc() } else { theme::faint() })
                        .when(on, |icon| {
                            icon.group_hover(id, |style| style.text_color(theme::ink()))
                        }),
                )
        };
        let player = self.player.clone();
        let act = move |doing: fn(&mut Player, &mut Context<Player>)| {
            let player = player.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut gpui::App| {
                player.update(cx, doing);
            }
        };
        let mut shuffle_button = toggle("shuffle", "shuffle", shuffle);
        let mut repeat_button = toggle("repeat", "repeat", repeat != Repeat::Off).when(
            repeat == Repeat::One,
            |button| {
                button.child(
                    div()
                        .absolute()
                        .top_0()
                        .right(px(1.))
                        .text_size(px(9.))
                        .line_height(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::acc())
                        .child("1"),
                )
            },
        );
        let step = |id: &'static str, name: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(name, 17., theme::ink()))
        };
        let mut previous = step("previous", "previous");
        let mut next = step("next", "next");
        let mut play = div()
            .id("play")
            .size(px(34.))
            .rounded_full()
            .bg(theme::ink())
            .flex()
            .items_center()
            .justify_center()
            .child(icon(
                if playing { "pause" } else { "play" },
                13.,
                theme::bg(),
            ));
        if on {
            shuffle_button = shuffle_button.on_click(act(|player, cx| {
                let shuffle = player.state.as_ref().is_some_and(|state| state.shuffle);
                player.set_shuffle(!shuffle, cx);
            }));
            repeat_button = repeat_button.on_click(act(|player, cx| player.cycle_repeat(cx)));
            previous =
                previous.on_click(act(|player, cx| player.skip(SkipDirection::Previous, cx)));
            next = next.on_click(act(|player, cx| player.skip(SkipDirection::Next, cx)));
            play = play.on_click(act(|player, cx| player.toggle(cx)));
        }

        let clock_cell = |text: String, right: bool| {
            div()
                .min_w(px(30.))
                .flex()
                .when(right, |clock| clock.justify_end())
                .child(text)
        };
        let seek_bounds = Rc::clone(&self.seek_bounds);
        let this = cx.entity().downgrade();
        let held = self.held.is_some();
        let bar_color = if held { theme::acc() } else { theme::ink() };
        let seek = div()
            .id("seek")
            .flex_1()
            .py(px(5.))
            .relative()
            .group("seek")
            .child(
                div()
                    .h(px(3.))
                    .rounded(px(2.))
                    .overflow_hidden()
                    .bg(theme::fill2())
                    .when(live, |track| {
                        track.child(
                            div()
                                .h_full()
                                .w(gpui::relative(fraction))
                                .rounded(px(2.))
                                .bg(bar_color)
                                .group_hover("seek", |style| style.bg(theme::acc())),
                        )
                    }),
            )
            // The bar's place, and while it is held the pointer anywhere in the window.
            .child(
                canvas(
                    move |bounds, _, _| seek_bounds.set(Some(bounds)),
                    move |bounds, (), window, _| {
                        let down = this.clone();
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                            if phase.bubble()
                                && event.button == MouseButton::Left
                                && bounds.contains(&event.position)
                            {
                                let _ = down.update(cx, |bar, cx| {
                                    if live {
                                        bar.held = entry
                                            .zip(bar.fraction_at(event.position.x))
                                            .map(|(entry, fraction)| SeekDrag {
                                                entry,
                                                duration,
                                                fraction,
                                            });
                                        cx.notify();
                                    }
                                });
                            }
                        });
                        if held {
                            let moved = this.clone();
                            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                                if phase.bubble() {
                                    let _ = moved.update(cx, |bar, cx| {
                                        if let Some(at) = bar.fraction_at(event.position.x)
                                            && let Some(drag) = &mut bar.held
                                        {
                                            drag.fraction = at;
                                            cx.notify();
                                        }
                                    });
                                }
                            });
                            let up = this.clone();
                            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                                if phase.bubble() && event.button == MouseButton::Left {
                                    let _ = up.update(cx, |bar, cx| {
                                        let Some(mut drag) = bar.held.take() else {
                                            return;
                                        };
                                        if let Some(at) = bar.fraction_at(event.position.x) {
                                            drag.fraction = at;
                                            bar.player.update(cx, |player, cx| {
                                                player.seek(drag.entry, drag.position(), cx)
                                            });
                                        }
                                        cx.notify();
                                    });
                                }
                            });
                        }
                    },
                )
                .absolute()
                .size_full(),
            );

        let side_button = |id: &'static str, name: &'static str, enabled: bool| {
            div()
                .id(id)
                .group(id)
                .size(px(32.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .when(!enabled, |button| button.opacity(0.35))
                .child(icon(name, 15., theme::dim()).when(enabled, |icon| {
                    icon.group_hover(id, |style| style.text_color(theme::ink()))
                }))
        };
        if animating {
            window.request_animation_frame();
        }
        div()
            .h(px(PLAYER_BAR))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(24.))
            .pl(px(14.))
            .pr(px(22.))
            // Over a colour the page borrows, the colour runs on behind the bar.
            // The real bar blurs it (`blur(30px)`), but nothing scrolls under
            // the bar: the wash and its veil, already blurred, are all there
            // is, so seen through they are the same.
            .bg(if self.washed {
                gpui::Rgba {
                    a: 0.72,
                    ..theme::bar_bg()
                }
            } else {
                theme::bar_bg()
            })
            .border_t_1()
            .border_color(theme::line())
            .child(now_playing)
            .child(
                div()
                    .w(px(440.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(22.))
                            .when(!on, |controls| controls.opacity(0.35))
                            .child(shuffle_button)
                            .child(previous)
                            .child(play)
                            .child(next)
                            .child(repeat_button),
                    )
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .text_size(px(11.5))
                            .text_color(theme::dim())
                            .font_features(tabular())
                            .child(clock_cell(
                                if on {
                                    clock(shown_ms)
                                } else {
                                    "–:––".to_owned()
                                },
                                true,
                            ))
                            .child(seek)
                            .child(clock_cell(
                                if live {
                                    clock(duration)
                                } else {
                                    "–:––".to_owned()
                                },
                                false,
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(240.))
                    .flex()
                    .justify_end()
                    .gap(px(6.))
                    .child(self.up_next_button(cx))
                    .child(side_button("lean-back", "leanBack", on).when(on, |button| {
                        button.on_click(cx.listener(|_, _, _, cx| cx.emit(BarEvent::LeanBack)))
                    })),
            )
    }
}

/// The notice over the player bar: a pill in the page's ink, centred, that
/// comes up a short hop and goes by itself (`frame/Toast.svelte`).
pub(crate) fn toast(
    text: SharedString,
    since: Instant,
    width: f32,
    window: &mut Window,
) -> impl IntoElement {
    let t = (since.elapsed().as_secs_f32() / 0.22).min(1.);
    if t < 1. {
        window.request_animation_frame();
    }
    let eased = crate::shell::bezier(0.2, 0.7, 0.2, 1., t);
    div()
        .absolute()
        .left_0()
        .w(px(width))
        .bottom(px(92. - 8. * (1. - eased)))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w(px(width * 0.7))
                .px(px(16.))
                .py(px(10.))
                .rounded_full()
                .bg(theme::ink())
                .text_color(theme::bg())
                .text_size(px(13.5))
                .font_weight(FontWeight(500.))
                .truncate()
                .opacity(eased)
                .shadow(vec![theme::css_shadow(10., 30., gpui::rgba(0x00000059))])
                .child(text),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seek_drag_expires_when_its_entry_changes() {
        let entry = Id::from_bytes([1; 16]);
        let drag = SeekDrag {
            entry,
            duration: 20_000,
            fraction: 0.75,
        };
        assert!(drag.for_entry(Some(entry)));
        assert_eq!(drag.position(), 15_000);
        assert!(!drag.for_entry(Some(Id::from_bytes([2; 16]))));
        assert!(!drag.for_entry(None));
    }
}
