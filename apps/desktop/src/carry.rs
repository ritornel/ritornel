//! Something carried to Up next: songs, a record, or entries of Up next
//! itself. Where it is dropped says where it goes: straight after an entry,
//! at the top to play next, or at the end ("Queue, context menus, and
//! Settings" in docs/internals/desktop.md).
//!
//! The carrying is GPUI's own drag and drop, all within the window: a
//! source starts it with `on_drag`, handing over a `Carried` and the chip
//! drawn under the pointer, and a target takes it with `on_drop`. What is
//! carried is also kept here, for the parts that show they can take it
//! (the card's ring, the player bar's button) while GPUI's own is out of
//! reach; it counts only while GPUI still has a drag under way.

use std::sync::Arc;

use gpui::{
    App, AppContext, Context, Entity, FontWeight, Global, IntoElement, ObjectFit, ParentElement,
    Pixels, Point, Render, RenderImage, SharedString, Styled, StyledImage, Window, div, img, px,
};
use ritornel_contract::{AlbumRef, Id, PlayContext, PlaySource, QueuePlace};

use crate::player::{Player, move_to};
use crate::theme;

/// What is carried.
#[derive(Clone)]
pub(crate) enum Carried {
    /// Songs, by their track appearances.
    Songs {
        ids: Vec<Id>,
        name: String,
        context: Option<PlayContext>,
        art: Option<Arc<RenderImage>>,
    },
    /// A record: its tracks are read when it is dropped.
    Record {
        album: AlbumRef,
        name: String,
        art: Option<Arc<RenderImage>>,
    },
    /// Entries of Up next, being put in another order.
    Entries { ids: Vec<Id>, name: String },
}

impl Carried {
    /// Whether it is entries of Up next, which only the card takes.
    pub(crate) fn entries(&self) -> bool {
        matches!(self, Self::Entries { .. })
    }

    fn name(&self) -> &str {
        match self {
            Self::Songs { name, .. } | Self::Record { name, .. } | Self::Entries { name, .. } => {
                name
            }
        }
    }
}

/// What is being carried, as it was picked up.
struct Carrying(Carried);

impl Global for Carrying {}

/// What is being carried now, if anything.
pub(crate) fn carried(cx: &App) -> Option<&Carried> {
    if !cx.has_active_drag() {
        return None;
    }
    cx.try_global::<Carrying>().map(|carrying| &carrying.0)
}

/// A record on a page, carried to Up next as `Record.svelte`'s is: its
/// songs are read when it lands.
pub(crate) fn record<E: gpui::StatefulInteractiveElement>(
    element: E,
    tile: &ritornel_contract::AlbumTile,
    art: Option<Arc<RenderImage>>,
) -> E {
    element.on_drag(
        Carried::Record {
            album: tile.album,
            name: tile.title.clone().unwrap_or_else(|| "Untitled".to_owned()),
            art,
        },
        lift,
    )
}

/// Starts carrying `carried`, as a source's `on_drag` does: it is kept for
/// the targets to see, and its chip is drawn under the pointer, which
/// picked it up `offset` from the source's corner.
pub(crate) fn lift(
    carried: &Carried,
    offset: Point<Pixels>,
    _window: &mut Window,
    cx: &mut App,
) -> Entity<Chip> {
    crate::debug(format_args!("carry: lifted “{}”", carried.name()));
    cx.set_global(Carrying(carried.clone()));
    let chip = Chip {
        carried: carried.clone(),
        offset,
    };
    cx.new(|_| chip)
}

/// Puts `carried` at `place`: straight after an entry, next, or at the end.
pub(crate) fn drop(carried: &Carried, place: QueuePlace, player: &Entity<Player>, cx: &mut App) {
    crate::debug(format_args!(
        "carry: dropped “{}” at {place:?}",
        carried.name()
    ));
    let carried = carried.clone();
    player.update(cx, |player, cx| match carried {
        Carried::Entries { ids, .. } => {
            if let Some(to) = move_to(place, &ids) {
                player.move_entries(ids, to, cx);
            }
        }
        Carried::Songs {
            ids, name, context, ..
        } => {
            let name = if ids.len() == 1 {
                format!("“{name}”")
            } else {
                format!("{} songs", ids.len())
            };
            player
                .add(PlaySource::Tracks { ids }, place, name, context, cx)
                .detach();
        }
        // A record is read when it lands, so a group gives the release it shows.
        Carried::Record { album, name, .. } => add_record(player, album, name, place, cx),
    });
}

fn add_record(
    player: &mut Player,
    album: AlbumRef,
    name: String,
    place: QueuePlace,
    cx: &mut Context<Player>,
) {
    let read = player.record_songs(album, cx);
    cx.spawn(async move |this, cx| {
        // The record has gone since it was shown; there is nothing to add.
        let Some((source, context)) = read.await else {
            return;
        };
        let added = this.update(cx, |player, cx| {
            player.add(source, place, format!("“{name}”"), Some(context), cx)
        });
        if let Ok(added) = added {
            added.await;
        }
    })
    .detach();
}

/// The image a carried thing is drawn as under the pointer: its cover, its
/// name, and how many songs when there are several, as the prototype draws
/// it (`.dragchip`). The pointer holds it 20 points in from its corner, as
/// `setDragImage(image, 20, 20)` does.
pub(crate) struct Chip {
    carried: Carried,
    /// Where the pointer picked up the source, from its corner: the chip is
    /// drawn from there.
    offset: Point<Pixels>,
}

impl Render for Chip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (art, count) = match &self.carried {
            Carried::Songs { ids, art, .. } => (art.clone(), ids.len()),
            Carried::Record { art, .. } => (art.clone(), 0),
            Carried::Entries { ids, .. } => (None, ids.len()),
        };
        let name: SharedString = self.carried.name().to_owned().into();
        let chip = div()
            .absolute()
            .left(self.offset.x - px(20.))
            .top(self.offset.y - px(20.))
            .h(px(40.))
            .max_w(px(260.))
            .pl(px(5.))
            .pr(px(14.))
            .flex()
            .items_center()
            .gap(px(9.))
            .rounded(px(10.))
            .bg(theme::raised())
            .shadow(vec![
                gpui::BoxShadow::new(px(0.), px(0.), gpui::Hsla::from(theme::line()))
                    .spread_radius(px(1.)),
            ])
            .text_color(theme::ink())
            .text_size(px(13.))
            .whitespace_nowrap()
            .child(
                div()
                    .size(px(30.))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(theme::fill2())
                    .children(art.map(|art| {
                        img(art)
                            .size(px(30.))
                            .rounded(px(6.))
                            .object_fit(ObjectFit::Cover)
                    })),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight(600.))
                    .child(name),
            )
            .children((count > 1).then(|| {
                div()
                    .flex_none()
                    .text_color(theme::dim())
                    .child(format!("{count} songs"))
            }));
        div().relative().child(chip)
    }
}
