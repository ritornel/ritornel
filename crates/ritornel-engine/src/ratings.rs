//! The listener's stars and loves, as they stand for each recording. See
//! flags 5 and 6 in docs/design/schema.md: a row is never changed, the
//! newest row of each kind from each source is that source's answer, what
//! is said here names a recording, and what a service reports names a
//! source item and reaches a recording through its current assignment.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Id, LibraryStore, SubjectKind, WriteRequest, Written};

/// What stands for one recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Said {
    /// The listener's stars, said here: 0 to 100, ten to the half star.
    pub rating: Option<i64>,
    /// Whether some source's newest love of it stands.
    pub loved: bool,
}

/// What stands for each of `recordings`, by their canonical keys. One
/// nothing is said of is left out.
pub(crate) fn said(
    connection: &Connection,
    recordings: &[i64],
) -> Result<HashMap<i64, Said>, Error> {
    let wanted: HashSet<i64> = recordings.iter().copied().collect();
    let mut found: HashMap<i64, Said> = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    // Every row of the listener's that reaches a recording, in the order
    // they were said, so that the last seen of each source and subject is
    // its answer. The table is small: one row for each thing said.
    let mut statement = connection.prepare_cached(
        "SELECT lr.kind, coalesce(ra.canonical_id, rr.canonical_id), coalesce(lr.endpoint_id, -1),
                coalesce(lr.source_item_id, -lr.recording_id), lr.value, lr.cleared
         FROM listener_rating lr
         LEFT JOIN assignment a ON a.source_item_id = lr.source_item_id
                               AND a.superseded_at_ms IS NULL
         LEFT JOIN recording ra ON ra.id = a.recording_id
         LEFT JOIN recording rr ON rr.id = lr.recording_id
         WHERE lr.listener_id = (SELECT min(id) FROM listener)
         ORDER BY lr.id",
    )?;
    let mut rows = statement.query([])?;
    let mut loves: HashMap<(i64, i64, i64), bool> = HashMap::new();
    while let Some(row) = rows.next()? {
        let Some(recording) = row.get::<_, Option<i64>>(1)? else {
            continue;
        };
        if !wanted.contains(&recording) {
            continue;
        }
        let kind: String = row.get(0)?;
        let (source, subject): (i64, i64) = (row.get(2)?, row.get(3)?);
        let cleared: bool = row.get(5)?;
        match kind.as_str() {
            // Stars are said here, of the song: the newest of them, on the
            // recording or on one merged into it, stands.
            "rating" if source < 0 => {
                let value: Option<i64> = row.get(4)?;
                found.entry(recording).or_default().rating = value.filter(|_| !cleared);
            }
            "love" => {
                loves.insert((recording, source, subject), cleared);
            }
            _ => {}
        }
    }
    for ((recording, _, _), cleared) in loves {
        if !cleared {
            found.entry(recording).or_default().loved = true;
        }
    }
    Ok(found)
}

impl LibraryStore {
    /// What stands for the recording of each of `recordings`, by their
    /// canonical keys.
    #[cfg(test)]
    pub(crate) fn said_of(&self, recordings: Vec<i64>) -> Result<HashMap<i64, Said>, Error> {
        self.read(move |connection| said(connection, &recordings))
    }

