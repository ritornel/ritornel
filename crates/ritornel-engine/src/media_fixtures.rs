//! The reader against the audio fixtures in `fixtures/audio`, with every
//! hash pinned. Scheme 1 is what Symphonia 0.6.1's demuxers deliver, so an
//! upgrade that moves any of these values fails here, and must come with a
//! new scheme number. `fixtures/audio/generate.sh` says how each file was
//! made.

use std::path::PathBuf;

use ritornel_domain::payload::Payload;
use ritornel_domain::{Facts, extract};

use crate::media::read_file;

pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio")
}

fn read(name: &str) -> Payload {
    read_file(&fixtures(), name, 0, 0, Vec::new())
}

/// Each fixture that is audio: its codec, its duration in milliseconds and
/// its audio hash.
const PINNED: [(&str, &str, u64, &str); 17] = [
    (
        "a.flac",
        "flac",
        500,
        "9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da",
    ),
    (
        "a.retagged.flac",
        "flac",
        500,
        "9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da",
    ),
    (
        "a.id3v1.flac",
        "flac",
        500,
        "9bc10212a894e26f8a4861671767910516f571472def328d116d24a0290c10da",
    ),
    (
        "b.flac",
        "flac",
        500,
        "965afb3a8fd3e3aa0d08901d19024674e12a3c0b71a790183057a893e0b1f6e2",
    ),
    (
        "b.compilation.flac",
        "flac",
        500,
        "965afb3a8fd3e3aa0d08901d19024674e12a3c0b71a790183057a893e0b1f6e2",
    ),
    (
        "a.mp3",
        "mp3",
        500,
        "1459d102a13cf68edec2de615d73cb254511c468e6f365f49ebb3c7c54d586c2",
    ),
    (
        "a.retagged.mp3",
        "mp3",
        500,
        "1459d102a13cf68edec2de615d73cb254511c468e6f365f49ebb3c7c54d586c2",
    ),
    (
        "a.m4a",
        "aac",
        564,
        "033e2ae663c743c4b4341a8706371569905f3babc8f49d53aadb4f820b69fbc4",
    ),
    (
        "a.retagged.m4a",
        "aac",
        564,
        "033e2ae663c743c4b4341a8706371569905f3babc8f49d53aadb4f820b69fbc4",
    ),
    (
        "a.musicbrainz.m4a",
        "aac",
        564,
        "033e2ae663c743c4b4341a8706371569905f3babc8f49d53aadb4f820b69fbc4",
    ),
    (
        "a.alac.m4a",
        "alac",
        500,
        "3f5d53e771def0d20d0f44a965bcad183041a05dd3516991eaddbc19bfa9f05c",
    ),
    (
        "a.ogg",
        "vorbis",
        488,
        "c50924960244c0bb6ce3494e24762d7ff6dec476a7b9557f3ae9500daef628b5",
    ),
    (
        "a.retagged.ogg",
        "vorbis",
        512,
        "c50924960244c0bb6ce3494e24762d7ff6dec476a7b9557f3ae9500daef628b5",
    ),
    (
        "a.opus",
        "opus",
        500,
        "6d0f98393b886f360fffe1a5cc114d755a05090a6b04774bbbb506925e52f737",
    ),
    (
        "a.retagged.opus",
        "opus",
        500,
        "6d0f98393b886f360fffe1a5cc114d755a05090a6b04774bbbb506925e52f737",
    ),
    (
        "a.wav",
        "pcm_s16le",
        500,
        "fc8ad2e52a74b87553ddf13248d50d9e6e27d58a41aa3b190d3324251992fa56",
    ),
    (
        "a.aiff",
        "pcm_s16be",
        500,
        "14aa57b94440a35446e0db483c1aa8c11c724ccbf37cf519a84b6c6c1cd0951e",
    ),
];

