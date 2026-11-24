//! The corrections calls: the disc layouts, the library's settings, whose
//! one setting is the switch that lets it be asked about at MusicBrainz,
//! and the lookup of one album. See docs/internals/corrections.md.

use std::sync::{Arc, PoisonError};

use ritornel_contract::methods::{CorrectionsLookup, LibrarySetSettings, Method};
use ritornel_contract::{
    CorrectionsSummary, DiscList, DiscLookup, DiscsParams, ErrorCode, LibrarySettings,
    LibrarySettingsSet, LookupDone, LookupOutcome, LookupParams, PreviewParams, PreviewShown,
    ProposalGetParams, ProposalGot, ProposalList, Receipt, ReceiptStatus, RpcError, SetBy,
    SetLibrarySettingsParams, Setting,
};
use ritornel_engine::{Asked, Command, KeptReply, WriteRequest, reads_as_disc_reply};
use ritornel_source::Catalogue;

use super::playback::refused;
use super::{Core, Stores, fingerprint, revision};

/// The library's settings, from whether a call let it be asked about.
fn settings(lookups: Option<bool>) -> LibrarySettings {
    LibrarySettings {
        lookups: Setting {
            value: lookups == Some(true),
            set_by: if lookups.is_some() {
                SetBy::Call
            } else {
                SetBy::Default
            },
        },
    }
}

/// The catalogue this build asks: MusicBrainz, when it is built in.
/// `test_url` sends every request to a loopback address, for the tests.
#[cfg(feature = "musicbrainz")]
pub(crate) fn catalogue(test_url: Option<&str>) -> Option<Arc<dyn Catalogue>> {
    match ritornel_musicbrainz::MusicBrainz::new(test_url) {
        Ok(musicbrainz) => Some(Arc::new(musicbrainz)),
        Err(error) => {
            tracing::warn!(%error, "MusicBrainz cannot be asked");
            None
        }
    }
}

/// The catalogue this build asks: none, in a build without MusicBrainz.
#[cfg(not(feature = "musicbrainz"))]
pub(crate) fn catalogue(_: Option<&str>) -> Option<Arc<dyn Catalogue>> {
    None
}

fn stored<T: serde::de::DeserializeOwned>(method: &str, json: &str) -> Result<T, RpcError> {
    serde_json::from_str(json).map_err(|error| {
        RpcError::new(
            ErrorCode::Internal,
            format!("the stored result of {method} could not be read: {error}"),
        )
    })
}

impl Core {
    /// Sets the catalogue for this run. Called once, at start.
    pub(crate) fn set_catalogue(&self, catalogue: Arc<dyn Catalogue>) {
        // A second call is a mistake of the caller's, and keeps the first.
        let _ = self.catalogue.set(catalogue);
    }

    /// The catalogue, on a server built with one, once it has started.
    pub(crate) fn catalogue(&self) -> Option<&Arc<dyn Catalogue>> {
        self.catalogue.get()
    }

    /// `corrections.discs`: the disc layouts the sidecars give.
    pub(super) fn corrections_discs(
        &self,
        stores: &Stores,
        params: &DiscsParams,
    ) -> Result<DiscList, RpcError> {
        Self::library(stores, |library| library.discs(params).map_err(refused))
    }

    /// `corrections.summary`.
    pub(super) fn corrections_summary(
        &self,
        stores: &Stores,
    ) -> Result<CorrectionsSummary, RpcError> {
        Self::library(stores, |library| {
            library.corrections_summary().map_err(refused)
        })
    }

    /// `corrections.list`.
    pub(super) fn corrections_list(&self, stores: &Stores) -> Result<ProposalList, RpcError> {
        Self::library(stores, |library| library.proposals().map_err(refused))
    }

    /// `corrections.get`.
    pub(super) fn corrections_get(
        &self,
        stores: &Stores,
        params: &ProposalGetParams,
    ) -> Result<ProposalGot, RpcError> {
        Self::library(stores, |library| {
            let (revision, proposal) = library.proposal(params.proposal_id).map_err(refused)?;
            Ok(ProposalGot {
                library_revision: revision,
                proposal,
            })
        })
    }

