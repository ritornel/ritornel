//! A picture folder of invented portraits, imported for a scanned library:
//! matched by ID and by name, served in place, and dropped when the folder
//! no longer chooses them.

use std::path::Path;
use std::sync::{Arc, Mutex};

use ritornel_contract::{ArtistRow, Id, ListKind, Query, Row};

use super::{PicturesImported, safe_name};
use crate::covers::Covers;
use crate::scan_tests::Rig;
use crate::testing::Scratch;
use crate::{Command, Error, Fingerprint, WriteRequest};

/// The ID `a.flac` gives its artist, Moss Arcade.
const MOSS_ARCADE: &str = "12121212-3434-4565-8787-909090909090";

/// A JPEG of one colour.
fn jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode_image(&image)
        .unwrap();
    bytes
}

/// Writes an artist's folder: a portrait and a manifest that chooses it.
fn portrait(folder: &Path, dir: &str, file: &str, bytes: &[u8]) {
    let dir = folder.join(dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), bytes).unwrap();
    let manifest = serde_json::json!({
        "format": 1,
        "artist": { "name": "invented", "unknown": "ignored" },
        "files": [{ "file": file, "kind": "portrait", "source": "owner" }],
        "chosen": {
            "portrait": { "file": file, "by": "owner", "at": "2026-01-01", "why": "test" },
            "wide": { "file": "absent.jpg", "by": "rule:v1" }
        },
        "a field from a later format": true
    });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

/// A library of two artists, Moss Arcade with an ID and Various Artists with
/// none, and an empty picture folder that is a root of the server.
fn rig() -> (Rig, Scratch) {
    let mut rig = Rig::new();
    rig.put("Moss Arcade/Tin Weather/02.flac", "a.flac");
    rig.put("Various Artists/Night Drives/07.flac", "b.compilation.flac");
    rig.scan();
    let folder = Scratch::new();
    std::fs::write(
        folder.path().join("index.json"),
        r#"{"format": 1, "artists": []}"#,
    )
    .unwrap();
    rig.server
        .add_root("artists", folder.path().to_str().unwrap())
        .unwrap();
    (rig, folder)
}

fn import(rig: &Rig, folder: &Path, key: u8) -> Result<PicturesImported, Error> {
    import_checked(rig, &super::check_picture_folder(folder)?, key)
}

fn import_checked(
    rig: &Rig,
    folder: &super::PictureFolder,
    key: u8,
) -> Result<PicturesImported, Error> {
    let request = WriteRequest {
        method: "artists.importPictures",
        command: Some(Command {
            id: Id::from_bytes([key; 16]),
            fingerprint: Fingerprint::of("test", &[key]),
        }),
        client: None,
    };
    let slot = Arc::new(Mutex::new(PicturesImported::default()));
    let kept = Arc::clone(&slot);
    rig.library
        .import_artist_pictures(request, "artists", folder, move |counts, _| {
            *kept.lock().unwrap() = *counts;
            String::new()
        })?;
    Ok(*slot.lock().unwrap())
}

fn artists(rig: &Rig) -> Vec<ArtistRow> {
    let view = rig
        .library
        .list_open(ListKind::Artists, &Query::default())
        .unwrap();
    rig.library
        .list_rows(ListKind::Artists, &view.ids)
        .unwrap()
        .into_iter()
        .filter_map(|row| match row {
            Row::Artist(artist) => Some(artist),
            _ => None,
        })
        .collect()
}

fn named<'a>(rows: &'a [ArtistRow], name: &str) -> &'a ArtistRow {
    rows.iter()
        .find(|row| row.name.as_deref() == Some(name))
        .unwrap()
}

