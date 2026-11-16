//! An invented library in memory, for the engine's tests.

use std::sync::Arc;

use super::{Invented, Plan};
use crate::testing::{CountingEntropy, FixedClock, Scratch};
use crate::{Client, Command, Fingerprint, LibraryStore, ServerStore, SubjectKind, WriteRequest};

pub(crate) const NOW: i64 = 1_794_744_000_000;

pub(crate) struct Library {
    pub(crate) store: LibraryStore,
    pub(crate) invented: Invented,
    pub(crate) servers: ServerStore,
    pub(crate) scratch: Scratch,
}

pub(crate) fn library(albums: usize) -> Library {
    let scratch = Scratch::new();
    let clock = Arc::new(FixedClock::at(NOW));
    let entropy = Arc::new(CountingEntropy::default());
    let servers = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
    let (identity, _) = servers.ensure_identity("desk").unwrap();
    let request = WriteRequest {
        method: "library.create",
        command: Some(Command {
            id: crate::Id::from_bytes([0xc0; 16]),
            fingerprint: Fingerprint::of("library.create", b"{}"),
        }),
        client: Some(Client {
            session: crate::Id::from_bytes([0x5e; 16]),
            kind: SubjectKind::Person,
        }),
    };
    let (store, _) =
        LibraryStore::create_in_memory(&identity, request, |_| String::new(), clock, entropy)
            .unwrap();
    let plan = Plan {
        albums,
        seed: 11,
        root: scratch.path().join("root"),
        now_ms: NOW,
        cover_sides: (100, 140),
    };
    let invented = super::seed(&store, &servers, &plan, |_, _| {}).unwrap();
    Library {
        store,
        invented,
        servers,
        scratch,
    }
}