    /// Gives a recording the listener's stars, or takes them away: a new
    /// row, as a rating is never changed. `answer` is given the write's
    /// revision and the recording as it is now, one merged since being
    /// named by the one it was merged into, and returns what the command
    /// answers with.
    pub fn set_rating<F>(
        &self,
        request: WriteRequest,
        recording: Id,
        value: Option<u32>,
        answer: F,
    ) -> Result<Written, Error>
    where
        F: FnOnce(i64, Id) -> String + Send + 'static,
    {
        if value.is_some_and(|value| value > ritornel_contract::MAX_RATING) {
            return Err(Error::Invalid("a rating is 0 to 100"));
        }
        let actor = request
            .client
            .map_or(SubjectKind::Person, |client| client.kind);
        let session = request.client.map(|client| client.session);
        self.write(request, move |write| {
            let connection = write.connection();
            let found: Option<(i64, [u8; 16])> = connection
                .query_row(
                    "SELECT c.id, c.public_id FROM recording r
                     JOIN recording c ON c.id = r.canonical_id
                     WHERE r.public_id = ?1",
                    [recording.as_bytes()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((key, now_id)) = found else {
                return Err(Error::NotFound("no such recording"));
            };
            let listener: Option<i64> =
                connection.query_row("SELECT min(id) FROM listener", [], |row| row.get(0))?;
            let listener = listener.ok_or(Error::Invalid("the library has no listener"))?;
            let actor = match actor {
                SubjectKind::Agent => "agent",
                SubjectKind::Person | SubjectKind::Server => "person",
            };
            connection.execute(
                "INSERT INTO listener_rating
                   (listener_id, recording_id, kind, value, cleared, actor_kind, actor_session_pid,
                    rated_at_ms, rev)
                 VALUES (?1, ?2, 'rating', ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    listener,
                    key,
                    value,
                    value.is_none(),
                    actor,
                    session.as_ref().map(Id::as_bytes),
                    write.now_ms(),
                    write.rev(),
                ],
            )?;
            Ok(answer(write.rev(), Id::from_bytes(now_id)))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock};
    use crate::{Command, Fingerprint, ServerStore};

    fn library() -> (LibraryStore, Vec<(i64, Id)>) {
        let clock = Arc::new(FixedClock::at(1_794_776_400_000));
        let entropy = Arc::new(CountingEntropy::default());
        let server = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let (library, _) = LibraryStore::create_in_memory(
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            clock,
            entropy,
        )
        .unwrap();
        server.claim_library(library.library_id()).unwrap();
        server.add_root("music", "/invented/music").unwrap();
        let (tracks, _) = crate::playback::tests::catalog(
            &library,
            identity.server_id,
            &crate::playback::tests::ALBUM,
        );
        let recordings = tracks
            .iter()
            .map(|track| {
                let shown = library.track(*track).unwrap().unwrap();
                let id = shown.recording.unwrap();
                let key = library
                    .read(move |connection| {
                        Ok(connection.query_row(
                            "SELECT id FROM recording WHERE public_id = ?1",
                            [id.as_bytes()],
                            |row| row.get::<_, i64>(0),
                        )?)
                    })
                    .unwrap();
                (key, id)
            })
            .collect();
        (library, recordings)
    }

    fn rate(library: &LibraryStore, key: u8, recording: Id, value: Option<u32>) -> Written {
        library
            .set_rating(
                WriteRequest {
                    method: "ratings.set",
                    command: Some(Command {
                        id: Id::from_bytes([key; 16]),
                        fingerprint: Fingerprint::of("ratings.set", &[key]),
                    }),
                    client: None,
                },
                recording,
                value,
                |rev, _| rev.to_string(),
            )
            .unwrap()
    }

    #[test]
    fn the_newest_stars_stand_and_taking_them_away_leaves_none() {
        let (library, recordings) = library();
        let [(first, first_id), (second, second_id), ..] = recordings[..] else {
            panic!("the album has four tracks");
        };
        assert!(library.said_of(vec![first]).unwrap().is_empty());
        rate(&library, 1, first_id, Some(60));
        rate(&library, 2, first_id, Some(100));
        rate(&library, 3, second_id, Some(40));
        let said = library.said_of(vec![first, second]).unwrap();
        assert_eq!(said[&first].rating, Some(100));
        assert_eq!(said[&second].rating, Some(40));
        assert!(!said[&first].loved);

        rate(&library, 4, first_id, None);
        let said = library.said_of(vec![first, second]).unwrap();
        assert_eq!(said[&first].rating, None);
        assert_eq!(said[&second].rating, Some(40));
    }

    #[test]
    fn a_rating_sent_again_is_answered_from_its_receipt_and_one_out_of_range_is_refused() {
        let (library, recordings) = library();
        let (key, id) = recordings[0];
        let first = rate(&library, 9, id, Some(80));
        let again = rate(&library, 9, id, Some(80));
        assert!(again.replayed);
        assert_eq!(again.result_json, first.result_json);
        assert_eq!(library.said_of(vec![key]).unwrap()[&key].rating, Some(80));
        let refused = library.set_rating(
            WriteRequest {
                method: "ratings.set",
                command: None,
                client: None,
            },
            id,
            Some(101),
            |rev, _| rev.to_string(),
        );
        assert!(matches!(refused, Err(Error::Invalid(_))));
        let unknown = library.set_rating(
            WriteRequest {
                method: "ratings.set",
                command: None,
                client: None,
            },
            Id::from_bytes([0xee; 16]),
            Some(20),
            |rev, _| rev.to_string(),
        );
        assert!(matches!(unknown, Err(Error::NotFound(_))));
    }
}