    /// `corrections.preview`: read-only.
    pub(super) fn corrections_preview(
        &self,
        stores: &Stores,
        params: &PreviewParams,
    ) -> Result<PreviewShown, RpcError> {
        if params.choices.is_empty() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "name at least one proposal",
            ));
        }
        Self::library(stores, |library| {
            library.preview(&params.choices).map_err(refused)
        })
    }

    /// `library.settings`: whether the library may be asked about, and
    /// whether a call said so.
    pub(super) fn library_settings(&self, stores: &Stores) -> Result<LibrarySettings, RpcError> {
        let set = Self::library(stores, |library| library.lookups_set().map_err(refused))?;
        Ok(settings(set))
    }

    /// `library.setSettings`: the change and its receipt are kept in the
    /// library together.
    pub(super) fn library_set_settings(
        &self,
        stores: &Stores,
        params: &SetLibrarySettingsParams,
    ) -> Result<LibrarySettingsSet, RpcError> {
        let Some(enabled) = params.lookups else {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "name a setting to change: lookups",
            ));
        };
        let key = params.idempotency_key;
        let request = WriteRequest {
            method: LibrarySetSettings::NAME,
            command: Some(Command {
                id: key,
                fingerprint: fingerprint::<LibrarySetSettings>(params)?,
            }),
            client: self.client(),
        };
        let written = Self::library(stores, |library| {
            library
                .set_lookups(request, enabled, move |rev| {
                    let set = LibrarySettingsSet {
                        receipt: Receipt {
                            command_id: key,
                            status: ReceiptStatus::Done,
                            revision: Some(revision(rev)),
                        },
                        settings: settings(Some(enabled)),
                    };
                    serde_json::to_string(&set).unwrap_or_default()
                })
                .map_err(refused)
        })?;
        stored(LibrarySetSettings::NAME, &written.result_json)
    }

    /// `corrections.lookup`: asks the catalogue about the discs of one
    /// album, and keeps each reply. One lookup runs at a time, from its
    /// receipt's check to its commit, so the same question is never sent
    /// twice at once. The library's switch is read again before each
    /// request, so a lookup turned off on the way stops there. The replies
    /// and the receipt are kept in one write: a call sent again with its
    /// key is answered from the receipt and asks nothing.
    pub(super) fn corrections_lookup(
        &self,
        stores: &Stores,
        params: &LookupParams,
    ) -> Result<LookupDone, RpcError> {
        let key = params.idempotency_key;
        let command = Command {
            id: key,
            fingerprint: fingerprint::<CorrectionsLookup>(params)?,
        };
        let _one_at_a_time = self.lookup.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(done) =
            Self::library(stores, |library| library.replay(&command).map_err(refused))?
        {
            return stored(CorrectionsLookup::NAME, &done.result_json);
        }
        if params.album.is_none() == params.folder.is_none() {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "name one album: a release or a folder",
            ));
        }
        let Some(catalogue) = self.catalogue().cloned() else {
            return Err(RpcError::new(
                ErrorCode::Unavailable,
                "this server is built without MusicBrainz",
            )
            .with_detail("reason", "no_catalogue"));
        };
        let off = || {
            RpcError::new(
                ErrorCode::Unavailable,
                "asking MusicBrainz is off for this library: turn it on with library.setSettings",
            )
            .with_detail("reason", "lookups_off")
        };
        let enabled = |stores: &Stores| {
            Self::library(stores, |library| library.lookups_enabled().map_err(refused))
        };
        if !enabled(stores)? {
            return Err(off());
        }
        let (album, root, folder) = (params.album, params.root.clone(), params.folder.clone());
        let discs = Self::library(stores, |library| {
            library.discs_of(album, root, folder).map_err(refused)
        })?;
        let Some(first) = discs.first() else {
            return Err(RpcError::new(
                ErrorCode::NotFound,
                "that album has no disc layout to ask about",
            )
            .with_detail("reason", "no_discs"));
        };
        let question = catalogue.disc(first);
        let asked = Asked {
            catalogue: catalogue.name(),
            kind: question.kind,
            includes: question.includes,
        };
        let mut outcomes: Vec<(LookupOutcome, Option<String>)> = Vec::new();
        let mut replies = Vec::new();
        for disc in &discs {
            let had = Self::library(stores, |library| {
                library.replied(asked, disc).map_err(refused)
            })?;
            if had && !params.again {
                outcomes.push((LookupOutcome::Kept, None));
                continue;
            }
            if !enabled(stores)? {
                return Err(off());
            }
            let reply = catalogue.ask(&catalogue.disc(disc)).and_then(|reply| {
                // Only what reads as a reply about this disc is one.
                if reads_as_disc_reply(disc, reply.status, &reply.body) {
                    Ok(reply)
                } else {
                    Err(ritornel_source::Error::source(
                        "MusicBrainz's reply could not be read",
                    ))
                }
            });
            match reply {
                Ok(reply) => {
                    let outcome = if reply.status == 404 {
                        LookupOutcome::NotFound
                    } else {
                        LookupOutcome::Replied
                    };
                    outcomes.push((outcome, None));
                    replies.push(KeptReply {
                        key: disc.clone(),
                        status: reply.status,
                        body: reply.body,
                    });
                }
                Err(error) => outcomes.push((LookupOutcome::Failed, Some(error.to_string()))),
            }
        }
        let request = WriteRequest {
            method: CorrectionsLookup::NAME,
            command: Some(command),
            client: self.client(),
        };
        let keys = discs.clone();
        let written = Self::library(stores, |library| {
            library
                .keep_replies(request, asked, replies, keys, move |rev, mut albums| {
                    let done = LookupDone {
                        receipt: Receipt {
                            command_id: key,
                            status: ReceiptStatus::Done,
                            revision: Some(revision(rev)),
                        },
                        discs: discs
                            .into_iter()
                            .zip(outcomes)
                            .map(|(disc_id, (outcome, failure))| DiscLookup {
                                albums: albums.remove(&disc_id).unwrap_or_default(),
                                disc_id,
                                outcome,
                                failure,
                            })
                            .collect(),
                    };
                    serde_json::to_string(&done).unwrap_or_default()
                })
                .map_err(refused)
        })?;
        stored(CorrectionsLookup::NAME, &written.result_json)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use ritornel_contract::{CONTRACT_VERSION, ErrorCode, Frame, Request, RequestId, SessionKind};
    use ritornel_engine::{OsEntropy, ScanSettings, ServerStore, SystemClock, run_scan};
    use ritornel_source::{Catalogue, Error, Question, Reply};
    use serde_json::{Value, json};

    use super::{Core, Stores};
    use crate::Bounds;
    use crate::data_dir::DataDir;
    use crate::session::Session;
    use crate::testing::Scratch;

    /// The engine's invented disc: three tracks of 75, 120 and 90 sectors.
    const DISC: &str = "YuBQA_w4y7cbiR1haQP4UVic4Ko-";
    const REPLY: &str = include_str!("../../../../fixtures/musicbrainz/disc-tin-weather.json");

    /// A catalogue of recorded replies: what it was asked, and whether it
    /// fails.
    #[derive(Debug, Default)]
    struct Recorded {
        replies: Mutex<HashMap<String, (u16, String)>>,
        asked: Mutex<Vec<String>>,
        failing: Mutex<bool>,
    }

    impl Catalogue for Recorded {
        fn name(&self) -> &'static str {
            "musicbrainz"
        }

        fn disc(&self, disc_id: &str) -> Question {
            Question {
                kind: "disc",
                key: disc_id.to_owned(),
                includes: "artist-credits+release-groups+recordings",
            }
        }

        fn ask(&self, question: &Question) -> Result<Reply, Error> {
            self.asked.lock().unwrap().push(question.key.clone());
            if *self.failing.lock().unwrap() {
                return Err(Error::source("MusicBrainz is busy (503): ask again later"));
            }
            let (status, body) = self
                .replies
                .lock()
                .unwrap()
                .get(&question.key)
                .cloned()
                .unwrap_or((404, r#"{"error":"Not Found"}"#.to_owned()));
            Ok(Reply { status, body })
        }
    }

    fn call(
        core: &Core,
        session: &Session,
        method: &str,
        params: Value,
    ) -> Result<Value, (ErrorCode, Value)> {
        let request = Frame::Request(Request {
            id: RequestId::Number(1),
            method: method.to_owned(),
            params: Some(params),
        });
        let Some(Frame::Response(response)) = core.dispatch(session, request) else {
            panic!("a request gets a response");
        };
        response.outcome.map_err(|error| {
            (
                error.data.code,
                serde_json::to_value(&error.data.details).unwrap(),
            )
        })
    }

    fn key(byte: u8) -> String {
        ritornel_contract::Id::from_bytes([byte; 16]).to_string()
    }

    /// An AIFF file of CD audio exactly `sectors` long, tagged in an ID3
    /// chunk with its title, artist, album and track.
    fn aiff(sectors: u32, seed: u32, tags: &[(&str, &str)]) -> Vec<u8> {
        let frames = sectors * 588;
        let mut frames_of_tags = Vec::new();
        for (id, text) in tags {
            frames_of_tags.extend_from_slice(id.as_bytes());
            frames_of_tags.extend_from_slice(&(text.len() as u32 + 1).to_be_bytes());
            frames_of_tags.extend_from_slice(&[0, 0, 0]);
            frames_of_tags.extend_from_slice(text.as_bytes());
        }
        let size = frames_of_tags.len() as u32;
        let mut id3 = b"ID3\x03\x00\x00".to_vec();
        id3.extend([21, 14, 7, 0].map(|shift| ((size >> shift) & 0x7f) as u8));
        id3.extend_from_slice(&frames_of_tags);
        if id3.len() % 2 == 1 {
            id3.push(0);
        }
        let mut chunks = b"COMM".to_vec();
        chunks.extend_from_slice(&18u32.to_be_bytes());
        chunks.extend_from_slice(&2u16.to_be_bytes());
        chunks.extend_from_slice(&frames.to_be_bytes());
        chunks.extend_from_slice(&16u16.to_be_bytes());
        chunks.extend_from_slice(&[0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        chunks.extend_from_slice(b"SSND");
        chunks.extend_from_slice(&(8 + frames * 4).to_be_bytes());
        chunks.extend_from_slice(&[0; 8]);
        for frame in 0..frames {
            let sample = (frame.wrapping_mul(seed.wrapping_mul(2_654_435_761)) >> 16) as u16;
            chunks.extend_from_slice(&sample.to_be_bytes());
            chunks.extend_from_slice(&sample.to_be_bytes());
        }
        chunks.extend_from_slice(b"ID3 ");
        chunks.extend_from_slice(&(id3.len() as u32).to_be_bytes());
        chunks.extend_from_slice(&id3);
        let mut file = b"FORM".to_vec();
        file.extend_from_slice(&(4 + chunks.len() as u32).to_be_bytes());
        file.extend_from_slice(b"AIFF");
        file.extend_from_slice(&chunks);
        file
    }

    /// Writes the invented disc, tagged, and its log, into a folder.
    fn write_disc(folder: &std::path::Path) {
        std::fs::create_dir_all(folder).unwrap();
        let mut log =
            String::from("Exact Audio Copy\n Track | Start | Length | Start sector | End sector\n");
        for (n, (start, length)) in [(0u32, 75u32), (75, 120), (195, 90)]
            .into_iter()
            .enumerate()
        {
            let number = (n + 1).to_string();
            let tags = [
                ("TIT2", "A Song"),
                ("TPE1", "Moss Arcade"),
                ("TALB", "Tin Weather"),
                ("TRCK", number.as_str()),
            ];
            std::fs::write(
                folder.join(format!("{number:0>2} Track.aiff")),
                aiff(length, n as u32 + 1, &tags),
            )
            .unwrap();
            log.push_str(&format!(
                "  {number} | 0:00.00 | 0:01.00 | {start} | {}\n",
                start + length - 1
            ));
        }
        std::fs::write(folder.join("Tin Weather.log"), log).unwrap();
    }

    /// A hub with a library of one folder: the invented disc and its log,
    /// scanned. The catalogue is recorded, so nothing is sent anywhere.
    fn hub(catalogue: Option<Arc<Recorded>>) -> (Core, Scratch, Arc<Session>) {
        let scratch = Scratch::new();
        write_disc(&scratch.path().join("music/Moss Arcade/Tin Weather"));

        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let clock = Arc::new(SystemClock);
        let store = ServerStore::open_in_memory(clock.clone(), Arc::new(OsEntropy)).unwrap();
        let (identity, issued) = store.ensure_identity("desk").unwrap();
        let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
        let core = Core::new(
            Stores::new(store, None),
            &identity,
            &dir,
            verifier,
            clock,
            Arc::new(OsEntropy),
            Bounds::default(),
        )
        .unwrap();
        if let Some(catalogue) = catalogue {
            core.set_catalogue(catalogue);
        }
        let session = core.open_session(SessionKind::Full).unwrap();
        let hello = json!({ "name": "test", "kind": "cli", "version": "1", "contractVersion": CONTRACT_VERSION });
        call(&core, &session, "server.hello", hello).unwrap();
        call(
            &core,
            &session,
            "library.create",
            json!({ "idempotencyKey": key(0xc0) }),
        )
        .unwrap();
        let added = call(
            &core,
            &session,
            "sources.add",
            json!({ "idempotencyKey": key(0xc1), "name": "music",
                    "path": scratch.path().join("music").to_str().unwrap() }),
        )
        .unwrap();
        call(
            &core,
            &session,
            "scan.start",
            json!({ "idempotencyKey": key(0xc2), "sourceId": added["source"]["sourceId"] }),
        )
        .unwrap();
        core.with_library(|server, library| {
            let job = server
                .claim_job(ritornel_contract::Id::from_bytes([0xb0; 16]))
                .unwrap()
                .unwrap();
            run_scan(job, server, library, ScanSettings::default()).unwrap();
        })
        .unwrap();
        (core, scratch, session)
    }

    fn lookup(
        core: &Core,
        session: &Session,
        byte: u8,
        again: bool,
    ) -> Result<Value, (ErrorCode, Value)> {
        call(
            core,
            session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(byte), "folder": "Moss Arcade/Tin Weather", "again": again }),
        )
    }

    fn switch(core: &Core, session: &Session, byte: u8, enabled: bool) {
        let set = call(
            core,
            session,
            "library.setSettings",
            json!({ "idempotencyKey": key(byte), "lookups": enabled }),
        )
        .unwrap();
        assert_eq!(
            set["settings"]["lookups"],
            json!({ "value": enabled, "setBy": "call" })
        );
    }

    #[test]
    fn nothing_is_asked_until_the_library_says_it_may_be() {
        let catalogue = Arc::new(Recorded::default());
        let (core, _scratch, session) = hub(Some(catalogue.clone()));
        assert_eq!(
            call(&core, &session, "library.settings", json!({})).unwrap(),
            json!({ "lookups": { "value": false, "setBy": "default" } })
        );
        let refused = lookup(&core, &session, 1, false).unwrap_err();
        assert_eq!(refused.0, ErrorCode::Unavailable);
        assert_eq!(refused.1["reason"], "lookups_off");
        // A change that names no setting is refused.
        assert_eq!(
            call(
                &core,
                &session,
                "library.setSettings",
                json!({ "idempotencyKey": key(9) })
            )
            .unwrap_err()
            .0,
            ErrorCode::Invalid
        );
        switch(&core, &session, 2, true);
        switch(&core, &session, 3, false);
        assert_eq!(
            lookup(&core, &session, 4, false).unwrap_err().1["reason"],
            "lookups_off"
        );
        assert!(catalogue.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_server_built_without_musicbrainz_says_so() {
        let (core, _scratch, session) = hub(None);
        switch(&core, &session, 2, true);
        let refused = lookup(&core, &session, 1, false).unwrap_err();
        assert_eq!(
            (refused.0, refused.1["reason"].clone()),
            (ErrorCode::Unavailable, json!("no_catalogue"))
        );
    }

    #[test]
    fn a_disc_is_asked_about_once_and_its_reply_names_the_album() {
        let catalogue = Arc::new(Recorded::default());
        catalogue
            .replies
            .lock()
            .unwrap()
            .insert(DISC.to_owned(), (200, REPLY.to_owned()));
        let (core, _scratch, session) = hub(Some(catalogue.clone()));
        switch(&core, &session, 2, true);

        let done = lookup(&core, &session, 3, false).unwrap();
        assert_eq!(done["status"], "done");
        let disc = &done["discs"][0];
        assert_eq!(
            (disc["discId"].as_str(), disc["outcome"].as_str()),
            (Some(DISC), Some("replied"))
        );
        assert_eq!(
            disc["albums"],
            json!([{ "releaseGroup": "00000000-0000-4000-8000-0000000000b1", "title": "Tin Weather",
                     "artist": "Moss Arcade", "firstYear": 2004, "releases": 2 }])
        );
        // Sent again with its key: the same answer, and nothing asked.
        assert_eq!(lookup(&core, &session, 3, false).unwrap(), done);
        // Asked again with a new key: the reply is kept, so it is not sent.
        let kept = lookup(&core, &session, 4, false).unwrap();
        assert_eq!(kept["discs"][0]["outcome"], "kept");
        assert_eq!(kept["discs"][0]["albums"], disc["albums"]);
        assert_eq!(*catalogue.asked.lock().unwrap(), [DISC]);
        // Unless asked to.
        lookup(&core, &session, 5, true).unwrap();
        assert_eq!(catalogue.asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn no_such_disc_is_kept_as_a_reply_and_a_failure_is_not() {
        let catalogue = Arc::new(Recorded::default());
        let (core, _scratch, session) = hub(Some(catalogue.clone()));
        switch(&core, &session, 2, true);

        *catalogue.failing.lock().unwrap() = true;
        let failed = lookup(&core, &session, 3, false).unwrap();
        assert_eq!(failed["discs"][0]["outcome"], "failed");
        assert_eq!(
            failed["discs"][0]["failure"],
            "MusicBrainz is busy (503): ask again later"
        );

        *catalogue.failing.lock().unwrap() = false;
        let unknown = lookup(&core, &session, 4, false).unwrap();
        assert_eq!(unknown["discs"][0]["outcome"], "not_found");
        assert_eq!(unknown["discs"][0]["albums"], json!([]));
        // "Not found" is an answer: it is not asked again.
        assert_eq!(
            lookup(&core, &session, 5, false).unwrap()["discs"][0]["outcome"],
            "kept"
        );
        assert_eq!(catalogue.asked.lock().unwrap().len(), 2);
    }

    #[test]
    fn an_album_names_one_folder_or_one_release_and_must_have_a_disc() {
        let catalogue = Arc::new(Recorded::default());
        let (core, _scratch, session) = hub(Some(catalogue));
        switch(&core, &session, 2, true);
        let neither = call(
            &core,
            &session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(3) }),
        );
        assert_eq!(neither.unwrap_err().0, ErrorCode::Invalid);
        let nowhere = call(
            &core,
            &session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(4), "folder": "Elsewhere" }),
        );
        assert_eq!(nowhere.unwrap_err().1["reason"], "no_discs");
    }

    #[test]
    fn a_release_names_the_discs_of_the_folders_its_files_are_in() {
        let catalogue = Arc::new(Recorded::default());
        let (core, _scratch, session) = hub(Some(catalogue.clone()));
        switch(&core, &session, 2, true);
        let found = call(
            &core,
            &session,
            "library.search",
            json!({ "text": "A Song" }),
        )
        .unwrap();
        let release = found["songs"][0]["releaseId"].clone();
        assert!(release.is_string(), "{found}");
        let done = call(
            &core,
            &session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(3), "album": release }),
        )
        .unwrap();
        assert_eq!(done["discs"][0]["discId"], DISC);
        assert_eq!(*catalogue.asked.lock().unwrap(), [DISC]);
        let unknown = call(
            &core,
            &session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(4), "album": key(0x77) }),
        );
        assert_eq!(unknown.unwrap_err().0, ErrorCode::NotFound);
    }

    #[test]
    fn a_folder_two_roots_have_is_asked_about_only_in_the_root_named() {
        let catalogue = Arc::new(Recorded::default());
        let (core, scratch, session) = hub(Some(catalogue.clone()));
        switch(&core, &session, 2, true);
        // A second root with a folder of the same path and the same disc.
        write_disc(&scratch.path().join("other/Moss Arcade/Tin Weather"));
        let added = call(
            &core,
            &session,
            "sources.add",
            json!({ "idempotencyKey": key(0xd1), "name": "other",
                    "path": scratch.path().join("other").to_str().unwrap() }),
        )
        .unwrap();
        call(
            &core,
            &session,
            "scan.start",
            json!({ "idempotencyKey": key(0xd2), "sourceId": added["source"]["sourceId"] }),
        )
        .unwrap();
        core.with_library(|server, library| {
            let job = server
                .claim_job(ritornel_contract::Id::from_bytes([0xb1; 16]))
                .unwrap()
                .unwrap();
            run_scan(job, server, library, ScanSettings::default()).unwrap();
        })
        .unwrap();
        let either = lookup(&core, &session, 3, false).unwrap_err();
        assert_eq!(either.0, ErrorCode::Invalid);
        assert!(catalogue.asked.lock().unwrap().is_empty());
        let named = call(
            &core,
            &session,
            "corrections.lookup",
            json!({ "idempotencyKey": key(4), "folder": "Moss Arcade/Tin Weather", "root": "other" }),
        );
        assert_eq!(named.unwrap()["discs"][0]["outcome"], "not_found");
    }

    #[test]
    fn a_reply_that_does_not_read_as_one_about_the_disc_is_a_failure_and_is_not_kept() {
        let catalogue = Arc::new(Recorded::default());
        catalogue.replies.lock().unwrap().insert(
            DISC.to_owned(),
            (200, r#"{"id":"another","releases":[]}"#.to_owned()),
        );
        let (core, _scratch, session) = hub(Some(catalogue.clone()));
        switch(&core, &session, 2, true);
        let failed = lookup(&core, &session, 3, false).unwrap();
        assert_eq!(failed["discs"][0]["outcome"], "failed");
        assert_eq!(
            failed["discs"][0]["failure"],
            "MusicBrainz's reply could not be read"
        );
        // Nothing was kept, so it is asked again.
        lookup(&core, &session, 4, false).unwrap();
        assert_eq!(catalogue.asked.lock().unwrap().len(), 2);
    }

    /// The real client against a fake MusicBrainz on loopback.
    #[cfg(feature = "musicbrainz")]
    #[test]
    fn the_server_asks_musicbrainz_through_its_client() {
        use ritornel_musicbrainz::USER_AGENT;
        use ritornel_musicbrainz::fake::FakeMusicBrainz;

        let fake = FakeMusicBrainz::start();
        fake.reply(DISC, 200, REPLY);
        let (core, _scratch, session) = hub(None);
        core.set_catalogue(super::catalogue(Some(&fake.base_url())).unwrap());
        switch(&core, &session, 2, true);
        let done = lookup(&core, &session, 3, false).unwrap();
        assert_eq!(done["discs"][0]["albums"][0]["title"], "Tin Weather");
        let seen = fake.seen();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].path.starts_with(&format!("/ws/2/discid/{DISC}?")));
        assert_eq!(seen[0].user_agent, USER_AGENT);
    }

    #[test]
    fn a_disc_asked_about_is_proposed_and_previewed_and_the_preview_writes_nothing() {
        let catalogue = Arc::new(Recorded::default());
        catalogue
            .replies
            .lock()
            .unwrap()
            .insert(DISC.to_owned(), (200, REPLY.to_owned()));
        let (core, _scratch, session) = hub(Some(catalogue));
        let summary = call(&core, &session, "corrections.summary", json!({})).unwrap();
        assert_eq!(
            (summary["open"].as_u64(), summary["lookups"].as_bool()),
            (Some(0), Some(false))
        );
        switch(&core, &session, 2, true);
        lookup(&core, &session, 3, false).unwrap();
        let summary = call(&core, &session, "corrections.summary", json!({})).unwrap();
        assert_eq!(
            (summary["open"].as_u64(), summary["lookups"].as_bool()),
            (Some(1), Some(true))
        );
        let list = call(&core, &session, "corrections.list", json!({})).unwrap();
        let proposal = &list["proposals"][0];
        assert_eq!(proposal["folder"], "Moss Arcade/Tin Weather");
        assert_eq!(proposal["candidates"][0]["title"], "Tin Weather");
        let id = proposal["proposalId"].clone();
        let got = call(
            &core,
            &session,
            "corrections.get",
            json!({ "proposalId": id }),
        )
        .unwrap();
        assert_eq!(got["proposal"]["restsOn"][0]["discId"], DISC);
        assert_eq!(got["proposal"]["files"].as_array().unwrap().len(), 3);
        let revision = got["libraryRevision"].clone();
        let shown = call(
            &core,
            &session,
            "corrections.preview",
            json!({ "choices": [{ "proposalId": id }] }),
        )
        .unwrap();
        assert_eq!(shown["after"][0]["primaryType"], "album");
        assert_eq!(shown["after"][0]["firstYear"], 2004);
        assert_eq!(shown["libraryRevision"], revision);
        let none = call(
            &core,
            &session,
            "corrections.preview",
            json!({ "choices": [] }),
        );
        assert_eq!(none.unwrap_err().0, ErrorCode::Invalid);
        let unknown = call(
            &core,
            &session,
            "corrections.get",
            json!({ "proposalId": key(0x55) }),
        );
        assert_eq!(unknown.unwrap_err().0, ErrorCode::NotFound);
    }
}
