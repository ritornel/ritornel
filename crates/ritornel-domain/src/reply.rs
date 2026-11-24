//! What a catalogue's reply says, read into facts. See "MusicBrainz
//! lookups" in docs/internals/corrections.md.
//!
//! The plugin that fetches a reply does not read it: the reply is kept
//! whole, and read here, so that the facts can be read again from the kept
//! replies, with no plugin and no network, whenever [`READER_VERSION`]
//! changes.

use serde_json::Value;

/// The version of how a reply is read into facts.
pub const READER_VERSION: u32 = 1;

/// One fact a reply states about one release: a field and its value, the
/// way a file's tags state claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// The release it is about, by its MusicBrainz ID.
    pub release: String,
    /// The field, named as a file's claims are where one has the same
    /// meaning: `mb_release_group`, `album_artist`, `disc_number`.
    pub field: &'static str,
    /// The value, as text.
    pub value: String,
    /// The position among the values of a field with several, from 0.
    pub ordinal: u32,
}

/// Why a reply could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable(pub String);

/// The facts MusicBrainz's reply to a lookup of `disc_id` states: for each
/// release the disc is on, what it is, which album it is of, who each is
/// credited to, and which of its media the disc is. A disc MusicBrainz
/// does not know (404, with its JSON error) states nothing. Anything else,
/// or a reply about another disc, is unreadable, and is not a reply.
pub fn read_disc_reply(disc_id: &str, status: u16, body: &str) -> Result<Vec<Fact>, Unreadable> {
    let reply: Value =
        serde_json::from_str(body).map_err(|error| Unreadable(format!("not JSON: {error}")))?;
    match status {
        404 if reply.get("error").is_some_and(Value::is_string) => return Ok(Vec::new()),
        200 => {}
        _ => {
            return Err(Unreadable(format!(
                "not a reply to a disc lookup ({status})"
            )));
        }
    }
    if reply.get("id").and_then(Value::as_str) != Some(disc_id) {
        return Err(Unreadable("a reply about another disc".to_owned()));
    }
    let releases = reply
        .get("releases")
        .and_then(Value::as_array)
        .ok_or_else(|| Unreadable("no releases".to_owned()))?;
    let mut facts = Vec::new();
    for release in releases {
        let Some(id) = text(release, "id") else {
            continue;
        };
        let mut say = |field: &'static str, value: Option<String>, ordinal: u32| {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                facts.push(Fact {
                    release: id.clone(),
                    field,
                    value,
                    ordinal,
                });
            }
        };
        say("title", text(release, "title"), 0);
        say("status", text(release, "status"), 0);
        say("date", text(release, "date"), 0);
        say("country", text(release, "country"), 0);
        say("barcode", text(release, "barcode"), 0);
        let credits = release
            .get("artist-credit")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for (ordinal, credit) in (0..).zip(credits) {
            let artist = credit.get("artist").unwrap_or(&Value::Null);
            say(
                "album_artist",
                text(credit, "name").or_else(|| text(artist, "name")),
                ordinal,
            );
            say("album_artist_join", text(credit, "joinphrase"), ordinal);
            say("mb_album_artist", text(artist, "id"), ordinal);
        }
        if let Some(group) = release.get("release-group") {
            say("mb_release_group", text(group, "id"), 0);
            say("release_group_title", text(group, "title"), 0);
            // The album's own credit, which an edition's may differ from.
            let credits = group
                .get("artist-credit")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for (ordinal, credit) in (0..).zip(credits) {
                let artist = credit.get("artist").unwrap_or(&Value::Null);
                say(
                    "group_artist",
                    text(credit, "name").or_else(|| text(artist, "name")),
                    ordinal,
                );
                say("group_artist_join", text(credit, "joinphrase"), ordinal);
                say("mb_group_artist", text(artist, "id"), ordinal);
            }
            say("primary_type", text(group, "primary-type"), 0);
            say("first_release_date", text(group, "first-release-date"), 0);
            let secondary = group
                .get("secondary-types")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for (ordinal, kind) in (0..).zip(secondary) {
                say("secondary_type", kind.as_str().map(str::to_owned), ordinal);
            }
        }
        let media = release
            .get("media")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        say("medium_count", Some(media.len().to_string()), 0);
        // The media this disc is: usually one, and more for a release
        // that holds the same CD twice.
        let mut ordinal = 0;
        for medium in media {
            let is_this = medium
                .get("discs")
                .and_then(Value::as_array)
                .is_some_and(|discs| {
                    discs
                        .iter()
                        .any(|disc| text(disc, "id").as_deref() == Some(disc_id))
                });
            if !is_this {
                continue;
            }
            let number = |key: &str| {
                medium
                    .get(key)
                    .and_then(Value::as_u64)
                    .map(|n| n.to_string())
            };
            say("disc_number", number("position"), ordinal);
            say("track_count", number("track-count"), ordinal);
            ordinal += 1;
        }
    }
    Ok(facts)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISC: &str = "YuBQA_w4y7cbiR1haQP4UVic4Ko-";
    const BONUS: &str = "oAA1ZY.4QOAq6tpdobDdb4M9qyc-";
    const ONE: &str = "00000000-0000-4000-8000-0000000000c1";
    const DELUXE: &str = "00000000-0000-4000-8000-0000000000c2";

    fn facts_of<'a>(facts: &'a [Fact], release: &str, field: &str) -> Vec<&'a str> {
        facts
            .iter()
            .filter(|fact| fact.release == release && fact.field == field)
            .map(|fact| fact.value.as_str())
            .collect()
    }

    #[test]
    fn a_reply_names_each_release_its_album_its_credit_and_which_medium_the_disc_is() {
        let body = include_str!("../../../fixtures/musicbrainz/disc-tin-weather.json");
        let facts = read_disc_reply(DISC, 200, body).unwrap();
        for release in [ONE, DELUXE] {
            assert_eq!(
                facts_of(&facts, release, "mb_release_group"),
                ["00000000-0000-4000-8000-0000000000b1"]
            );
            assert_eq!(facts_of(&facts, release, "album_artist"), ["Moss Arcade"]);
            assert_eq!(
                facts_of(&facts, release, "mb_album_artist"),
                ["00000000-0000-4000-8000-0000000000a1"]
            );
            assert_eq!(facts_of(&facts, release, "primary_type"), ["Album"]);
            assert_eq!(
                facts_of(&facts, release, "first_release_date"),
                ["2004-05-11"]
            );
            assert_eq!(facts_of(&facts, release, "disc_number"), ["1"]);
            assert_eq!(facts_of(&facts, release, "track_count"), ["3"]);
        }
        assert_eq!(facts_of(&facts, ONE, "medium_count"), ["1"]);
        assert_eq!(facts_of(&facts, DELUXE, "medium_count"), ["2"]);
        assert_eq!(facts_of(&facts, DELUXE, "title"), ["Tin Weather (deluxe)"]);
        assert_eq!(facts_of(&facts, DELUXE, "barcode"), ["0000000000024"]);
        // An empty join phrase says nothing.
        assert!(facts_of(&facts, ONE, "album_artist_join").is_empty());
    }

    #[test]
    fn the_bonus_disc_is_the_second_medium_of_one_release() {
        let body = include_str!("../../../fixtures/musicbrainz/disc-tin-weather-bonus.json");
        let facts = read_disc_reply(BONUS, 200, body).unwrap();
        assert!(facts_of(&facts, ONE, "title").is_empty());
        assert_eq!(facts_of(&facts, DELUXE, "disc_number"), ["2"]);
        assert_eq!(facts_of(&facts, DELUXE, "track_count"), ["2"]);
    }

    #[test]
    fn no_such_disc_states_nothing_and_anything_else_is_unreadable() {
        let body = include_str!("../../../fixtures/musicbrainz/not-found.json");
        assert_eq!(read_disc_reply(DISC, 404, body), Ok(Vec::new()));
        let reply = include_str!("../../../fixtures/musicbrainz/disc-tin-weather.json");
        for (status, body) in [
            (200, "<html>"),
            (200, "{}"),
            (404, "<html>Not Found</html>"),
            (404, "{}"),
            (500, reply),
        ] {
            assert!(
                read_disc_reply(DISC, status, body).is_err(),
                "{status} {body}"
            );
        }
        // A reply about another disc is not this one's.
        assert!(read_disc_reply(BONUS, 200, reply).is_err());
    }

    #[test]
    fn the_albums_own_credit_is_read_with_each_join_at_its_place() {
        let body = r#"{"id":"YuBQA_w4y7cbiR1haQP4UVic4Ko-","releases":[{"id":"r",
          "artist-credit":[{"name":"A","joinphrase":"","artist":{"id":"a"}},
                           {"name":"B","joinphrase":" feat. ","artist":{"id":"b"}},
                           {"name":"C","artist":{"id":"c"}}],
          "release-group":{"id":"g","artist-credit":[{"name":"A","artist":{"id":"a"}}]}}]}"#;
        let facts = read_disc_reply(DISC, 200, body).unwrap();
        let of = |field: &str| -> Vec<(u32, &str)> {
            facts
                .iter()
                .filter(|fact| fact.field == field)
                .map(|fact| (fact.ordinal, fact.value.as_str()))
                .collect()
        };
        assert_eq!(of("album_artist"), [(0, "A"), (1, "B"), (2, "C")]);
        assert_eq!(of("album_artist_join"), [(1, " feat. ")]);
        assert_eq!(of("group_artist"), [(0, "A")]);
        assert_eq!(of("mb_group_artist"), [(0, "a")]);
    }
}
