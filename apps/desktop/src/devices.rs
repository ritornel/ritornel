//! Devices: what the library is synced to, and the page of the one you are
//! on ("Devices" in docs/internals/desktop.md).
//!
//! Down the left, a column of device tabs: each device's small drawing,
//! its name and its state, the one you are on on a card. With one device
//! the column is not there and the page has the window's width. The page
//! is the same before and after a device is set up: its drawing, name,
//! state and facts; three figures for the next sync, to copy, free after
//! and how long; one primary button; one bar of room; and its views, Sync
//! and Music, and Plays for a player that gives plays back.
//!
//! It holds nothing of its own: everything shown is read from the server
//! with the `devices.*` calls, and read again when a `devices` event says a
//! device changed. A sync is the server's, and carries on if the window
//! closes.

mod art;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Div, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, RenderImage, Rgba,
    ScrollEvent, SharedString, Stateful, StatefulInteractiveElement, Styled, StyledImage,
    Subscription, Task, Window, canvas, div, img, px,
};
use ritornel_contract::methods::{
    DevicesActions, DevicesAddFolder, DevicesEject, DevicesForget, DevicesFormats, DevicesGet,
    DevicesList, DevicesSet, DevicesSync, DevicesUnmanaged, JobsCancel, LibrarySearch, Method,
};
use ritornel_contract::transcode::{Mp3, Target};
use ritornel_contract::{
    Event, Id, JobCancelParams, NoParams, SearchParams, SearchResults, SyncActionKind,
    SyncActionRow, SyncActionState, SyncActions, SyncActionsParams, SyncAddFolderParams,
    SyncAttentionLevel, SyncDevice, SyncDeviceCommandParams, SyncDeviceDetail, SyncDeviceParams,
    SyncFolderKind, SyncFormat, SyncFormatOption, SyncFormats, SyncGroupBy, SyncLossless,
    SyncMadeFor, SyncOutcome, SyncPageParams, SyncPlaysKind, SyncRun, SyncSelector, SyncSetParams,
    SyncSetup, SyncSource, SyncSourceRole, SyncSourceSpec, SyncStartParams, SyncStep,
    SyncStepState, SyncUnit, SyncUnmanaged,
};

use crate::button::{Role, button};
use crate::input::{Field, FieldEvent};
use crate::link::{Link, LinkEvent};
use crate::marks::Mark;
use crate::net::{Net, no_library};
use crate::theme::{self, Status};
use crate::wall::viewport_of;
use crate::words::{plural, thousands};

/// The column of device tabs: where it starts, and how wide it is.
const COLUMN_LEFT: f32 = 16.;
const COLUMN: f32 = 216.;
/// The page's left edge beside the column, and its margins without one.
const BESIDE_COLUMN: f32 = COLUMN_LEFT + COLUMN + 32.;
const GUTTER: f32 = 40.;
/// The widest the page's own content runs.
const MEASURE: f32 = 1320.;
/// How many rows of a plan or a run are read at once.
const ROWS: u32 = 500;

/// A view of a device's page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Sync,
    Music,
    Plays,
}

/// What a `+` in Music adds what is found as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Adding {
    Rule,
    Always,
    LeaveOut,
}

impl Adding {
    fn role(self) -> SyncSourceRole {
        match self {
            Self::Rule => SyncSourceRole::Rule,
            Self::Always => SyncSourceRole::Always,
            Self::LeaveOut => SyncSourceRole::LeaveOut,
        }
    }
}

/// The rows shown, and what they were read for.
struct Rows {
    key: RowsKey,
    read: SyncActions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowsKey {
    device: Id,
    /// The plan's revision, or the run's ID.
    of: String,
    run: bool,
    kind: Option<SyncActionKind>,
    failed: bool,
}

pub(crate) struct DevicesPage {
    net: Option<Arc<Net>>,
    link: Entity<Link>,
    /// The shell's focus handle, which the keys go back to.
    home: FocusHandle,
    list: Option<Vec<SyncDevice>>,
    failed: bool,
    /// The server holds no library, so there is nothing to sync.
    unheld: bool,
    chosen: Option<Id>,
    detail: Option<SyncDeviceDetail>,
    view: View,
    /// Whether the view was chosen by hand, and for which device: until
    /// then a new device opens on Music and one set up on Sync.
    view_of: Option<Id>,
    rows: Option<Rows>,
    filter: Option<SyncActionKind>,
    /// Whether only what failed is shown.
    only_failed: bool,
    /// The groups opened, and their tracks.
    opened: HashMap<String, Vec<SyncActionRow>>,
    formats: Option<SyncFormats>,
    unmanaged: Option<SyncUnmanaged>,
    /// What a command was refused with, said under the header until the
    /// next thing is done.
    problem: Option<SharedString>,
    /// The Forget that waits to be confirmed.
    forgetting: bool,
    /// Whether the files Ritornel did not write are listed.
    reviewing: bool,
    /// The name being typed, while renaming.
    renaming: bool,
    name: Entity<Field>,
    /// The search for an artist or a record to add, and what it adds as.
    adding: Option<Adding>,
    find: Entity<Field>,
    found: Option<SearchResults>,
    asked: u64,
    pictures: HashMap<Id, Arc<RenderImage>>,
    wanted: HashSet<Id>,
    reading: Option<Task<()>>,
    reading_rows: Option<Task<()>>,
    pub top: f32,
    elastic: crate::elastic::Elastic,
    height: Rc<Cell<f32>>,
    freshness: crate::stage::Freshness,
    clock: crate::settings::words::Style,
    _subscriptions: Vec<Subscription>,
}

/// A size as a page says it: "2.6 GB", "48 MB".
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000_000, "TB"),
        (1_000_000_000, "GB"),
        (1_000_000, "MB"),
        (1_000, "kB"),
    ];
    for (unit, name) in UNITS {
        if bytes >= unit {
            let tenths = (bytes + unit / 20) / (unit / 10);
            // A round number, and anything of three figures, has no tenths.
            return if tenths.is_multiple_of(10) || tenths >= 1000 {
                format!("{} {name}", (tenths + 5) / 10)
            } else {
                format!("{}.{} {name}", tenths / 10, tenths % 10)
            };
        }
    }
    if bytes == 0 {
        "Nothing".to_owned()
    } else {
        format!("{bytes} B")
    }
}

/// How long something takes, near enough: "4 min", "< 1 min".
fn about(ms: u64) -> String {
    match ms / 1000 {
        seconds if seconds < 50 => "< 1 min".to_owned(),
        seconds if seconds < 3600 => format!("{} min", (seconds + 30) / 60),
        seconds => {
            let (hours, minutes) = (seconds / 3600, (seconds % 3600) / 60);
            if minutes == 0 {
                format!("{hours} h")
            } else {
                format!("{hours} h {minutes} min")
            }
        }
    }
}

fn format_name(format: SyncFormat) -> (&'static str, String) {
    match format.lossless {
        SyncLossless::AsIs => ("FLAC", "as it is".to_owned()),
        SyncLossless::Encode {
            target: Target::Mp3 { mode },
        } => (
            "MP3",
            match mode {
                Mp3::V0 => "V0".to_owned(),
                Mp3::V2 => "V2".to_owned(),
                Mp3::Cbr(kbps) => format!("{kbps} kbps"),
            },
        ),
        SyncLossless::Encode {
            target: Target::Opus { bitrate_kbps, .. },
        } => ("Opus", format!("{bitrate_kbps} kbps")),
        SyncLossless::Encode {
            target: Target::Flac {},
        } => ("FLAC", String::new()),
    }
}

fn made_for_name(made_for: SyncMadeFor) -> &'static str {
    match made_for {
        SyncMadeFor::CarStereo => "Car stereo",
        SyncMadeFor::Phone => "Phone",
        SyncMadeFor::OtherPlayer => "Other player",
        SyncMadeFor::PlainCopy => "Plain copy",
    }
}

const MADE_FOR: [SyncMadeFor; 4] = [
    SyncMadeFor::CarStereo,
    SyncMadeFor::Phone,
    SyncMadeFor::OtherPlayer,
    SyncMadeFor::PlainCopy,
];

/// The limits a folder that is always here is offered, in bytes.
const LIMITS: [u64; 7] = [
    1_000_000_000,
    4_000_000_000,
    8_000_000_000,
    16_000_000_000,
    32_000_000_000,
    64_000_000_000,
    128_000_000_000,
];

fn kind_word(kind: SyncActionKind) -> &'static str {
    match kind {
        SyncActionKind::Add => "Add",
        SyncActionKind::Replace => "Replace",
        SyncActionKind::Remove => "Remove",
        SyncActionKind::Conflict => "Conflict",
        SyncActionKind::Blocked => "Blocked",
    }
}

/// Why a row is there, in words, where its kind does not say it all.
fn reason_words(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "tags" => "its tags changed",
        "cover" => "its cover changed",
        "audio" => "its audio changed",
        "format" => "in the new format",
        "missing" => "it went missing from the device",
        "not_wanted" => "no longer wanted",
        "changed_on_device" => "changed on the device, so it is left alone",
        "master_gone" => "its master is gone from the library, so it is kept",
        "path_taken" => "a file Ritornel didn't write has its name",
        "path_too_long" => "its path is too long for the device",
        "no_format" => "the device plays none of its formats",
        "no_encoder" => "this server has no encoder for the format",
        "master_unavailable" => "its file is not where the library last found it",
        _ => return None,
    })
}

/// What a device says of itself in a few words, and how it is marked.
fn state_of(device: &SyncDevice) -> (String, Mark) {
    let attached = device.connection.attached;
    let ring = |status| {
        if attached {
            Mark::here(status)
        } else {
            Mark::away(status)
        }
    };
    if device.activity == ritornel_contract::SyncActivity::Syncing {
        return ("Syncing".to_owned(), Mark::here(Status::Working));
    }
    if device.setup == SyncSetup::Ignored {
        return ("Ignored".to_owned(), ring(Status::Quiet));
    }
    // One not set up is quiet: it is here, and not Ritornel's yet. What
    // its first sync would need is said on its page.
    if device.setup == SyncSetup::New && attached {
        return ("Not set up".to_owned(), Mark::here(Status::Quiet));
    }
    let reasons = &device.attention.reasons;
    let said = |reason: &str| reasons.iter().any(|each| each == reason);
    match device.attention.level {
        SyncAttentionLevel::Failed => {
            let words = if said("files_failed") {
                "Some files failed"
            } else {
                "The last sync failed"
            };
            return (words.to_owned(), ring(Status::Fail));
        }
        SyncAttentionLevel::NeedsYou => {
            let words = if said("root_unbound") {
                "Its folder isn't known here"
            } else if said("marker_gone") {
                "Needs setting up again"
            } else if said("does_not_fit") {
                "Doesn't all fit"
            } else if said("conflicts") {
                "Files changed on it"
            } else {
                "The last sync was cut off"
            };
            return (words.to_owned(), ring(Status::Need));
        }
        SyncAttentionLevel::None => {}
    }
    if !attached {
        return ("Not connected".to_owned(), Mark::away(Status::Quiet));
    }
    match device.to_copy_bytes {
        Some(bytes) if bytes > 0 => (format!("{} to sync", size(bytes)), Mark::here(Status::Ok)),
        _ => ("Up to date".to_owned(), Mark::here(Status::Ok)),
    }
}

