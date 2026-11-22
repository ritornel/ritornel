//! Covers from an invented library: rendered to a size and kept, missing
//! when their file has gone, and new when the image changes. And covers of
//! a scanned root: a picture embedded in a file and a folder image, each
//! read only while it is still the picture the scan recorded.

use ritornel_contract::{Id, ListKind, Query, Row};

use super::{CoverError, Covers};
use crate::WriteRequest;
use crate::invent::testing::{Library, library};
use crate::invent::{Churn, Churned};
use crate::scan_tests::Rig;

fn covers(library: &Library) -> Covers {
    Covers::new(&library.scratch.path().join("cache")).unwrap()
}

/// The art of the first tiles that have some.
fn arts(library: &Library) -> Vec<Id> {
    let albums = library
        .store
        .list_open(ListKind::Albums, &Query::default())
        .unwrap();
    library
        .store
        .list_rows(ListKind::Albums, &albums.ids)
        .unwrap()
        .into_iter()
        .filter_map(|row| match row {
            Row::Album(album) => album.art_id,
            _ => None,
        })
        .collect()
}

fn write(library: &Library, sql: &'static str) {
    library
        .store
        .write(
            WriteRequest {
                method: "test.write",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute_batch(sql)?;
                Ok(String::new())
            },
        )
        .unwrap();
}

#[test]
fn a_cover_is_scaled_to_fit_its_size_and_kept_for_next_time() {
    let library = library(10);
    let covers = covers(&library);
    let art = arts(&library)[0];
    assert!(covers.cached(art, 64).is_none());
    let rendered = covers
        .render(&library.store, &library.servers, art, 64)
        .unwrap();
    let decoded = image::load_from_memory(&rendered.bytes).unwrap();
    assert_eq!(decoded.width().max(decoded.height()), 64);
    assert_eq!(covers.cached(art, 64), Some(rendered.bytes));
    // Nothing is left half-written.
    let left: Vec<_> = std::fs::read_dir(library.scratch.path().join("cache/tmp"))
        .unwrap()
        .collect();
    assert!(left.is_empty());
    // A picture smaller than the size is never scaled up. The invented
    // covers here are 100 to 140 pixels.
    let big = covers
        .render(&library.store, &library.servers, art, 1024)
        .unwrap();
    let decoded = image::load_from_memory(&big.bytes).unwrap();
    assert!((100..=140).contains(&decoded.width()));
}

#[test]
fn a_size_that_is_not_a_bucket_and_an_unknown_picture_are_refused() {
    let library = library(5);
    let covers = covers(&library);
    let art = arts(&library)[0];
    assert!(matches!(
        covers.render(&library.store, &library.servers, art, 100),
        Err(CoverError::BadSize)
    ));
    assert!(matches!(
        covers.render(
            &library.store,
            &library.servers,
            Id::from_bytes([7; 16]),
            64
        ),
        Err(CoverError::NotFound)
    ));
}

#[test]
fn a_cover_whose_file_has_gone_is_not_found() {
    let library = library(5);
    let covers = covers(&library);
    for art in arts(&library) {
        // Every folder image of the record is removed.
        let sources = library.store.picture_sources(art).unwrap();
        let roots = library.servers.roots().unwrap();
        let (_, root) = roots
            .iter()
            .find(|(name, _)| *name == sources[0].root)
            .unwrap();
        let file = std::path::Path::new(root).join(&sources[0].path);
        std::fs::remove_file(file.parent().unwrap().join("cover.jpg")).unwrap();
        assert!(matches!(
            covers.render(&library.store, &library.servers, art, 64),
            Err(CoverError::NotFound)
        ));
    }
    // A file that is known to be missing is not even looked for.
    write(&library, "UPDATE source_item SET missing_since_ms = 1");
    let art = arts(&library).into_iter().next();
    assert!(art.is_none_or(|art| library.store.picture_sources(art).unwrap().is_empty()));
}

#[test]
fn a_changed_image_is_a_new_art_id_and_the_old_one_is_kept() {
    let library = library(20);
    let covers = covers(&library);
    let before = arts(&library);
    let first = covers
        .render(&library.store, &library.servers, before[0], 64)
        .unwrap();
    let mut churn = Churn::start(
        &library.store,
        &library.servers,
        5,
        crate::invent::testing::NOW,
    )
    .unwrap();
    // Steps until one replaces a cover.
    loop {
        if churn.step(&library.store).unwrap() == Churned::Recovered {
            break;
        }
    }
    let after = arts(&library);
    let new: Vec<_> = after.iter().filter(|art| !before.contains(art)).collect();
    assert!(!new.is_empty());
    for art in new {
        covers
            .render(&library.store, &library.servers, *art, 64)
            .unwrap();
    }
    // The cover served before under its ID is the same bytes still.
    assert_eq!(covers.cached(before[0], 64), Some(first.bytes));
}