#[test]
fn portraits_are_matched_by_id_and_by_name_and_drawn_in_place_of_covers() {
    let (rig, folder) = rig();
    let before = artists(&rig);
    let cover = named(&before, "Moss Arcade").art_id;
    assert!(cover.is_some());
    assert!(!named(&before, "Moss Arcade").portrait);

    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(30, 30, [200, 40, 40]),
    );
    portrait(
        folder.path(),
        "by-name/Various Artists",
        "me.jpg",
        &jpeg(20, 20, [40, 40, 200]),
    );
    // An artist with an ID is never found by name.
    portrait(
        folder.path(),
        "by-name/Moss Arcade",
        "wrong.jpg",
        &jpeg(10, 10, [0, 0, 0]),
    );
    let counts = import(&rig, folder.path(), 0xa1).unwrap();
    assert_eq!(
        counts,
        PicturesImported {
            artists: 2,
            by_mbid: 1,
            by_name: 1,
            without: 0,
            unreadable: 0,
            dropped: 0,
            ambiguous: 0,
        }
    );
    let after = artists(&rig);
    let moss = named(&after, "Moss Arcade");
    assert!(moss.portrait);
    assert_ne!(moss.art_id, cover);
    assert!(named(&after, "Various Artists").portrait);
    assert_eq!(
        rig.rows("SELECT matched_by, path, source, chosen_by FROM artist_picture ORDER BY id"),
        [
            format!("mbid|{MOSS_ARCADE}/portrait.jpg|owner|owner"),
            "name|by-name/Various Artists/me.jpg|owner|owner".to_owned(),
        ]
    );

    // It is served from the folder at a cover's sizes, with its colour.
    let cache = Scratch::new();
    let covers = Covers::new(cache.path()).unwrap();
    let art = moss.art_id.unwrap();
    let rendered = covers.render(&rig.library, &rig.server, art, 64).unwrap();
    let decoded = image::load_from_memory(&rendered.bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (30, 30));
    let color = covers
        .color(&rig.library, &rig.server, art)
        .unwrap()
        .unwrap();
    assert!(color.hue < 10 || color.hue > 350, "{color:?}");
}

#[test]
fn an_import_again_changes_nothing_and_a_portrait_no_longer_chosen_is_dropped() {
    let (rig, folder) = rig();
    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(30, 30, [200, 40, 40]),
    );
    import(&rig, folder.path(), 0xa1).unwrap();
    let rows = "SELECT artist_id, picture_id, rev FROM artist_picture ORDER BY id";
    let first = rig.rows(rows);
    assert_eq!(first.len(), 1);
    let again = import(&rig, folder.path(), 0xa2).unwrap();
    assert_eq!((again.by_mbid, again.dropped), (1, 0));
    assert_eq!(rig.rows(rows), first);

    // A picture replaced under the same name is another picture.
    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(30, 30, [40, 200, 40]),
    );
    import(&rig, folder.path(), 0xa3).unwrap();
    assert_ne!(rig.rows(rows), first);

    // The manifest chooses none: the portrait goes, and the cover is back.
    std::fs::write(
        folder.path().join(MOSS_ARCADE).join("manifest.json"),
        r#"{"format": 1, "files": [], "chosen": {}}"#,
    )
    .unwrap();
    let counts = import(&rig, folder.path(), 0xa4).unwrap();
    assert_eq!((counts.by_mbid, counts.without, counts.dropped), (0, 2, 1));
    assert!(rig.rows(rows).is_empty());
    let moss = artists(&rig);
    let moss = named(&moss, "Moss Arcade");
    assert!(!moss.portrait && moss.art_id.is_some());
}