fn selector_words(source: &SyncSource) -> (&'static str, String) {
    match source.spec.selector {
        SyncSelector::Everything => ("", "Everything".to_owned()),
        SyncSelector::Artist { .. } => ("Artist", source.label.clone()),
        SyncSelector::Album { .. } => ("Album", source.label.clone()),
        SyncSelector::AddedWithin { days } => ("Added", format!("Last {days} days")),
    }
}

impl DevicesPage {
    pub(crate) fn new(link: &Entity<Link>, home: FocusHandle, cx: &mut Context<Self>) -> Self {
        let followed = cx.subscribe(link, |page: &mut Self, _, event: &LinkEvent, cx| {
            match event {
                LinkEvent::Connected(net) | LinkEvent::Restated(net) => {
                    page.net = Some(Arc::clone(net));
                    if page.freshness.heard(false).is_some() {
                        page.load(cx);
                    }
                }
                LinkEvent::Away => page.net = None,
                LinkEvent::Server(Event::Devices(event)) => page.heard(event, cx),
                // What the library has decides what a sync would do.
                LinkEvent::Server(Event::Library(_)) if page.freshness.heard(false).is_some() => {
                    page.load(cx)
                }
                LinkEvent::Server(_) => {}
            }
        });
        let name = cx.new(|cx| Field::new("Name", cx));
        let named = cx.subscribe(
            &name,
            |page: &mut Self, _, event: &FieldEvent, cx| match event {
                FieldEvent::Entered => page.rename(cx),
                FieldEvent::Escaped => {
                    page.renaming = false;
                    cx.notify();
                }
                _ => {}
            },
        );
        let find = cx.new(|cx| Field::new("Find an artist or an album", cx));
        let finding = cx.subscribe(
            &find,
            |page: &mut Self, _, event: &FieldEvent, cx| match event {
                FieldEvent::Changed(text) => page.search(text.to_string(), cx),
                FieldEvent::Escaped => {
                    page.adding = None;
                    page.found = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        let mut page = Self {
            net: link.read(cx).net(),
            link: link.clone(),
            home,
            list: None,
            failed: false,
            unheld: false,
            chosen: None,
            detail: None,
            view: View::Sync,
            view_of: None,
            rows: None,
            filter: None,
            only_failed: false,
            opened: HashMap::new(),
            formats: None,
            unmanaged: None,
            problem: None,
            forgetting: false,
            reviewing: false,
            renaming: false,
            name,
            adding: None,
            find,
            found: None,
            asked: 0,
            pictures: HashMap::new(),
            wanted: HashSet::new(),
            reading: None,
            reading_rows: None,
            top: 0.,
            elastic: crate::elastic::Elastic::default(),
            height: Rc::new(Cell::new(0.)),
            freshness: crate::stage::Freshness::default(),
            clock: crate::settings::words::Style::of_locale(
                &crate::platform::locale().unwrap_or_else(|| "en-US".to_owned()),
            ),
            _subscriptions: vec![followed, named, finding],
        };
        page.load(cx);
        page
    }

    pub(crate) fn seen(&mut self, seen: bool, cx: &mut Context<Self>) {
        if self.freshness.shown(seen).is_some() {
            self.load(cx);
        }
    }

    pub(crate) fn ready(&self) -> bool {
        self.list.is_some() || self.failed || self.unheld || self.net.is_none()
    }

    /// Scrolls the page by `dy` at once, as a script's jump does.
    pub(crate) fn jump(&mut self, dy: f32, viewport: f32) {
        let max = (self.height.get() - viewport).max(0.);
        self.top = (self.top + dy).clamp(0., max);
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (_, viewport) = viewport_of(window);
        let max = (self.height.get() - viewport).max(0.);
        self.elastic
            .scroll(&mut self.top, crate::elastic::Wheel::of(event), max);
        cx.notify();
    }

    fn ask<M: Method>(
        &self,
        params: M::Params,
    ) -> Option<tokio::task::JoinHandle<Result<M::Result, ritornel_client::Error>>>
    where
        M::Params: Send + Sync + 'static,
        M::Result: Send + 'static,
    {
        let net = self.net.as_ref()?;
        let client = Arc::clone(&net.client);
        Some(
            net.rt()
                .spawn(async move { client.call::<M>(&params).await }),
        )
    }

    /// A `devices` event: the device shown takes its run from it at once
    /// and is read again; any other changes the column.
    fn heard(&mut self, event: &ritornel_contract::SyncDevicesEvent, cx: &mut Context<Self>) {
        if self.freshness.heard(false).is_none() {
            return;
        }
        if Some(event.device_id) == self.chosen
            && let (Some(detail), Some(run)) = (&mut self.detail, &event.run)
        {
            // The run comes whole, so its progress needs no read.
            detail.run = Some(run.clone());
            detail.plan = None;
            self.read_rows(cx);
            cx.notify();
            // Its page is read again when the run ends or its state moves.
            if event
                .changed
                .iter()
                .all(|change| *change == ritornel_contract::SyncDeviceChange::Actions)
            {
                return;
            }
        }
        self.load(cx);
    }

    /// Reads the column, and the page of the device chosen.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.ask::<DevicesList>(NoParams {}) else {
            return;
        };
        self.reading = Some(cx.spawn(async move |this, cx| {
            let answer = list.await;
            let _ = this.update(cx, |page, cx| {
                match answer {
                    Ok(Ok(list)) => {
                        page.failed = false;
                        page.unheld = false;
                        let still = page
                            .chosen
                            .filter(|id| list.devices.iter().any(|device| device.device_id == *id));
                        let chosen = still.or_else(|| list.devices.first().map(|d| d.device_id));
                        if chosen != page.chosen {
                            page.choose(chosen, cx);
                        }
                        page.list = Some(list.devices);
                        page.load_detail(cx);
                    }
                    Ok(Err(error)) if no_library(&error) => {
                        page.unheld = true;
                        page.list = None;
                    }
                    _ => page.failed = page.list.is_none(),
                }
                cx.notify();
            });
        }));
    }

    fn choose(&mut self, device: Option<Id>, cx: &mut Context<Self>) {
        self.chosen = device;
        self.detail = None;
        self.rows = None;
        self.opened.clear();
        self.formats = None;
        self.unmanaged = None;
        self.problem = None;
        self.forgetting = false;
        self.reviewing = false;
        self.renaming = false;
        self.adding = None;
        self.found = None;
        self.filter = None;
        self.only_failed = false;
        self.top = 0.;
        cx.notify();
    }

    /// Goes to a device's page, as a click on its tab does.
    pub(crate) fn go_to(&mut self, device: Id, cx: &mut Context<Self>) {
        if self.chosen != Some(device) {
            self.choose(Some(device), cx);
            self.load_detail(cx);
        }
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        let Some(device) = self.chosen else { return };
        let Some(get) = self.ask::<DevicesGet>(SyncDeviceParams { device_id: device }) else {
            return;
        };
        let formats = self.ask::<DevicesFormats>(SyncDeviceParams { device_id: device });
        cx.spawn(async move |this, cx| {
            let detail = get.await;
            let _ = this.update(cx, |page, cx| {
                if page.chosen != Some(device) {
                    return;
                }
                if let Ok(Ok(detail)) = detail {
                    // A device opens on Music until it is set up, and on
                    // Sync after, unless a view was chosen by hand.
                    if page.view_of != Some(device) {
                        page.view = if detail.device.setup == SyncSetup::New {
                            View::Music
                        } else {
                            View::Sync
                        };
                    }
                    page.detail = Some(detail);
                    page.read_rows(cx);
                    cx.notify();
                }
            });
            if let Some(formats) = formats {
                let formats = formats.await;
                let _ = this.update(cx, |page, cx| {
                    if page.chosen == Some(device)
                        && let Ok(Ok(formats)) = formats
                    {
                        page.formats = Some(formats);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// What the rows shown are of: the run under way or just ended, or the
    /// plan.
    fn rows_key(&self) -> Option<RowsKey> {
        let detail = self.detail.as_ref()?;
        let device = detail.device.device_id;
        let (of, run) = match (&detail.plan, &detail.run) {
            (_, Some(run)) if run.running => (run.run_id.to_string(), true),
            (Some(plan), _) => (plan.plan_revision.clone(), false),
            (None, Some(run)) => (run.run_id.to_string(), true),
            (None, None) => return None,
        };
        Some(RowsKey {
            device,
            of,
            run,
            kind: self.filter,
            failed: self.only_failed,
        })
    }

    fn read_rows(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.rows_key() else {
            self.rows = None;
            return;
        };
        let params = |parent: Option<String>| SyncActionsParams {
            device_id: key.device,
            plan_revision: (!key.run).then(|| key.of.clone()),
            run_id: key.run.then(|| key.of.parse().ok()).flatten(),
            kind: key.kind,
            state: key.failed.then_some(SyncActionState::Failed),
            group_by: SyncGroupBy::Album,
            parent_id: parent,
            offset: 0,
            limit: ROWS,
        };
        let Some(read) = self.ask::<DevicesActions>(params(None)) else {
            return;
        };
        // The groups open are read again with the rows, so theirs move too.
        let opened: Vec<_> = self
            .opened
            .keys()
            .filter_map(|group| {
                Some((
                    group.clone(),
                    self.ask::<DevicesActions>(params(Some(group.clone())))?,
                ))
            })
            .collect();
        self.reading_rows = Some(cx.spawn(async move |this, cx| {
            let read = read.await;
            let mut tracks = Vec::new();
            for (group, read) in opened {
                if let Ok(Ok(read)) = read.await {
                    tracks.push((group, read.rows));
                }
            }
            let _ = this.update(cx, |page, cx| {
                if page.rows_key().as_ref() != Some(&key) {
                    return;
                }
                if let Ok(Ok(read)) = read {
                    page.rows = Some(Rows { key, read });
                    for (group, rows) in tracks {
                        page.opened.insert(group, rows);
                    }
                    page.want_pictures(cx);
                    cx.notify();
                }
            });
        }));
    }

    /// Opens a group to its tracks, or closes it.
    fn toggle(&mut self, group: String, cx: &mut Context<Self>) {
        if self.opened.remove(&group).is_some() {
            cx.notify();
            return;
        }
        self.opened.insert(group, Vec::new());
        self.read_rows(cx);
    }

    /// Asks for the covers the rows and the sources show.
    fn want_pictures(&mut self, cx: &mut Context<Self>) {
        let Some(net) = self.net.clone() else { return };
        let mut arts: Vec<Id> = Vec::new();
        if let Some(rows) = &self.rows {
            arts.extend(rows.read.rows.iter().filter_map(|row| row.art_id));
        }
        if let Some(detail) = &self.detail {
            arts.extend(detail.sources.iter().filter_map(|source| source.art_id));
        }
        if let Some(found) = &self.found {
            arts.extend(found.albums.iter().filter_map(|album| album.art_id));
            arts.extend(found.artists.iter().filter_map(|artist| artist.art_id));
        }
        for art in arts {
            if self.pictures.contains_key(&art) || !self.wanted.insert(art) {
                continue;
            }
            let http = Arc::clone(&net.art);
            let sizes = net.art_sizes.clone();
            let read = net
                .rt()
                .spawn(async move { crate::covers::fetch_square(&http, &sizes, art, 88).await });
            cx.spawn(async move |this, cx| {
                let Ok(Ok((image, _, _))) = read.await else {
                    return;
                };
                let _ = this.update(cx, |page, cx| {
                    page.pictures.insert(art, Arc::new(image));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Sends a command, says what it was refused with, and reads the page
    /// again.
    fn command<M: Method>(&mut self, params: M::Params, cx: &mut Context<Self>)
    where
        M::Params: Send + Sync + 'static,
        M::Result: Send + 'static,
    {
        let Some(sent) = self.ask::<M>(params) else {
            return;
        };
        self.problem = None;
        cx.spawn(async move |this, cx| {
            let answer = sent.await;
            let _ = this.update(cx, |page, cx| {
                match answer {
                    Ok(Ok(_)) => {}
                    Ok(Err(ritornel_client::Error::Call(error))) => {
                        page.problem = Some(error.message.into());
                    }
                    _ => page.problem = Some("The server could not be reached.".into()),
                }
                page.load(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn set(&mut self, change: impl FnOnce(&mut SyncSetParams), cx: &mut Context<Self>) {
        let (Some(device), Ok(key)) = (self.chosen, ritornel_client::idempotency_key()) else {
            return;
        };
        let mut params = SyncSetParams {
            idempotency_key: key,
            device_id: device,
            name: None,
            icon_key: None,
            ignored: None,
            sources: None,
            format: None,
            cover_px: None,
            id3v1: None,
            reserve_bytes: None,
            limit_bytes: None,
            when_full: None,
            on_connect: None,
            made_for: None,
            folder_kind: None,
            codecs: None,
            playlists: None,
        };
        change(&mut params);
        self.formats = None;
        self.command::<DevicesSet>(params, cx);
    }

    /// The sources as they stand, to send back changed.
    fn specs(&self) -> Vec<SyncSourceSpec> {
        self.detail
            .as_ref()
            .map(|detail| detail.sources.iter().map(|source| source.spec).collect())
            .unwrap_or_default()
    }

    fn add_source(&mut self, role: SyncSourceRole, selector: SyncSelector, cx: &mut Context<Self>) {
        let mut sources = self.specs();
        let spec = SyncSourceSpec {
            role,
            selector,
            unit: SyncUnit::Albums,
        };
        if !sources.contains(&spec) {
            sources.push(spec);
            self.set(|params| params.sources = Some(sources), cx);
        }
    }

    fn remove_source(&mut self, source: Id, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let sources: Vec<SyncSourceSpec> = detail
            .sources
            .iter()
            .filter(|each| each.source_id != source)
            .map(|each| each.spec)
            .collect();
        self.set(|params| params.sources = Some(sources), cx);
    }

    fn search(&mut self, text: String, cx: &mut Context<Self>) {
        self.asked += 1;
        let asked = self.asked;
        if text.trim().is_empty() {
            self.found = None;
            cx.notify();
            return;
        }
        let Some(search) = self.ask::<LibrarySearch>(SearchParams {
            text,
            limit: Some(6),
        }) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let found = search.await;
            let _ = this.update(cx, |page, cx| {
                if page.asked == asked
                    && let Ok(Ok(found)) = found
                {
                    page.found = Some(found);
                    page.want_pictures(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn rename(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        self.renaming = false;
        if !name.is_empty() {
            self.set(|params| params.name = Some(name), cx);
        }
        cx.notify();
    }

    /// Starts a sync at the plan shown, setting a new device up first.
    pub(crate) fn sync(&mut self, accept_cut: bool, cx: &mut Context<Self>) {
        let Some(detail) = &self.detail else { return };
        let (Some(plan), Ok(key)) = (&detail.plan, ritornel_client::idempotency_key()) else {
            return;
        };
        let needs_setup = detail.device.setup == SyncSetup::New
            || detail
                .device
                .attention
                .reasons
                .iter()
                .any(|reason| reason == "marker_gone");
        let params = SyncStartParams {
            idempotency_key: key,
            device_id: detail.device.device_id,
            plan_revision: plan.plan_revision.clone(),
            adopt: needs_setup,
            accept_cut,
            skip_retags: false,
        };
        // A sync is watched on Sync.
        self.view = View::Sync;
        self.view_of = Some(detail.device.device_id);
        self.filter = None;
        self.only_failed = false;
        self.command::<DevicesSync>(params, cx);
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let job = self
            .detail
            .as_ref()
            .and_then(|detail| detail.run.as_ref())
            .filter(|run| run.running)
            .and_then(|run| run.job_id);
        if let (Some(job_id), Ok(idempotency_key)) = (job, ritornel_client::idempotency_key()) {
            self.command::<JobsCancel>(
                JobCancelParams {
                    idempotency_key,
                    job_id,
                },
                cx,
            );
        }
    }

    /// Adds the folder at `path` as a device, and goes to its page.
    pub(crate) fn add_folder(&mut self, path: String, cx: &mut Context<Self>) {
        let Ok(key) = ritornel_client::idempotency_key() else {
            return;
        };
        let name = crate::settings::words::folder_name(&path, "Folder");
        let Some(added) = self.ask::<DevicesAddFolder>(SyncAddFolderParams {
            idempotency_key: key,
            path,
            name,
            limit_bytes: None,
            made_for: None,
            kind: None,
        }) else {
            return;
        };
        self.problem = None;
        cx.spawn(async move |this, cx| {
            let added = added.await;
            let _ = this.update(cx, |page, cx| {
                match added {
                    Ok(Ok(added)) => {
                        page.choose(Some(added.device_id), cx);
                        page.view_of = None;
                    }
                    Ok(Err(ritornel_client::Error::Call(error))) => {
                        page.problem = Some(error.message.into());
                    }
                    _ => page.problem = Some("The server could not be reached.".into()),
                }
                page.load(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks for a folder with the system's picker, and adds it.
    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let path = crate::platform::choose_folder(
                handle,
                "Add".to_owned(),
                "Choose a folder, a drive or a card to sync music to. Nothing is written until its first sync."
                    .to_owned(),
                cx,
            )
            .await;
            if let Ok(Some(path)) = path {
                let _ = this.update(cx, |page, cx| {
                    page.add_folder(path.display().to_string(), cx);
                });
            }
        })
        .detach();
    }

    /// A system menu at the click, and what was chosen of it.
    fn menu(
        &self,
        items: Vec<String>,
        checked: Option<usize>,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
        chosen: impl FnOnce(&mut Self, usize, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let handle = window.window_handle();
        let at = event.position();
        cx.spawn(async move |this, cx| {
            let picked = crate::menu::choose(handle, items, checked, at, cx).await;
            if let Some(index) = picked {
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |page, cx| chosen(page, index, window, cx));
                });
            }
        })
        .detach();
    }

    /// Chooses a view by hand, as a click on the control does.
    pub(crate) fn show(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        self.view_of = self.chosen;
        if view == View::Music && self.unmanaged.is_none() {
            self.read_unmanaged(cx);
        }
        cx.notify();
    }

    fn read_unmanaged(&mut self, cx: &mut Context<Self>) {
        let Some(device) = self.chosen else { return };
        let Some(read) = self.ask::<DevicesUnmanaged>(SyncPageParams {
            device_id: device,
            offset: 0,
            limit: 200,
        }) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |page, cx| {
                if page.chosen == Some(device)
                    && let Ok(Ok(read)) = read
                {
                    page.unmanaged = Some(read);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Filters the rows by kind, as a click on a count does.
    pub(crate) fn filter_by(&mut self, kind: Option<SyncActionKind>, cx: &mut Context<Self>) {
        self.filter = kind;
        self.only_failed = false;
        self.opened.clear();
        self.read_rows(cx);
        cx.notify();
    }

    /// Opens the `n`th group of the rows shown, for a script.
    pub(crate) fn open_row(&mut self, n: usize, cx: &mut Context<Self>) {
        let group = self
            .rows
            .as_ref()
            .and_then(|rows| rows.read.rows.get(n))
            .and_then(|row| row.group_id.clone());
        if let Some(group) = group {
            self.toggle(group, cx);
        }
    }

    /// The devices listed, for a script to choose among.
    pub(crate) fn listed(&self) -> Vec<Id> {
        self.list
            .iter()
            .flatten()
            .map(|device| device.device_id)
            .collect()
    }

    /// Asks to forget the device shown, which a second press confirms.
    pub(crate) fn ask_forget(&mut self, cx: &mut Context<Self>) {
        self.forgetting = true;
        cx.notify();
    }
}

// ------------------------------------------------------------------ drawing

/// A small pill of the page's own: a menu's button, a filter, a chip.
fn pill(id: impl Into<gpui::ElementId>, height: f32) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(height))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(7.))
        .whitespace_nowrap()
}

/// A heading of a section of Music, with what stands at its end.
fn heading(title: &'static str, aside: Option<&'static str>, end: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_baseline()
        .gap(px(9.))
        .mb(px(12.))
        .child(
            div()
                .text_size(px(17.))
                .line_height(px(22.))
                .font_weight(FontWeight(650.))
                .child(title),
        )
        .children(aside.map(|aside| {
            div()
                .text_size(px(13.))
                .text_color(theme::faint())
                .child(aside)
        }))
        .child(div().flex_1())
        .children(end)
}

impl DevicesPage {
    fn cover(&self, art: Option<Id>, name: &str, side: f32, window: &Window) -> Div {
        let picture = match art.and_then(|art| self.pictures.get(&art)) {
            Some(picture) => Some(Arc::clone(picture)),
            None if art.is_none() => Some(theme::soft_cached(
                name,
                (side * window.scale_factor()).ceil() as u32,
            )),
            None => None,
        };
        div()
            .size(px(side))
            .flex_none()
            .rounded(px(5.))
            .overflow_hidden()
            .bg(theme::fill())
            .children(picture.map(|picture| {
                img(picture)
                    .size(px(side))
                    .rounded(px(5.))
                    .object_fit(ObjectFit::Cover)
            }))
    }

    /// The column of device tabs.
    fn column(&self, devices: &[SyncDevice], cx: &mut Context<Self>) -> Div {
        let mut column = div()
            .absolute()
            .left(px(COLUMN_LEFT))
            .top(px(20.))
            .w(px(COLUMN))
            .flex()
            .flex_col()
            .gap(px(4.));
        for device in devices {
            let on = self.chosen == Some(device.device_id);
            let (state, mark) = state_of(device);
            let key = device
                .icon
                .key
                .clone()
                .unwrap_or_else(|| device.icon.default.clone());
            let id = device.device_id;
            let tab = div()
                .id(SharedString::from(format!("device-{id}")))
                .w_full()
                .min_h(px(50.))
                .py(px(7.))
                .pl(px(10.))
                .pr(px(14.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .gap(px(10.))
                .when(on, |tab| {
                    tab.bg(theme::raised())
                        .border_1()
                        .border_color(theme::line())
                })
                .when(!on, |tab| {
                    tab.border_1()
                        .border_color(gpui::transparent_black())
                        .hover(theme::hovered)
                        .active(theme::pressed)
                })
                .on_click(cx.listener(move |page, _, _, cx| page.go_to(id, cx)))
                .child(
                    div()
                        .w(px(34.))
                        .flex()
                        .flex_none()
                        .justify_center()
                        .child(art::art(&key, 34., 26.)),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            // A long name wraps to a second line rather
                            // than being cut.
                            div()
                                .text_size(px(14.))
                                .line_height(px(17.))
                                .font_weight(FontWeight(600.))
                                .text_color(theme::ink())
                                .child(device.name.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_size(px(12.))
                                .line_height(px(15.))
                                .text_color(mark.status.text())
                                .whitespace_nowrap()
                                .child(mark.draw(if mark.here { 6. } else { 8. }))
                                .child(state),
                        ),
                );
            column = column.child(tab);
        }
        column.child(
            div()
                .id("device-add")
                .mt(px(2.))
                .ml(px(4.))
                .size(px(36.))
                .rounded(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .hover(theme::hovered)
                .active(theme::pressed)
                .on_click(cx.listener(|page, event: &ClickEvent, window, cx| {
                    page.menu(
                        vec!["Add Folder…".to_owned()],
                        None,
                        event,
                        window,
                        cx,
                        |page, _, window, cx| page.choose_folder(window, cx),
                    );
                }))
                .child(crate::icons::icon("plus", 13., theme::faint())),
        )
    }

    /// The top of a device's page: its drawing, name, state and facts, the
    /// three figures of its next sync, and what can be done.
    fn top(&self, detail: &SyncDeviceDetail, solo: bool, cx: &mut Context<Self>) -> Div {
        let device = &detail.device;
        let (state, mark) = state_of(device);
        let key = device
            .icon
            .key
            .clone()
            .unwrap_or_else(|| device.icon.default.clone());
        let running = detail.run.as_ref().is_some_and(|run| run.running);
        // When it last synced, beside its state.
        let clock = crate::settings::words::Clock {
            now: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(0)),
            zone: jiff::tz::TimeZone::system(),
            style: self.clock,
        };
        let synced = device.last_sync.map(|last| {
            format!(
                "synced {}",
                crate::settings::words::when(last.at_ms, &clock)
            )
        });
        let mut facts: Vec<String> = Vec::new();
        if let Some(folder) = &detail.folder {
            facts.push(
                match folder.kind {
                    SyncFolderKind::Removable => "Removable",
                    SyncFolderKind::AlwaysHere => "Always here",
                }
                .to_owned(),
            );
            facts.push(
                match folder.made_for {
                    SyncMadeFor::CarStereo => "for a car stereo",
                    SyncMadeFor::Phone => "for a phone",
                    SyncMadeFor::OtherPlayer => "for another player",
                    SyncMadeFor::PlainCopy => "a plain copy",
                }
                .to_owned(),
            );
        }
        if let Some(room) = &detail.room {
            facts.push(size(room.capacity_bytes));
        }
        if detail.facts.albums > 0 {
            facts.push(plural(detail.facts.albums, "album"));
        }
        let facts = facts.join(" · ");

        let figures = detail.plan.as_ref().map(|plan| {
            let figure = |value: String, label: &'static str| {
                div()
                    .child(
                        div()
                            .text_size(px(22.))
                            .line_height(px(30.))
                            .font_weight(FontWeight::BOLD)
                            .font_features(crate::bar::tabular())
                            .whitespace_nowrap()
                            .child(value),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(theme::dim())
                            .whitespace_nowrap()
                            .child(label),
                    )
            };
            let nothing = plan.figures.to_copy_bytes == 0;
            div()
                .flex()
                .items_center()
                .gap(px(30.))
                .child(figure(size(plan.figures.to_copy_bytes), "to copy"))
                .child(figure(size(plan.figures.free_after_bytes), "free after"))
                .child(figure(
                    if nothing {
                        "—".to_owned()
                    } else {
                        about(plan.figures.estimated_ms)
                    },
                    if plan.figures.exact {
                        "to sync"
                    } else {
                        "estimated"
                    },
                ))
        });

        // What can be done: one primary button, and none while it runs.
        let mut actions = div().flex().flex_none().items_center().gap(px(9.));
        if running {
            let stopping = detail.run.as_ref().is_some_and(|run| run.stopping);
            actions = actions.child(
                button(
                    "device-stop",
                    if stopping { "Stopping…" } else { "Stop" },
                    Role::Secondary,
                )
                .when(!stopping, |stop| {
                    stop.on_click(cx.listener(|page, _, _, cx| page.stop(cx)))
                }),
            );
        } else {
            let work = detail.plan.as_ref().is_some_and(|plan| {
                let counts = &plan.counts;
                counts.add.tracks + counts.replace.tracks + counts.remove.tracks > 0
            });
            let fits = detail.plan.as_ref().is_none_or(|plan| plan.fits);
            let setting_up = device.setup == SyncSetup::New
                || device.attention.reasons.iter().any(|r| r == "marker_gone");
            let can = device.connection.attached
                && device.setup != SyncSetup::Ignored
                && detail.plan.is_some()
                && (work || setting_up);
            let label = if setting_up {
                "Set up and sync"
            } else {
                "Sync"
            };
            actions = actions.child(
                button("device-sync", label, Role::Primary)
                    .when(!can, |sync| sync.opacity(0.4))
                    .when(can, |sync| {
                        sync.on_click(cx.listener(move |page, _, _, cx| {
                            // A plan that doesn't fit asks first, on Sync.
                            if fits {
                                page.sync(false, cx);
                            } else {
                                page.show(View::Sync, cx);
                            }
                        }))
                    }),
            );
            if detail.capabilities.ejectable && device.connection.attached {
                actions = actions.child(button("device-eject", "Eject", Role::Secondary).on_click(
                    cx.listener(|page, _, _, cx| {
                        if let (Some(device), Ok(key)) =
                            (page.chosen, ritornel_client::idempotency_key())
                        {
                            page.command::<DevicesEject>(
                                SyncDeviceCommandParams {
                                    idempotency_key: key,
                                    device_id: device,
                                },
                                cx,
                            );
                        }
                    }),
                ));
            }
        }
        let ignored = device.setup == SyncSetup::Ignored;
        actions = actions.child(
            div()
                .id("device-more")
                .size(px(36.))
                .rounded_full()
                .bg(theme::fill())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .hover(theme::hovered_on_fill)
                .active(theme::pressed)
                .on_click(cx.listener(move |page, event: &ClickEvent, window, cx| {
                    let mut items = vec![
                        "Rename…".to_owned(),
                        if ignored { "Stop Ignoring" } else { "Ignore" }.to_owned(),
                        "Forget…".to_owned(),
                    ];
                    // With one device there is no column to add from.
                    if solo {
                        items.push("Add Folder…".to_owned());
                    }
                    page.menu(
                        items,
                        None,
                        event,
                        window,
                        cx,
                        move |page, index, window, cx| match index {
                            0 => {
                                let name = page
                                    .detail
                                    .as_ref()
                                    .map(|detail| detail.device.name.clone())
                                    .unwrap_or_default();
                                page.renaming = true;
                                page.name.update(cx, |field, cx| {
                                    field.set(name, window, cx);
                                    field.focus_all(window, cx);
                                });
                                cx.notify();
                            }
                            1 => page.set(|params| params.ignored = Some(!ignored), cx),
                            2 => page.ask_forget(cx),
                            _ => page.choose_folder(window, cx),
                        },
                    );
                }))
                .child(crate::icons::icon("dots", 15., theme::ink())),
        );

        let name: AnyElement = if self.renaming {
            div()
                .h(px(42.))
                .flex()
                .items_center()
                .child(
                    div()
                        .w(px(320.))
                        .h(px(34.))
                        .px(px(12.))
                        .rounded(px(9.))
                        .bg(theme::raised())
                        .border_1()
                        .border_color(theme::acc())
                        .flex()
                        .items_center()
                        .text_size(px(18.))
                        .font_weight(FontWeight(600.))
                        .child(self.name.clone()),
                )
                .into_any_element()
        } else {
            crate::tracked::tracked(
                device.name.clone(),
                30.,
                42.,
                FontWeight::BOLD,
                theme::ink(),
                -0.022,
            )
            .into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap(px(20.))
            .child(div().flex_none().child(art::art(&key, 96., 62.)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(name)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .text_size(px(14.))
                            .line_height(px(20.))
                            .text_color(theme::dim())
                            .child(mark.draw(8.))
                            .child(
                                div()
                                    .font_weight(FontWeight(500.))
                                    .text_color(theme::ink())
                                    .child(state),
                            )
                            .children(synced.map(|synced| {
                                div()
                                    .flex()
                                    .gap(px(7.))
                                    .child(div().text_color(theme::faint()).child("·"))
                                    .child(synced)
                            })),
                    )
                    .child(
                        div()
                            .mt(px(2.))
                            .text_size(px(13.))
                            .line_height(px(18.))
                            .text_color(theme::faint())
                            .truncate()
                            .child(facts),
                    ),
            )
            .children(figures)
            .child(actions)
    }

    /// The views of the page, in the app's segmented control.
    fn views(&self, detail: &SyncDeviceDetail, cx: &mut Context<Self>) -> Div {
        let mut views = vec![(View::Sync, "Sync"), (View::Music, "Music")];
        // A folder records no plays, so it has no Plays.
        if detail.capabilities.plays != SyncPlaysKind::None {
            views.push((View::Plays, "Plays"));
        }
        let mut control = div()
            .flex()
            .p(px(2.))
            .rounded(px(10.))
            .bg(theme::fill())
            .text_size(px(13.));
        for (view, label) in views {
            let base = div()
                .id(label)
                .flex()
                .items_center()
                .justify_center()
                .min_w(px(74.))
                .h(px(28.))
                .px(px(13.))
                .rounded(px(8.))
                .whitespace_nowrap()
                .on_click(cx.listener(move |page, _, _, cx| page.show(view, cx)));
            control = control.child(if self.view == view {
                base.bg(theme::raised())
                    .text_color(theme::ink())
                    .font_weight(FontWeight(600.))
                    .shadow(vec![theme::css_shadow(1., 3., gpui::rgba(0x00000024))])
                    .child(label)
            } else {
                base.text_color(theme::dim())
                    .font_weight(FontWeight(500.))
                    .hover(theme::hovered_ink)
                    .active(theme::pressed)
                    .child(label)
            });
        }
        div().flex().child(control)
    }

    /// One bar of room: what Ritornel has on it, what the sync would add,
    /// what is left alone, and where the room ends.
    fn room(&self, detail: &SyncDeviceDetail) -> Option<Div> {
        let room = detail.room.as_ref()?;
        let capacity = room.capacity_bytes.max(1) as f32;
        let share = |bytes: u64| (bytes as f32 / capacity).clamp(0., 1.);
        let adding = detail
            .plan
            .as_ref()
            .map_or(0, |plan| plan.figures.to_copy_bytes);
        // What stays of what is on it, then what is added.
        let kept = room
            .managed_after_bytes
            .saturating_sub(adding)
            .min(room.managed_bytes);
        let over = room.over_bytes > 0;
        let part = |bytes: u64, color: Rgba| {
            div()
                .h_full()
                .flex_none()
                .w(gpui::relative(share(bytes)))
                .bg(color)
        };
        let soft = Rgba {
            a: 0.45,
            ..theme::acc()
        };
        let key = |color: Rgba, label: &'static str, bytes: u64| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().size(px(8.)).rounded(px(2.)).bg(color))
                .child(label)
                .child(
                    div()
                        .font_weight(FontWeight(600.))
                        .text_color(theme::ink())
                        .child(size(bytes)),
                )
        };
        Some(
            div()
                .child(
                    div()
                        .relative()
                        .h(px(8.))
                        .rounded(px(4.))
                        .overflow_hidden()
                        .bg(theme::fill())
                        .flex()
                        .child(part(kept, theme::acc()))
                        .child(part(adding, if over { theme::need() } else { soft }))
                        .child(part(room.unmanaged_bytes, theme::faint()))
                        // Where the room ends: the capacity less the reserve.
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .bottom_0()
                                .w(px(1.5))
                                .left(gpui::relative(1. - share(room.reserve_bytes).min(0.5)))
                                .bg(theme::dim()),
                        ),
                )
                .child(
                    div()
                        .mt(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(18.))
                        .text_size(px(13.))
                        .text_color(theme::dim())
                        .font_features(crate::bar::tabular())
                        .child(key(theme::acc(), "On it", room.managed_bytes))
                        .child(key(
                            if over { theme::need() } else { soft },
                            "To copy",
                            adding,
                        ))
                        .when(room.unmanaged_bytes > 0, |keys| {
                            keys.child(key(theme::faint(), "Left alone", room.unmanaged_bytes))
                        })
                        .child(div().flex_1())
                        .child(format!(
                            "{} free of {}",
                            size(room.free_bytes),
                            size(room.capacity_bytes)
                        )),
                ),
        )
    }

    /// A tinted box that says what is wrong and what to do.
    fn notice(
        status: Status,
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
    ) -> Div {
        let tint = match status {
            Status::Fail => theme::fail_tint(),
            _ => theme::need_tint(),
        };
        div()
            .px(px(16.))
            .py(px(13.))
            .rounded(px(12.))
            .bg(tint)
            .flex()
            .items_center()
            .gap(px(12.))
            .child(Mark::here(status).draw(8.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .child(
                        div()
                            .font_weight(FontWeight(600.))
                            .text_color(status.text())
                            .child(title.into()),
                    )
                    .child(div().text_color(theme::dim()).child(text.into())),
            )
    }

    /// One row of a plan or a run.
    fn row(
        &self,
        row: &SyncActionRow,
        inner: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (word, color) = match row.state {
            SyncActionState::Done => ("Done", theme::ok_text()),
            SyncActionState::Failed => ("Failed", theme::fail_text()),
            SyncActionState::Copying => ("Copying", theme::working()),
            SyncActionState::Preparing => ("Preparing", theme::working()),
            SyncActionState::InDoubt => ("Unsure", theme::dim()),
            SyncActionState::Skipped => ("Skipped", theme::faint()),
            SyncActionState::Waiting => (
                kind_word(row.kind),
                match row.kind {
                    SyncActionKind::Conflict => theme::need_text(),
                    _ => theme::dim(),
                },
            ),
        };
        let mut said: Vec<String> = Vec::new();
        if let (Some(tracks), Some(of)) = (row.tracks, row.album_tracks) {
            said.push(if tracks >= of {
                plural(u64::from(tracks), "track")
            } else {
                format!("{tracks} of {of} tracks")
            });
        }
        if let Some(codec) = &row.codec {
            said.push(codec.to_uppercase());
        }
        if let Some(error) = &row.error {
            said.push(error.detail.clone());
        } else if let Some(why) = reason_words(&row.reason) {
            said.push(why.to_owned());
        }
        let id = row
            .group_id
            .clone()
            .or_else(|| row.action_id.clone())
            .unwrap_or_default();
        let group = row.group_id.clone();
        let open = group
            .as_ref()
            .is_some_and(|group| self.opened.contains_key(group));
        let removing = row.kind == SyncActionKind::Remove;
        div()
            .id(SharedString::from(format!("row-{}-{id}", u8::from(inner))))
            .h(px(if inner { 38. } else { 50. }))
            .flex()
            .items_center()
            .gap(px(14.))
            .text_size(px(14.))
            .rounded(px(8.))
            .when(inner, |row| row.pl(px(72.)))
            .when(!inner, |row| row.px(px(8.)).mx(px(-8.)))
            .when_some(group, |element, group| {
                element
                    .hover(theme::hovered)
                    .on_click(cx.listener(move |page, _, _, cx| page.toggle(group.clone(), cx)))
            })
            .when(!inner, |element| {
                element
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .text_size(px(13.))
                            .text_color(color)
                            .child(word),
                    )
                    .child(self.cover(row.art_id, &row.title, 36., window))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight(if inner { 400. } else { 500. }))
                            .child(row.title.clone()),
                    )
                    .when(!inner, |title| {
                        title.child(
                            div()
                                .truncate()
                                .text_size(px(12.5))
                                .text_color(theme::dim())
                                .child(row.artist.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(if inner { 380. } else { 320. }))
                    .flex_none()
                    .truncate()
                    .text_size(px(13.))
                    .text_color(if row.error.is_some() {
                        theme::fail_text()
                    } else {
                        theme::dim()
                    })
                    .child(said.join(" · ")),
            )
            .child(
                div()
                    .w(px(84.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .font_weight(FontWeight(500.))
                    .font_features(crate::bar::tabular())
                    .text_color(if inner { theme::dim() } else { theme::ink() })
                    .child(format!(
                        "{}{}",
                        if removing { "−" } else { "" },
                        size(row.bytes)
                    )),
            )
            .when(!inner, |element| {
                element.child(div().w(px(14.)).flex_none().when(
                    row.group_id.is_some(),
                    |chevron| {
                        chevron.child(crate::icons::icon("chevron", 14., theme::faint()).when(
                            !open,
                            |icon| {
                                icon.with_transformation(gpui::Transformation::rotate(
                                    gpui::percentage(-0.25),
                                ))
                            },
                        ))
                    },
                ))
            })
    }

    /// A run under way: its steps, how far it is, and what it is on.
    fn progress(&self, run: &SyncRun) -> Div {
        let step_name = |step: SyncStep| match step {
            SyncStep::Read => "Read",
            SyncStep::Plan => "Plan",
            SyncStep::Adopt => "Set up",
            SyncStep::Copy => "Copy",
            SyncStep::Finish => "Finish",
        };
        let mut steps = div().flex().items_center().gap(px(18.)).text_size(px(13.));
        for step in &run.steps {
            let (mark, color): (AnyElement, Rgba) = match step.state {
                SyncStepState::Done => (
                    crate::icons::icon("check", 11., theme::ok()).into_any_element(),
                    theme::dim(),
                ),
                SyncStepState::Running => (Mark::here(Status::Working).draw(10.), theme::ink()),
                // A step still to come is a dash, not a ring.
                SyncStepState::ToCome => (
                    div()
                        .w(px(8.))
                        .h(px(1.5))
                        .bg(theme::faint())
                        .into_any_element(),
                    theme::faint(),
                ),
            };
            steps = steps.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(color)
                    .when(step.state == SyncStepState::Running, |step| {
                        step.font_weight(FontWeight(600.))
                    })
                    .child(mark)
                    .child(step_name(step.step)),
            );
        }
        let share = if run.bytes_total > 0 {
            (run.bytes_done as f32 / run.bytes_total as f32).clamp(0., 1.)
        } else if run.actions_total > 0 {
            run.actions_done as f32 / run.actions_total as f32
        } else {
            0.
        };
        let mut said = vec![format!(
            "{} of {}",
            thousands(run.actions_done),
            plural(run.actions_total, "file")
        )];
        if run.bytes_total > 0 {
            said.push(format!(
                "{} of {}",
                size(run.bytes_done),
                size(run.bytes_total)
            ));
        }
        if let Some(rate) = run.rate_bytes_per_sec.filter(|rate| *rate > 0) {
            said.push(format!("{}/s", size(rate)));
        }
        if let Some(eta) = run.eta_ms {
            said.push(format!("{} left", about(eta)));
        }
        if run.failed > 0 {
            said.push(format!("{} failed", run.failed));
        }
        let on = run
            .current
            .iter()
            .find(|file| file.state == SyncActionState::Copying)
            .or(run.current.first())
            .map(|file| format!("{} — {}", file.title, file.artist));
        div()
            .p(px(18.))
            .rounded(px(14.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::line())
            .child(steps)
            .child(
                div()
                    .mt(px(14.))
                    .h(px(6.))
                    .rounded(px(3.))
                    .overflow_hidden()
                    .bg(theme::fill())
                    .child(
                        div()
                            .h_full()
                            .w(gpui::relative(share))
                            .rounded(px(3.))
                            .bg(theme::working()),
                    ),
            )
            .child(
                div()
                    .mt(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .text_size(px(13.))
                    .text_color(theme::dim())
                    .font_features(crate::bar::tabular())
                    .child(said.join(" · "))
                    .child(div().flex_1())
                    .children(on.map(|on| div().truncate().max_w(px(420.)).child(on))),
            )
    }

    /// Sync: what happens next, or what is happening.
    fn sync_view(&self, detail: &SyncDeviceDetail, window: &Window, cx: &mut Context<Self>) -> Div {
        let mut view = div().flex().flex_col();
        let running = detail.run.as_ref().filter(|run| run.running);
        if let Some(run) = running {
            view = view.child(self.progress(run)).child(div().h(px(22.)));
        } else if let Some(run) = &detail.run {
            // When it last synced, and what happened.
            let outcome = match run.outcome {
                Some(SyncOutcome::Complete) => "none failed".to_owned(),
                Some(SyncOutcome::FailedInPart) => format!("{} failed", run.failed),
                Some(SyncOutcome::Stopped) => "stopped part way".to_owned(),
                Some(SyncOutcome::CutOff) => "cut off part way".to_owned(),
                Some(SyncOutcome::Failed) | None => "it could not run".to_owned(),
            };
            let done = run.actions_done.saturating_sub(run.failed);
            view = view.child(
                div()
                    .mb(px(16.))
                    .text_size(px(13.))
                    .text_color(theme::dim())
                    .child(format!(
                        "Last sync: {} done · {outcome}{}",
                        plural(done, "file"),
                        run.error
                            .as_ref()
                            .map(|error| format!(" · {error}"))
                            .unwrap_or_default()
                    )),
            );
        }
        if let Some(plan) = &detail.plan {
            if !plan.fits {
                view = view.child(
                    Self::notice(
                        Status::Need,
                        format!(
                            "Not everything fits: {} ({}) would be left off",
                            plural(plan.cut.tracks, "track"),
                            size(plan.cut.bytes)
                        ),
                        "It fills in the order of What goes on, and stops at the first album that doesn't fit. Sync what fits, choose a smaller format in Music, or change what goes on.",
                    )
                    .child(
                        button("sync-what-fits", "Sync what fits", Role::Secondary)
                            .on_click(cx.listener(|page, _, _, cx| page.sync(true, cx))),
                    ),
                );
                view = view.child(div().h(px(18.)));
            }
            let counts = &plan.counts;
            let chip = |label: &'static str, count: u64, kind: Option<SyncActionKind>, on: bool| {
                pill(label, 30.)
                    .px(px(12.))
                    .rounded(px(15.))
                    .text_size(px(14.))
                    .text_color(if on { theme::ink() } else { theme::dim() })
                    .when(on, |chip| chip.bg(theme::fill()))
                    .when(!on, |chip| {
                        chip.hover(theme::hovered).active(theme::pressed)
                    })
                    .on_click(cx.listener(move |page, _, _, cx| page.filter_by(kind, cx)))
                    .child(label)
                    .child(
                        div()
                            .font_weight(FontWeight(600.))
                            .text_color(theme::ink())
                            .font_features(crate::bar::tabular())
                            .child(thousands(count)),
                    )
            };
            let all = counts.add.tracks
                + counts.replace.tracks
                + counts.remove.tracks
                + counts.conflict.tracks
                + counts.blocked.tracks;
            if all > 0 {
                let mut filters = div().flex().items_center().gap(px(4.)).mb(px(8.));
                filters = filters.child(chip("All", all, None, self.filter.is_none()));
                for (label, count, kind) in [
                    ("Add", counts.add.tracks, SyncActionKind::Add),
                    ("Replace", counts.replace.tracks, SyncActionKind::Replace),
                    ("Remove", counts.remove.tracks, SyncActionKind::Remove),
                    (
                        "Conflicts",
                        counts.conflict.tracks,
                        SyncActionKind::Conflict,
                    ),
                    ("Blocked", counts.blocked.tracks, SyncActionKind::Blocked),
                ] {
                    if count > 0 {
                        filters = filters.child(chip(
                            label,
                            count,
                            Some(kind),
                            self.filter == Some(kind),
                        ));
                    }
                }
                view = view.child(filters);
            }
        }
        let shown = self
            .rows
            .as_ref()
            .filter(|rows| Some(&rows.key) == self.rows_key().as_ref());
        match shown {
            Some(rows) if !rows.read.rows.is_empty() => {
                for row in &rows.read.rows {
                    view = view.child(self.row(row, false, window, cx));
                    if let Some(tracks) = row.group_id.as_ref().and_then(|g| self.opened.get(g)) {
                        for track in tracks {
                            view = view.child(self.row(track, true, window, cx));
                        }
                    }
                }
                if rows.read.total > rows.read.rows.len() as u64 {
                    view = view.child(
                        div()
                            .mt(px(8.))
                            .text_size(px(13.))
                            .text_color(theme::faint())
                            .child(format!(
                                "and {} more",
                                thousands(rows.read.total - rows.read.rows.len() as u64)
                            )),
                    );
                }
            }
            Some(_) if running.is_none() => {
                let (title, text) = if detail.device.setup == SyncSetup::New {
                    (
                        "Nothing to copy yet",
                        "Choose what goes on it in Music, then set it up.",
                    )
                } else {
                    (
                        "Nothing to sync",
                        "Everything on it is as the library has it.",
                    )
                };
                view = view.child(
                    div()
                        .py(px(40.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(18.))
                                .font_weight(FontWeight(600.))
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(theme::dim())
                                .child(text),
                        ),
                );
            }
            _ => {}
        }
        view
    }

    /// A labelled button that opens a menu.
    fn menu_button(
        id: &'static str,
        label: &'static str,
        value: impl Into<SharedString>,
        aside: Option<String>,
    ) -> (Div, Stateful<Div>) {
        let button = pill(id, 32.)
            .pl(px(12.))
            .pr(px(9.))
            .rounded(px(8.))
            .bg(theme::fill())
            .hover(theme::hovered_on_fill)
            .active(theme::pressed)
            .text_size(px(14.))
            .font_weight(FontWeight(500.))
            .child(value.into())
            .children(aside.map(|aside| {
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight(400.))
                    .text_color(theme::dim())
                    .child(aside)
            }))
            .child(crate::icons::icon("chevron", 14., theme::acc()));
        (
            div().flex().flex_col().gap(px(6.)).child(
                div()
                    .text_size(px(13.))
                    .text_color(theme::dim())
                    .child(label),
            ),
            button,
        )
    }

    /// Music: what goes on, and in what form.
    fn music_view(
        &self,
        detail: &SyncDeviceDetail,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut view = div().flex().flex_col();

        // What goes on: the sources, in order.
        let add = pill("source-add", 28.)
            .px(px(12.))
            .rounded(px(14.))
            .bg(theme::fill())
            .hover(theme::hovered_on_fill)
            .active(theme::pressed)
            .text_size(px(13.))
            .font_weight(FontWeight(600.))
            .on_click(cx.listener(|page, event: &ClickEvent, window, cx| {
                let items = vec![
                    "Everything".to_owned(),
                    "Added in the Last 30 Days".to_owned(),
                    "Added in the Last 90 Days".to_owned(),
                    "Added in the Last Year".to_owned(),
                    "An Artist or an Album…".to_owned(),
                ];
                page.menu(items, None, event, window, cx, |page, index, window, cx| {
                    let rule = SyncSourceRole::Rule;
                    match index {
                        0 => page.add_source(rule, SyncSelector::Everything, cx),
                        1 => page.add_source(rule, SyncSelector::AddedWithin { days: 30 }, cx),
                        2 => page.add_source(rule, SyncSelector::AddedWithin { days: 90 }, cx),
                        3 => page.add_source(rule, SyncSelector::AddedWithin { days: 365 }, cx),
                        _ => page.find_for(Adding::Rule, window, cx),
                    }
                });
            }))
            .child(crate::icons::icon("plus", 10., theme::ink()))
            .child("Add");
        view = view.child(heading(
            "What goes on",
            Some("in order, until it is full"),
            Some(add.into_any_element()),
        ));
        let rules: Vec<&SyncSource> = detail
            .sources
            .iter()
            .filter(|source| source.spec.role == SyncSourceRole::Rule)
            .collect();
        if rules.is_empty() {
            view = view.child(
                div()
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .text_size(px(14.))
                    .text_color(theme::dim())
                    .child(
                        "Nothing yet. Add everything, or the artists and albums you want on it.",
                    ),
            );
        }
        for (at, source) in rules.iter().enumerate() {
            let (kind, name) = selector_words(source);
            let id = source.source_id;
            view = view.child(
                div()
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .text_size(px(14.))
                    .child(
                        div()
                            .w(px(18.))
                            .flex_none()
                            .text_color(theme::faint())
                            .font_features(crate::bar::tabular())
                            .child((at + 1).to_string()),
                    )
                    .when(source.art_id.is_some(), |row| {
                        row.child(self.cover(source.art_id, &name, 32., window))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .when(!kind.is_empty(), |row| {
                                row.child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(theme::dim())
                                        .child(kind),
                                )
                            })
                            .child(div().truncate().font_weight(FontWeight(600.)).child(name))
                            .children(source.detail.clone().map(|detail| {
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .text_color(theme::dim())
                                    .child(detail)
                            })),
                    )
                    .when(source.cut.tracks > 0, |row| {
                        row.child(
                            div()
                                .flex_none()
                                .text_size(px(13.))
                                .text_color(theme::need_text())
                                .child(format!("{} don't fit", plural(source.cut.tracks, "track"))),
                        )
                    })
                    .child(
                        div()
                            .w(px(110.))
                            .flex_none()
                            .text_size(px(13.))
                            .text_color(theme::dim())
                            .font_features(crate::bar::tabular())
                            .child(plural(source.albums, "album")),
                    )
                    .child(
                        div()
                            .w(px(76.))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .font_weight(FontWeight(500.))
                            .font_features(crate::bar::tabular())
                            .child(size(source.claimed.bytes)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("source-remove-{id}")))
                            .size(px(24.))
                            .rounded_full()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .hover(theme::hovered)
                            .active(theme::pressed)
                            .on_click(cx.listener(move |page, _, _, cx| page.remove_source(id, cx)))
                            .child(crate::icons::icon("close", 9., theme::faint())),
                    ),
            );
        }

        // Leave out, and Always: chips.
        for (label, adding) in [("Leave out", Adding::LeaveOut), ("Always", Adding::Always)] {
            let mut chips = div().flex().flex_wrap().gap(px(6.));
            for source in detail
                .sources
                .iter()
                .filter(|source| source.spec.role == adding.role())
            {
                let (_, name) = selector_words(source);
                let id = source.source_id;
                chips = chips.child(
                    pill(SharedString::from(format!("chip-{id}")), 28.)
                        .pl(px(11.))
                        .pr(px(6.))
                        .rounded(px(14.))
                        .bg(theme::fill())
                        .text_size(px(14.))
                        .font_weight(FontWeight(500.))
                        .child(name)
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight(400.))
                                .text_color(theme::dim())
                                .child(plural(source.claimed.tracks, "track")),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("chip-remove-{id}")))
                                .size(px(18.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .hover(theme::hovered_on_fill)
                                .on_click(
                                    cx.listener(move |page, _, _, cx| page.remove_source(id, cx)),
                                )
                                .child(crate::icons::icon("close", 8., theme::faint())),
                        ),
                );
            }
            chips = chips.child(
                pill(
                    match adding {
                        Adding::LeaveOut => "leave-out-add",
                        _ => "always-add",
                    },
                    28.,
                )
                .px(px(10.))
                .rounded(px(14.))
                .border_1()
                .border_color(theme::line())
                .hover(theme::hovered)
                .active(theme::pressed)
                .on_click(cx.listener(move |page, _, window, cx| page.find_for(adding, window, cx)))
                .child(crate::icons::icon("plus", 9., theme::dim())),
            );
            view = view.child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(
                        div()
                            .w(px(72.))
                            .flex_none()
                            .text_size(px(13.))
                            .text_color(theme::dim())
                            .child(label),
                    )
                    .child(chips),
            );
        }
        if let Some(adding) = self.adding {
            view = view.child(self.finder(adding, window, cx));
        }

        // Already on it: what Ritornel did not write, left alone.
        if detail.unmanaged.files > 0 {
            let open = self.unmanaged.as_ref().filter(|_| self.reviewing);
            view = view.child(
                div()
                    .mt(px(16.))
                    .min_h(px(40.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(14.))
                    .child("Already on it")
                    .child(div().flex_1().text_color(theme::dim()).child(format!(
                        "{} Ritornel didn't write · {} · left alone",
                        plural(detail.unmanaged.files, "file"),
                        size(detail.unmanaged.bytes)
                    )))
                    .child(
                        button(
                            "unmanaged-review",
                            if open.is_some() { "Hide" } else { "Review" },
                            Role::Quiet,
                        )
                        .h(px(28.))
                        .px(px(12.))
                        .on_click(cx.listener(|page, _, _, cx| {
                            page.reviewing = !page.reviewing;
                            if page.unmanaged.is_none() {
                                page.read_unmanaged(cx);
                            }
                            cx.notify();
                        })),
                    ),
            );
            if let Some(unmanaged) = open {
                for file in &unmanaged.files {
                    view = view.child(
                        div()
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .gap(px(14.))
                            .text_size(px(13.))
                            .text_color(theme::dim())
                            .child(div().flex_1().truncate().child(file.path.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .font_features(crate::bar::tabular())
                                    .child(size(file.size_bytes)),
                            ),
                    );
                }
            }
        }

        // Format: what it is made for, and in what form the music goes.
        view = view
            .child(div().h(px(34.)))
            .child(heading("Format", None, None));
        let mut menus = div().flex().flex_wrap().gap_x(px(30.)).gap_y(px(16.));
        if let Some(folder) = detail.folder {
            let (label, made) =
                Self::menu_button("made-for", "Made for", made_for_name(folder.made_for), None);
            let checked = MADE_FOR.iter().position(|each| *each == folder.made_for);
            menus = menus.child(label.child(made.on_click(cx.listener(
                move |page, event: &ClickEvent, window, cx| {
                    let items = MADE_FOR
                        .iter()
                        .map(|m| made_for_name(*m).to_owned())
                        .collect();
                    page.menu(items, checked, event, window, cx, |page, index, _, cx| {
                        if let Some(made_for) = MADE_FOR.get(index).copied() {
                            page.set(|params| params.made_for = Some(made_for), cx);
                        }
                    });
                },
            ))));
        }
        let current = detail.settings.format;
        let (name, how) = format_name(current);
        let takes = self
            .formats
            .as_ref()
            .and_then(|formats| formats.formats.iter().find(|option| option.current))
            .map(|option| size(option.estimated_bytes));
        let aside = [Some(how).filter(|how| !how.is_empty()), takes]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let (label, format) = Self::menu_button("format", "Format", name, Some(aside));
        let options: Vec<SyncFormatOption> = self
            .formats
            .as_ref()
            .map(|formats| formats.formats.clone())
            .unwrap_or_default();
        menus = menus.child(label.child(format.on_click(cx.listener(
            move |page, event: &ClickEvent, window, cx| {
                // One that can't be chosen, or doesn't fit, is listed and
                // says so.
                let offered: Vec<&SyncFormatOption> =
                    options.iter().filter(|option| option.available).collect();
                let items = offered
                    .iter()
                    .map(|option| {
                        let (name, how) = format_name(option.format);
                        format!(
                            "{name} {how} · {}{}",
                            size(option.estimated_bytes),
                            if option.fits { "" } else { " · doesn't fit" }
                        )
                    })
                    .collect();
                let checked = offered.iter().position(|option| option.current);
                let formats: Vec<SyncFormat> = offered.iter().map(|o| o.format).collect();
                page.menu(
                    items,
                    checked,
                    event,
                    window,
                    cx,
                    move |page, index, _, cx| {
                        if let Some(format) = formats.get(index).copied() {
                            page.set(|params| params.format = Some(format), cx);
                        }
                    },
                );
            },
        ))));
        let covers = detail.settings.cover_px;
        let (label, cover) = Self::menu_button(
            "covers",
            "Covers",
            if covers == 0 { "None" } else { "Embedded" },
            (covers > 0).then(|| format!("{covers} px")),
        );
        let sizes = detail.capabilities.cover_sizes.clone();
        menus = menus.child(label.child(cover.on_click(cx.listener(
            move |page, event: &ClickEvent, window, cx| {
                let mut choices: Vec<u32> = sizes.clone();
                choices.push(0);
                let items = choices
                    .iter()
                    .map(|px| match px {
                        0 => "None".to_owned(),
                        px => format!("Embedded · {px} px"),
                    })
                    .collect();
                let checked = choices.iter().position(|px| *px == covers);
                page.menu(
                    items,
                    checked,
                    event,
                    window,
                    cx,
                    move |page, index, _, cx| {
                        if let Some(px) = choices.get(index).copied() {
                            page.set(|params| params.cover_px = Some(px), cx);
                        }
                    },
                );
            },
        ))));
        if let Some(folder) = detail.folder {
            let (label, kind) = Self::menu_button(
                "folder-kind",
                "It is",
                match folder.kind {
                    SyncFolderKind::Removable => "Removable",
                    SyncFolderKind::AlwaysHere => "Always here",
                },
                None,
            );
            let checked = usize::from(folder.kind == SyncFolderKind::AlwaysHere);
            menus = menus.child(label.child(kind.on_click(cx.listener(
                move |page, event: &ClickEvent, window, cx| {
                    let items = vec![
                        "Removable: a drive or a card".to_owned(),
                        "Always here: a folder on this computer".to_owned(),
                    ];
                    page.menu(
                        items,
                        Some(checked),
                        event,
                        window,
                        cx,
                        |page, index, _, cx| {
                            let kind = if index == 0 {
                                SyncFolderKind::Removable
                            } else {
                                SyncFolderKind::AlwaysHere
                            };
                            page.set(|params| params.folder_kind = Some(kind), cx);
                        },
                    );
                },
            ))));
            if folder.kind == SyncFolderKind::AlwaysHere {
                let limit = detail.settings.limit_bytes;
                let (label, room) = Self::menu_button(
                    "limit",
                    "Room",
                    limit.map_or_else(|| "No limit".to_owned(), size),
                    None,
                );
                menus = menus.child(label.child(room.on_click(cx.listener(
                    move |page, event: &ClickEvent, window, cx| {
                        let mut items = vec!["No limit".to_owned()];
                        items.extend(LIMITS.iter().map(|bytes| size(*bytes)));
                        let checked = match limit {
                            None => Some(0),
                            Some(limit) => LIMITS.iter().position(|b| *b == limit).map(|at| at + 1),
                        };
                        page.menu(items, checked, event, window, cx, |page, index, _, cx| {
                            let bytes = match index {
                                0 => 0,
                                index => LIMITS.get(index - 1).copied().unwrap_or(0),
                            };
                            page.set(|params| params.limit_bytes = Some(bytes), cx);
                        });
                    },
                ))));
            }
        }
        let played: Vec<String> = detail
            .capabilities
            .codecs
            .iter()
            .map(|codec| codec.to_uppercase())
            .collect();
        let codecs = match played.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => played.join(""),
        };
        view = view.child(menus).child(
            div()
                .mt(px(14.))
                .text_size(px(13.))
                .line_height(px(19.))
                .text_color(theme::faint())
                .child(format!(
                    "It plays {}. Lossy files are copied as they are, and lossless ones {}. Folders are Artist / Album / 01 Title{}.",
                    codecs,
                    match current.lossless {
                        SyncLossless::AsIs => "too",
                        SyncLossless::Encode { .. } => "are encoded on this library's computer",
                    },
                    detail
                        .path
                        .as_ref()
                        .map(|path| format!(", under {} in {path}", detail.facts.music_root))
                        .unwrap_or_default(),
                )),
        );
        view
    }

    /// Opens the search for an artist or an album to add.
    fn find_for(&mut self, adding: Adding, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = Some(adding);
        self.found = None;
        self.find.update(cx, |field, cx| {
            field.set(String::new(), window, cx);
            field.focus_all(window, cx);
        });
        cx.notify();
    }

    /// The search for an artist or an album, and what it found.
    fn finder(&self, adding: Adding, window: &Window, cx: &mut Context<Self>) -> Div {
        let what = match adding {
            Adding::Rule => "Add to what goes on",
            Adding::Always => "Always put on",
            Adding::LeaveOut => "Leave out",
        };
        let mut panel = div()
            .mt(px(14.))
            .p(px(14.))
            .rounded(px(12.))
            .bg(theme::raised())
            .border_1()
            .border_color(theme::line())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme::dim())
                            .flex_none()
                            .child(what),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(32.))
                            .px(px(12.))
                            .rounded(px(9.))
                            .bg(theme::fill())
                            .flex()
                            .items_center()
                            .text_size(px(14.))
                            .child(self.find.clone()),
                    )
                    .child(
                        button("find-done", "Done", Role::Secondary)
                            .h(px(32.))
                            .on_click(cx.listener(|page, _, window, cx| {
                                page.adding = None;
                                page.found = None;
                                window.focus(&page.home, cx);
                                cx.notify();
                            })),
                    ),
            );
        let Some(found) = &self.found else {
            return panel;
        };
        let role = adding.role();
        let result = |id: SharedString, cover: Div, title: String, sub: String| {
            div()
                .id(id)
                .h(px(44.))
                .px(px(8.))
                .mx(px(-8.))
                .rounded(px(8.))
                .flex()
                .items_center()
                .gap(px(12.))
                .text_size(px(14.))
                .hover(theme::hovered_on_fill)
                .active(theme::pressed)
                .child(cover)
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .font_weight(FontWeight(500.))
                        .child(title),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(13.))
                        .text_color(theme::dim())
                        .child(sub),
                )
        };
        if found.artists.is_empty() && found.albums.is_empty() {
            return panel.child(
                div()
                    .mt(px(12.))
                    .text_size(px(13.))
                    .text_color(theme::dim())
                    .child("No artist or album of that name."),
            );
        }
        panel = panel.child(div().h(px(8.)));
        for artist in &found.artists {
            let name = artist.name.clone().unwrap_or_default();
            let id = artist.artist_id;
            panel = panel.child(
                result(
                    SharedString::from(format!("found-artist-{id}")),
                    self.cover(artist.art_id, &name, 32., window).rounded_full(),
                    name,
                    format!("Artist · {}", plural(artist.album_count, "album")),
                )
                .on_click(cx.listener(move |page, _, _, cx| {
                    page.add_source(role, SyncSelector::Artist { artist_id: id }, cx);
                })),
            );
        }
        for album in &found.albums {
            let title = album.title.clone().unwrap_or_default();
            let id = album.album.id();
            let by = album
                .artist_credit
                .as_ref()
                .map(|credit| credit.text.clone())
                .unwrap_or_default();
            panel = panel.child(
                result(
                    SharedString::from(format!("found-album-{id}")),
                    self.cover(album.art_id, &title, 32., window),
                    title,
                    format!("Album · {by}"),
                )
                .on_click(cx.listener(move |page, _, _, cx| {
                    page.add_source(role, SyncSelector::Album { album_id: id }, cx);
                })),
            );
        }
        panel
    }
}

impl Render for DevicesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (window_width, viewport) = viewport_of(window);
        let width = window_width - crate::upnext::rail(cx);
        let max = (self.height.get() - viewport).max(0.);
        self.top = self.top.clamp(0., max);
        let animating = self.elastic.step(Instant::now());
        let top = self.top + self.elastic.shown();

        let devices = self.list.clone().unwrap_or_default();
        let solo = devices.len() < 2;
        let left = if solo { GUTTER } else { BESIDE_COLUMN };
        let content = (width - left - GUTTER).min(MEASURE);

        let mut body = div().w(px(content)).flex().flex_col();
        if self.list.is_none()
            && let Some(step) = self.link.read(cx).preparing
        {
            let (title, text) = crate::link::preparing_words(step);
            body = body.child(crate::blank::Blank::new(title, text).render(viewport));
        } else if self.unheld {
            body = body.child(
                crate::blank::Blank::new(
                    "No library yet.",
                    "Add your music in Settings, and what you sync it to will be here.",
                )
                .render(viewport),
            );
        } else if self.failed {
            let mut blank = crate::blank::Blank::new(
                "Your devices could not be read.",
                "The server didn't answer. It is asked again when it does.",
            );
            blank.failed = true;
            body = body.child(blank.render(viewport));
        } else if self.list.is_some() && devices.is_empty() {
            let page = cx.entity().downgrade();
            let mut blank = crate::blank::Blank::new(
                "Nothing to sync to yet.",
                "Add a folder, a drive or a card, choose what of your library goes on it, and Ritornel keeps it up to date: tagged, with covers, in a format it plays.",
            );
            blank.button = Some((
                "Add Folder…".into(),
                Box::new(move |_, window, cx| {
                    let _ = page.update(cx, |page, cx| page.choose_folder(window, cx));
                }),
            ));
            body = body
                .child(div().h(px(42.)).mb(px(6.)).child(crate::tracked::tracked(
                    "Devices",
                    30.,
                    42.,
                    FontWeight::BOLD,
                    theme::ink(),
                    -0.022,
                )))
                .child(blank.render(viewport));
            if let Some(problem) = &self.problem {
                body = body.child(
                    div()
                        .mt(px(18.))
                        .flex()
                        .justify_center()
                        .text_size(px(13.5))
                        .text_color(theme::fail_text())
                        .child(problem.clone()),
                );
            }
        } else if let Some(detail) = self.detail.clone() {
            body = body.child(self.top(&detail, solo, cx));
            if let Some(problem) = &self.problem {
                body = body.child(div().mt(px(14.)).child(Self::notice(
                    Status::Fail,
                    "That didn't work",
                    problem.clone(),
                )));
            }
            if self.forgetting {
                let name = detail.device.name.clone();
                body = body.child(
                    div().mt(px(14.)).child(
                        Self::notice(
                            Status::Need,
                            format!("Forget {name}?"),
                            "Its files stay as they are and are no longer Ritornel's. Its past syncs stay in the library's record.",
                        )
                        .child(
                            button("forget-cancel", "Cancel", Role::Secondary).on_click(
                                cx.listener(|page, _, _, cx| {
                                    page.forgetting = false;
                                    cx.notify();
                                }),
                            ),
                        )
                        .child(
                            button("forget-confirm", "Forget", Role::Destructive).on_click(
                                cx.listener(|page, _, _, cx| {
                                    page.forgetting = false;
                                    if let (Some(device), Ok(key)) =
                                        (page.chosen, ritornel_client::idempotency_key())
                                    {
                                        page.command::<DevicesForget>(
                                            SyncDeviceCommandParams {
                                                idempotency_key: key,
                                                device_id: device,
                                            },
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        ),
                    ),
                );
            }
            if let Some(problem) = &detail.device.connection.problem {
                let (title, text) = match problem.as_str() {
                    "root_unbound" => (
                        "Its folder isn't known on this computer",
                        "It was added on another computer, or the library has moved. Add its folder again here to go on syncing it.",
                    ),
                    _ => (
                        "Its folder isn't there",
                        "Plug its drive in, or put the folder back, and it carries on from where it was.",
                    ),
                };
                body = body.child(
                    div()
                        .mt(px(14.))
                        .child(Self::notice(Status::Need, title, text)),
                );
            }
            body = body.child(div().mt(px(22.)).child(self.views(&detail, cx)));
            if let Some(room) = self.room(&detail).filter(|_| self.view != View::Plays) {
                body = body.child(div().mt(px(22.)).child(room));
            }
            body = body.child(div().h(px(26.)));
            body = body.child(match self.view {
                View::Sync => self.sync_view(&detail, window, cx),
                View::Music => self.music_view(&detail, window, cx),
                View::Plays => div()
                    .py(px(40.))
                    .flex()
                    .justify_center()
                    .text_size(px(14.))
                    .text_color(theme::dim())
                    .child("No plays have been read from it yet."),
            });
        }

        let height = Rc::clone(&self.height);
        let head = Rc::new(Cell::new(0f32));
        let start = Rc::clone(&head);
        let page = div()
            .absolute()
            .left_0()
            .top(px(-top))
            .w(px(width))
            .flex()
            .flex_col()
            .child(
                canvas(
                    move |bounds, _, _| start.set(f32::from(bounds.origin.y)),
                    |_, (), _, _| {},
                )
                .h(px(0.))
                .w_full(),
            )
            .child(div().h(px(20.)).flex_none())
            .child(div().pl(px(left)).child(body))
            .child(div().h(px(56.)).flex_none())
            .child(
                canvas(
                    move |bounds, _, _| height.set(f32::from(bounds.origin.y) - head.get()),
                    |_, (), _, _| {},
                )
                .h(px(0.))
                .w_full(),
            );
        if animating {
            window.request_animation_frame();
        }
        div()
            .id("devices")
            .relative()
            .size_full()
            .overflow_hidden()
            .on_scroll(cx.listener(Self::on_scroll))
            .child(page)
            .when(!solo, |shown| shown.child(self.column(&devices, cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_and_a_while_are_said_as_a_page_says_them() {
        assert_eq!(size(0), "Nothing");
        assert_eq!(size(2_600_000_000), "2.6 GB");
        assert_eq!(size(309_000_000_000), "309 GB");
        assert_eq!(size(48_000_000), "48 MB");
        assert_eq!(size(1_950_000_000_000), "2 TB");
        assert_eq!(about(20_000), "< 1 min");
        assert_eq!(about(4 * 60_000), "4 min");
        assert_eq!(about(3 * 3_600_000 + 5 * 60_000), "3 h 5 min");
    }
}