#[test]
fn a_scanned_picture_is_read_from_its_file_while_it_is_the_same_picture() {
    let mut rig = Rig::new();
    // `a.flac` has an 8 by 8 PNG embedded. `b.flac` has none, and a folder
    // image beside it.
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Other/01.flac", "b.flac");
    let roots = rig.server.roots().unwrap();
    let root = std::path::PathBuf::from(&roots[0].1);
    let folder_image =
        |width, height| super::scale(&image::DynamicImage::new_rgb8(width, height), 1024).unwrap();
    std::fs::write(root.join("Other/cover.jpg"), folder_image(40, 30)).unwrap();
    rig.scan();
    let pictures = rig.rows(
        "SELECT p.public_id, rp.source_kind FROM picture p
         JOIN revision_picture rp ON rp.picture_id = p.id ORDER BY rp.source_kind",
    );
    // The rows come back as hex.
    let art = |row: &str| -> Id {
        let hex = row.split('|').next().unwrap();
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        Id::from_bytes(bytes)
    };
    let [embedded, folder] = &pictures[..] else {
        panic!("{pictures:?}");
    };
    assert!(embedded.ends_with("|embedded") && folder.ends_with("|folder_image"));
    let cache = Covers::new(&root.join("../cache")).unwrap();
    let size = |art: Id| {
        let rendered = cache.render(&rig.library, &rig.server, art, 64).unwrap();
        let decoded = image::load_from_memory(&rendered.bytes).unwrap();
        (decoded.width(), decoded.height())
    };
    assert_eq!(size(art(embedded)), (8, 8));
    assert_eq!(size(art(folder)), (40, 30));
    // A folder image replaced since the scan is another picture: the one
    // recorded is not there any more.
    std::fs::write(root.join("Other/cover.jpg"), folder_image(60, 20)).unwrap();
    std::fs::remove_file(cache.path(art(folder), 64)).unwrap();
    assert!(matches!(
        cache.render(&rig.library, &rig.server, art(folder), 64),
        Err(CoverError::NotFound)
    ));
}

#[test]
fn repeated_folder_art_leaves_room_for_a_readable_copy() {
    let mut rig = Rig::new();
    for n in 0..9 {
        rig.put(&format!("First/{n}.flac"), "b.flac");
    }
    rig.put("Second/1.flac", "b.flac");
    let root = std::path::PathBuf::from(&rig.server.roots().unwrap()[0].1);
    let bytes = super::scale(&image::DynamicImage::new_rgb8(32, 24), 64).unwrap();
    for name in ["First", "Second"] {
        std::fs::write(root.join(name).join("cover.jpg"), &bytes).unwrap();
    }
    rig.scan();
    let art = rig
        .library
        .read(|db| {
            let id = db.query_row("SELECT public_id FROM picture", [], |row| row.get(0))?;
            Ok(Id::from_bytes(id))
        })
        .unwrap();
    std::fs::remove_file(root.join("First/cover.jpg")).unwrap();
    assert_eq!(rig.library.picture_sources(art).unwrap().len(), 2);
    Covers::new(&root.join("../cache"))
        .unwrap()
        .render(&rig.library, &rig.server, art, 64)
        .unwrap();
}

#[test]
fn a_colour_is_the_hue_a_picture_has_most_of_and_grey_has_none() {
    use super::color_of_pixels;
    // All one red.
    let red = color_of_pixels(std::iter::repeat_n([200, 30, 30], 576)).unwrap();
    assert_eq!(red.hue, 0);
    assert!(red.saturation > 60, "{red:?}");
    // Mostly a blue, some grey and a little orange: the blue wins, and the
    // grey counts for nothing.
    let pixels = std::iter::repeat_n([40, 70, 180], 300)
        .chain(std::iter::repeat_n([128, 128, 128], 200))
        .chain(std::iter::repeat_n([230, 140, 40], 76));
    let blue = color_of_pixels(pixels).unwrap();
    assert!((220..=232).contains(&blue.hue), "{blue:?}");
    // Grey, near black and near white have no colour.
    assert_eq!(
        color_of_pixels(std::iter::repeat_n([90, 90, 92], 576)),
        None
    );
    assert_eq!(color_of_pixels(std::iter::repeat_n([3, 2, 4], 576)), None);
    assert_eq!(
        color_of_pixels(std::iter::repeat_n([252, 253, 251], 576)),
        None
    );
    assert_eq!(color_of_pixels(std::iter::empty()), None);
}

#[test]
fn a_colour_is_kept_with_the_covers_whether_a_cover_was_made_first_or_not() {
    let library = library(10);
    let covers = covers(&library);
    let arts = arts(&library);
    // Asked for before any cover of it was made: from its source.
    let first = arts[0];
    assert_eq!(covers.cached_color(first), None);
    let color = covers
        .color(&library.store, &library.servers, first)
        .unwrap();
    assert!(color.is_some(), "a painted cover has a colour");
    assert_eq!(covers.cached_color(first), Some(color));
    // Made with a cover, from the picture decoded for it.
    let second = arts[1];
    covers
        .render(&library.store, &library.servers, second, 64)
        .unwrap();
    let kept = covers
        .cached_color(second)
        .expect("kept when the cover was made");
    assert_eq!(
        covers
            .color(&library.store, &library.servers, second)
            .unwrap(),
        kept
    );
    // A picture that is not there has none to give.
    assert!(matches!(
        covers.color(&library.store, &library.servers, Id::from_bytes([3; 16])),
        Err(CoverError::NotFound)
    ));
}