#[test]
fn a_portrait_that_cannot_be_read_is_counted_and_a_folder_of_another_kind_refused() {
    let (rig, folder) = rig();
    portrait(folder.path(), MOSS_ARCADE, "portrait.jpg", b"not an image");
    // A manifest may not reach out of its folder.
    let dir = folder.path().join("by-name/Various Artists");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"format": 1, "chosen": {"portrait": {"file": "../../index.json"}}}"#,
    )
    .unwrap();
    let counts = import(&rig, folder.path(), 0xa1).unwrap();
    assert_eq!((counts.unreadable, counts.without), (2, 2));

    let elsewhere = Scratch::new();
    assert!(matches!(
        import(&rig, elsewhere.path(), 0xa2),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn a_name_is_made_safe_as_the_picture_folder_makes_it() {
    assert_eq!(safe_name("  AC/DC "), "AC_DC");
    assert_eq!(safe_name("...And You Will Know Us"), "And You Will Know Us");
    assert_eq!(safe_name("What?: \"Yes\" <|> *\\"), "What__ _Yes_ ___ __");
    assert_eq!(safe_name("..."), "_");
    assert_eq!(safe_name("Cafe\u{301}"), "Caf\u{e9}");
    assert_eq!(safe_name(&"x".repeat(130)).chars().count(), 120);
}

/// Runs an import on a thread of its own, and fails rather than waits if
/// it has not finished in ten seconds.
fn import_within(rig: Rig, folder: std::path::PathBuf, key: u8) -> (Rig, PicturesImported) {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let counts = import(&rig, &folder, key).unwrap();
        let _ = done.send((rig, counts));
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the import waited")
}

fn fifo(path: &Path) {
    let made = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(made.success());
}

#[test]
fn a_pipe_or_a_file_too_large_is_unreadable_and_never_waited_on() {
    let (rig, folder) = rig();
    // A portrait that is a named pipe, which a plain read would wait on.
    let dir = folder.path().join(MOSS_ARCADE);
    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(8, 8, [1, 2, 3]),
    );
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    fifo(&dir.join("portrait.jpg"));
    // A manifest larger than any manifest is.
    let named = folder.path().join("by-name/Various Artists");
    std::fs::create_dir_all(&named).unwrap();
    let mut huge = br#"{"format": 1, "chosen": {}, "padding": ""#.to_vec();
    huge.resize(super::MANIFEST_CAP as usize + 10, b' ');
    huge.extend_from_slice(br#""}"#);
    std::fs::write(named.join("manifest.json"), huge).unwrap();
    let (rig, counts) = import_within(rig, folder.path().to_path_buf(), 0xa1);
    assert_eq!(
        (counts.unreadable, counts.by_mbid, counts.by_name),
        (2, 0, 0)
    );

    // A portrait served is read the same way: one replaced by a pipe since
    // the import is not there.
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(8, 8, [1, 2, 3]),
    );
    let (rig, counts) = import_within(rig, folder.path().to_path_buf(), 0xa2);
    assert_eq!(counts.by_mbid, 1);
    let art = named_art(&rig, "Moss Arcade");
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    fifo(&dir.join("portrait.jpg"));
    let cache = Scratch::new();
    let covers = Covers::new(cache.path()).unwrap();
    let (served, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = served.send(matches!(
            covers.render(&rig.library, &rig.server, art, 64),
            Err(crate::covers::CoverError::NotFound)
        ));
    });
    assert!(
        answered
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("serving waited")
    );
}

fn named_art(rig: &Rig, name: &str) -> Id {
    let rows = artists(rig);
    let artist = named(&rows, name);
    assert!(artist.portrait, "{name}");
    artist.art_id.unwrap()
}