#[test]
fn every_fixture_has_the_audio_hash_scheme_1_pinned_for_it() {
    for (name, codec, duration_ms, hash) in PINNED {
        let payload = read(name);
        assert_eq!(payload.error, None, "{name}");
        let audio = payload.audio.unwrap();
        assert_eq!(
            (
                audio.hash_scheme,
                audio.codec.as_str(),
                audio.duration_ms,
                audio.hash.as_str()
            ),
            (1, codec, duration_ms, hash),
            "{name}"
        );
    }
    // Every audio file in the folder is pinned: one added without its hash
    // fails here.
    let mut audio: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.ends_with(".sh") && !name.ends_with(".py") && !name.starts_with("no"))
        .collect();
    audio.sort();
    let mut pinned: Vec<&str> = PINNED.iter().map(|(name, ..)| *name).collect();
    pinned.sort_unstable();
    assert_eq!(audio, pinned);
}

#[test]
fn retagging_never_changes_the_hash_and_other_audio_always_does() {
    let hash = |name: &str| read(name).audio.unwrap().hash;
    for (original, retagged) in [
        ("a.flac", "a.retagged.flac"),
        // An ID3v1 tag after the last frame, which the tail bound hides.
        ("a.flac", "a.id3v1.flac"),
        ("a.mp3", "a.retagged.mp3"),
        ("a.m4a", "a.retagged.m4a"),
        ("a.m4a", "a.musicbrainz.m4a"),
        ("a.ogg", "a.retagged.ogg"),
        ("a.opus", "a.retagged.opus"),
        ("b.flac", "b.compilation.flac"),
    ] {
        assert_eq!(hash(original), hash(retagged), "{retagged}");
        assert_ne!(read(original).tags, read(retagged).tags, "{retagged}");
    }
    let all: std::collections::HashSet<&str> = PINNED.iter().map(|(.., hash)| *hash).collect();
    // Eight encodings of one tone and one of another: nine pieces of audio.
    assert_eq!(all.len(), 9);
}

#[test]
fn a_file_with_no_packets_is_refused_and_has_no_hash() {
    let payload = read("no-packets.m4a");
    assert_eq!(payload.audio, None);
    assert_eq!(payload.container.as_deref(), Some("isomp4"));
    assert!(payload.error.is_some());
    // What could be read of it still was: its tags are kept.
    assert_eq!(
        Facts::from_claims(&extract(&payload)).title.as_deref(),
        Some("Paper Kites")
    );

    let payload = read("not-audio.flac");
    assert_eq!((payload.audio, payload.container), (None, None));
    assert_eq!(payload.error.unwrap().kind, "unsupported");

    let payload = read("there-is-no-such-file.flac");
    assert_eq!(payload.audio, None);
    assert_eq!(payload.error.unwrap().kind, "io");
}

