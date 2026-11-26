//! Playback: the player, its queue and session in `server.db`, the listen
//! recorder, and the audio pipeline under them. See
//! docs/internals/playback.md.
//!
//! - [`Player`] owns the queue, the session and the open listen. It sits on
//!   a [`Renderer`], and never sees the decoder or the output.
//! - [`AudioRenderer`] is the renderer that makes sound: Symphonia, rubato
//!   and cpal, with a ring between the decode thread and the output's
//!   callback. Its null output makes none.
//! - The store keeps the queue, the session and each listen until the hub
//!   has it; the outbox delivers it ([`deliver_listens`]).

pub(crate) mod audio;
pub(crate) mod catalog;
pub(crate) mod listen;
mod move_store;
mod outbox;
pub(crate) mod player;
pub(crate) mod queue;
pub(crate) mod renderer;
pub(crate) mod store;
#[cfg(test)]
pub(crate) mod tests;

pub use audio::{AudioRenderer, OnDemand, Output, VolumeCurve};
pub use catalog::{Catalog, Copy, TrackDisplay};
pub use move_store::PendingMove;
pub use outbox::{
    Delivered, Ingest, MAX_BACKOFF_MS, SendStamps, deliver_listens, deliver_listens_to,
};
pub use player::{
    AddTracks, CHECKPOINT_EVERY_MS, CLOSE_OUTPUT_AFTER_MS, Direction, Done, Fetch, NowPlaying,
    Order, OutputFailing, Place, PlayTracks, Player, PlayerEvent, PlayerSetup, PlayerState,
    QueuePage, Request, Settings, Status, Summary,
};
pub use queue::Repeat;
pub use renderer::{End, Event, FileFacts, Format, Progress, RenderError, Renderer};
/// Why a track was queued, which every listen of it records.
pub use ritornel_domain::history::ContextKind;