#[test]
fn a_link_out_of_the_folder_is_not_followed_on_import_or_when_served() {
    let (rig, folder) = rig();
    let outside = Scratch::new();
    let picture = jpeg(8, 8, [200, 10, 10]);
    std::fs::write(outside.path().join("elsewhere.jpg"), &picture).unwrap();
    // A portrait that links to a file outside the folder.
    portrait(folder.path(), MOSS_ARCADE, "portrait.jpg", &picture);
    let dir = folder.path().join(MOSS_ARCADE);
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere.jpg"),
        dir.join("portrait.jpg"),
    )
    .unwrap();
    // An artist's folder that is a link to a folder outside.
    let away = outside.path().join("away");
    portrait(outside.path(), "away", "me.jpg", &picture);
    std::fs::create_dir_all(folder.path().join("by-name")).unwrap();
    std::os::unix::fs::symlink(&away, folder.path().join("by-name/Various Artists")).unwrap();
    let counts = import(&rig, folder.path(), 0xa1).unwrap();
    assert_eq!(
        (counts.unreadable, counts.by_mbid, counts.by_name),
        (2, 0, 0)
    );

    // Imported as a file, then made a link with the same bytes: not served.
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    std::fs::write(dir.join("portrait.jpg"), &picture).unwrap();
    assert_eq!(import(&rig, folder.path(), 0xa2).unwrap().by_mbid, 1);
    let art = named_art(&rig, "Moss Arcade");
    std::fs::remove_file(dir.join("portrait.jpg")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("elsewhere.jpg"),
        dir.join("portrait.jpg"),
    )
    .unwrap();
    let cache = Scratch::new();
    let covers = Covers::new(cache.path()).unwrap();
    assert!(matches!(
        covers.render(&rig.library, &rig.server, art, 64),
        Err(crate::covers::CoverError::NotFound)
    ));
}

/// Gives the library a second artist with no ID, `Collision:A`, on the
/// compilation beside Various Artists, renamed `Collision/A`: two names
/// the folder makes the same.
fn collide(rig: &Rig) {
    rig.library
        .write(
            WriteRequest {
                method: "test.write",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute_batch(
                    "UPDATE artist SET display_name = 'Collision/A' WHERE display_name = 'Various Artists';
                     INSERT INTO artist (id, public_id, canonical_id, display_name, rev)
                     VALUES (900, randomblob(16), 900, 'Collision:A', 1);
                     INSERT INTO artist_credit_name (artist_credit_id, position, artist_id, credited_name, rev)
                     SELECT display_artist_credit_id, 1, 900, 'Collision:A', 1
                     FROM release WHERE display_title = 'Night Drives';",
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
}

/// A manifest for `by-name/<dir>` that names its artist.
fn named_portrait(folder: &Path, dir: &str, name: Option<&str>) {
    portrait(folder, dir, "me.jpg", &jpeg(8, 8, [9, 9, 9]));
    let manifest = serde_json::json!({
        "format": 1,
        "artist": { "name": name },
        "chosen": { "portrait": { "file": "me.jpg" } }
    });
    std::fs::write(folder.join(dir).join("manifest.json"), manifest.to_string()).unwrap();
}

#[test]
fn two_names_made_the_same_take_a_portrait_only_where_its_manifest_names_one() {
    let (rig, folder) = rig();
    collide(&rig);
    // The manifest names neither: neither takes it, and both are counted.
    named_portrait(folder.path(), "by-name/Collision_A", Some("Somebody Else"));
    let counts = import(&rig, folder.path(), 0xa1).unwrap();
    assert_eq!((counts.by_name, counts.ambiguous), (0, 2));
    assert!(artists(&rig).iter().filter(|row| row.portrait).count() == 0);

    // It names one of them exactly: that one takes it.
    named_portrait(folder.path(), "by-name/Collision_A", Some("Collision:A"));
    let counts = import(&rig, folder.path(), 0xa2).unwrap();
    assert_eq!((counts.by_name, counts.ambiguous), (1, 1));
    let rows = artists(&rig);
    assert!(named(&rows, "Collision:A").portrait);
    assert!(!named(&rows, "Collision/A").portrait);
}

#[test]
fn a_folder_checked_is_imported_even_if_its_index_goes_meanwhile() {
    let (rig, folder) = rig();
    portrait(
        folder.path(),
        MOSS_ARCADE,
        "portrait.jpg",
        &jpeg(8, 8, [1, 2, 3]),
    );
    // The server checks the folder, then records its root; the index can
    // go between. The import must not refuse a folder already accepted.
    let checked = super::check_picture_folder(folder.path()).unwrap();
    std::fs::remove_file(folder.path().join("index.json")).unwrap();
    let counts = import_checked(&rig, &checked, 0xa1).unwrap();
    assert_eq!(counts.by_mbid, 1);
}