#[test]
fn every_tag_is_read_verbatim_and_the_ones_understood_become_claims() {
    let payload = read("a.flac");
    // A tag nothing understands is in the payload as it was written.
    let invented = payload
        .tags
        .iter()
        .find(|tag| tag.key == "X_INVENTED")
        .unwrap();
    assert_eq!(invented.values, ["not understood, and kept"]);
    let facts = Facts::from_claims(&extract(&payload));
    assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
    assert_eq!(facts.artist, ["Moss Arcade"]);
    assert_eq!(facts.album.as_deref(), Some("Tin Weather"));
    assert_eq!(facts.album_artist, ["Moss Arcade"]);
    assert_eq!((facts.disc, facts.track), (Some(1), Some(2)));
    assert_eq!(facts.date.as_deref(), Some("2019-03-08"));
    assert_eq!(facts.original_date.as_deref(), Some("2004-10-11"));
    assert_eq!(facts.primary_type.as_deref(), Some("ep"));
    assert_eq!(
        facts.recording_mbid.as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    assert_eq!(
        facts.release_mbid.as_deref(),
        Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
    );
    assert_eq!(
        facts.release_group_mbid.as_deref(),
        Some("99999999-8888-4777-8666-555555555555")
    );
    assert_eq!(facts.artist_mbids, ["12121212-3434-4565-8787-909090909090"]);
    assert_eq!(facts.duration_ms, Some(500));
    // Its embedded picture is recorded by hash and size, not by its bytes.
    let [picture] = &payload.pictures[..] else {
        panic!("{:?}", payload.pictures);
    };
    assert_eq!(
        (
            picture.mime.as_str(),
            picture.width,
            picture.height,
            picture.size
        ),
        ("image/png", Some(8), Some(8), 98)
    );
    assert_eq!(
        picture.hash,
        "0ebc8ace303749fdba9171f4656e34ca5145e30c04899d16af582a192cfb64be"
    );
    let audio = payload.audio.unwrap();
    assert_eq!(
        audio.flac_md5.as_deref(),
        Some("9ec1a6a231f8395ec74e464a05dc8a9c")
    );
    assert!(audio.lossless());

    // ID3v2 frames, with Picard's user-defined ones.
    let facts = Facts::from_claims(&extract(&read("a.mp3")));
    assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
    assert_eq!(facts.album_artist, ["Moss Arcade"]);
    assert_eq!(facts.track, Some(2));
    assert_eq!(facts.original_date.as_deref(), Some("2004"));
    assert_eq!(facts.primary_type.as_deref(), Some("album"));
    assert_eq!(facts.secondary_types, ["live"]);
    assert_eq!(
        facts.release_mbid.as_deref(),
        Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
    );
    // An ID3v1 tag holds at most thirty characters of a title, so the
    // ID3v2 frame beside it wins.
    let facts = Facts::from_claims(&extract(&read("a.retagged.mp3")));
    assert_eq!(facts.title.as_deref(), Some("Another Name Entirely"));
    // And the artist both tags name is one artist, named once.
    assert_eq!(facts.artist, ["Somebody Else"]);
    let blocks: Vec<_> = read("a.retagged.mp3")
        .tags
        .iter()
        .filter_map(|tag| tag.block.clone())
        .collect();
    assert!(
        blocks.contains(&"id3v1".to_owned()) && blocks.contains(&"id3v2".to_owned()),
        "{blocks:?}"
    );

    // MP4 atoms.
    let facts = Facts::from_claims(&extract(&read("a.m4a")));
    assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
    assert_eq!(facts.album_artist, ["Moss Arcade"]);
    assert_eq!((facts.disc, facts.track), (Some(1), Some(2)));
    assert!(facts.compilation);
    assert!(!read("a.m4a").audio.unwrap().lossless());

    // A file with no tags it could read claims its name.
    let facts = Facts::from_claims(&extract(&read("a.wav")));
    assert_eq!(
        (facts.title, facts.title_inferred.as_deref()),
        (None, Some("a"))
    );
}

#[test]
fn an_m4a_s_freeform_atoms_are_read_as_picard_writes_them() {
    let payload = read("a.musicbrainz.m4a");
    // The reader names a freeform atom by its mean and its name.
    let named: Vec<String> = payload
        .tags
        .iter()
        .filter(|tag| tag.key.contains("MusicBrainz Track Id"))
        .map(|tag| format!("{}|{:?}", tag.key, tag.sub))
        .collect();
    assert_eq!(named.len(), 1, "{:?}", payload.tags);
    let facts = Facts::from_claims(&extract(&payload));
    assert_eq!(
        facts.recording_mbid.as_deref(),
        Some("11111111-2222-4333-8444-555555555555")
    );
    assert_eq!(
        facts.release_mbid.as_deref(),
        Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
    );
    assert_eq!(
        facts.release_group_mbid.as_deref(),
        Some("99999999-8888-4777-8666-555555555555")
    );
    assert_eq!(facts.artist_mbids, ["12121212-3434-4565-8787-909090909090"]);
    assert_eq!(
        facts.album_artist_mbids,
        ["12121212-3434-4565-8787-909090909090"]
    );
    assert_eq!(facts.artists, ["Moss Arcade"]);
    assert_eq!(facts.original_date.as_deref(), Some("2004-10-11"));
    assert_eq!(facts.primary_type.as_deref(), Some("album"));
    assert_eq!(facts.isrcs, ["XXA000000001"]);
    assert_eq!(facts.acoustids, ["56565656-7878-4909-8121-343434343434"]);
    let claims = extract(&payload);
    assert!(
        claims.iter().any(|claim| claim.name == "mb_track"
            && claim.value == "33333333-4444-4555-8666-777777777777"),
        "{claims:?}"
    );
}
