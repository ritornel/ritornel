use super::*;
use crate::testing::{CountingEntropy, FixedClock};
use crate::transcode::{Artwork, Tags};
use image::GenericImageView;
use lofty::file::TaggedFileExt;
use lofty::prelude::Accessor;
use lofty::tag::TagType;
use std::sync::Arc;

fn fixture(codec: Codec) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(format!("a.{}", codec.extension()))
}

fn audio_hash(path: &Path) -> String {
    let payload = crate::media::read_file(
        path.parent().unwrap(),
        path.file_name().unwrap().to_str().unwrap(),
        path.metadata().unwrap().len(),
        0,
        vec![],
    );
    assert!(payload.error.is_none(), "{:?}", payload.error);
    payload.audio.unwrap().hash
}

fn picture(color: u8) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(20, 10, image::Rgb([color, 10, 90]));
    let mut image = io::Cursor::new(Vec::new());
    img.write_to(&mut image, image::ImageFormat::Png).unwrap();
    image.into_inner()
}

fn pin(n: u8) -> Pin {
    Pin {
        id: Id::from_bytes([n; 16]),
        owner: PinOwner::SyncBatch,
    }
}

fn store() -> ServerStore {
    ServerStore::open_in_memory(
        Arc::new(FixedClock::at(100)),
        Arc::new(CountingEntropy::default()),
    )
    .unwrap()
}

#[test]
fn all_three_formats_round_trip_projected_tags_and_baseline_front_cover() {
    let dir = tempfile::tempdir().unwrap();
    let db = store();
    let cache = TranscodeCache::open(&db, dir.path(), pin(1).id, 0).unwrap();
    let cover = picture(221);
    let tags = Tags {
        title: Some("Cloud Orchard 雪".into()),
        artist: Some("Invented Trio".into()),
        album: Some("Winter Branches".into()),
        track: Some(9),
        album_artist: Some("Winter Trio".into()),
        disc: Some(2),
        year: Some(2024),
        genre: Some("Folk".into()),
    };
    for codec in [Codec::Flac, Codec::Mp3, Codec::Opus] {
        let master = fixture(codec);
        let audio = audio_hash(&master);
        let hash = blake3::hash(&fs::read(&master).unwrap());
        let source = PackageSource::Master {
            asset: AudioAsset {
                scheme: 1,
                hash: blake3::Hash::from_hex(&audio).unwrap(),
            },
            path: &master,
            facts: crate::playback::FileFacts::of(&master.metadata().unwrap()),
            codec,
        };
        let package = Package {
            tags: &tags,
            artwork: Some(Artwork {
                bytes: &cover,
                max_side: 10,
            }),
            id3v1: codec == Codec::Mp3,
        };
        let entry = cache.package(&db, source, &package, pin(2)).unwrap();
        let parsed = lofty::read_from_path(entry.path()).unwrap();
        let tag = parsed.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Cloud Orchard 雪"));
        assert_eq!(tag.artist().as_deref(), Some("Invented Trio"));
        assert_eq!(tag.album().as_deref(), Some("Winter Branches"));
        assert_eq!(tag.track(), Some(9));
        assert_eq!(tag.disk(), Some(2));
        assert_eq!(tag.genre().as_deref(), Some("Folk"));
        assert_eq!(
            tag.get_string(lofty::tag::ItemKey::AlbumArtist),
            Some("Winter Trio")
        );
        assert_eq!(
            tag.get_string(lofty::tag::ItemKey::RecordingDate),
            Some("2024")
        );
        assert_eq!(tag.pictures().len(), 1);
        let data = tag.pictures()[0].data();
        assert!(data.starts_with(&[0xff, 0xd8]) && data.windows(2).any(|m| m == [0xff, 0xc0]));
        assert_eq!(image::load_from_memory(data).unwrap().dimensions(), (10, 5));
        if codec == Codec::Mp3 {
            assert_eq!(
                parsed.tag(TagType::Id3v1).unwrap().title().as_deref(),
                Some("Cloud Orchard ?")
            );
        }
        assert_eq!(audio_hash(entry.path()), audio);
        assert_eq!(hash, blake3::hash(&fs::read(&master).unwrap()));
        let bytes = fs::read(entry.path()).unwrap();
        assert_eq!(entry.size_bytes, bytes.len() as u64);
        assert_eq!(entry.file_hash, blake3::hash(&bytes));
        assert_eq!(entry.sampled_hash, entry.file_hash);
        cache.release(&db, pin(2).id).unwrap();
        assert!(!entry.path().exists());
        let rebuilt = cache.package(&db, source, &package, pin(2)).unwrap();
        assert_eq!(
            fs::read(rebuilt.path()).unwrap(),
            bytes,
            "{codec:?} rebuild"
        );
        let bad_art = Package {
            tags: &tags,
            artwork: Some(Artwork {
                bytes: b"invalid",
                max_side: 10,
            }),
            id3v1: false,
        };
        assert!(cache.package(&db, source, &bad_art, pin(3)).is_err());
        let removed = Package {
            tags: &Tags::default(),
            artwork: None,
            id3v1: false,
        };
        let bare = cache.package(&db, source, &removed, pin(3)).unwrap();
        let parsed = lofty::read_from_path(bare.path()).unwrap();
        assert!(
            parsed
                .tags()
                .iter()
                .all(|t| t.title().is_none() && t.pictures().is_empty())
        );
    }
}

fn spec(id: &EncoderId, n: u8) -> Derivative<'_> {
    Derivative {
        asset: AudioAsset {
            scheme: 1,
            hash: blake3::hash(&[n]),
        },
        encoder: id,
        profile: crate::transcode::tests::mp3(),
        gain_tags: GainTags::default(),
    }
}

fn encoder() -> EncoderId {
    EncoderId {
        family: "test",
        version: "1".into(),
    }
}

#[test]
fn retag_and_cover_changes_reuse_audio_and_hits_only_touch_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let db = store();
    let cache = TranscodeCache::open(&db, dir.path(), pin(1).id, u64::MAX).unwrap();
    let id = encoder();
    let original = cache
        .derivative(&db, spec(&id, 1), pin(2), |out, _| {
            fs::copy(fixture(Codec::Mp3), out).map(|_| ())
        })
        .unwrap();
    let mtime = original.path().metadata().unwrap().modified().unwrap();
    let mut keys = HashSet::new();
    for (title, color, size) in [
        ("One", 9, 20),
        ("Two", 9, 20),
        ("Two", 10, 20),
        ("Two", 10, 10),
    ] {
        let derivative = cache
            .derivative(&db, spec(&id, 1), pin(3), |_, _| {
                panic!("cache hit encoded again")
            })
            .unwrap();
        let tags = Tags {
            title: Some(title.into()),
            ..Tags::default()
        };
        let art = picture(color);
        let package = Package {
            tags: &tags,
            artwork: Some(Artwork {
                bytes: &art,
                max_side: size,
            }),
            id3v1: false,
        };
        let entry = cache
            .package(
                &db,
                PackageSource::Derivative(&derivative),
                &package,
                pin(3),
            )
            .unwrap();
        assert!(keys.insert(entry.key()));
        assert_eq!(
            cache.size_hint(&db, entry.key()).unwrap(),
            Some(entry.size_bytes)
        );
    }
    assert_eq!(
        mtime,
        original.path().metadata().unwrap().modified().unwrap()
    );
}

#[test]
fn eviction_favors_derivatives_over_packages_and_respects_every_pin() {
    let temp = tempfile::tempdir().unwrap();
    let db = store();
    let mut cache = TranscodeCache::open(&db, temp.path(), pin(1).id, u64::MAX).unwrap();
    let id = encoder();
    let base = cache
        .derivative(&db, spec(&id, 1), pin(2), |p, _| {
            fs::copy(fixture(Codec::Mp3), p).map(|_| ())
        })
        .unwrap();
    let tags = Tags {
        title: Some("Alder".into()),
        ..Tags::default()
    };
    let package = Package {
        tags: &tags,
        artwork: None,
        id3v1: false,
    };
    let copy = cache
        .package(&db, PackageSource::Derivative(&base), &package, pin(3))
        .unwrap();
    cache
        .derivative(&db, spec(&id, 1), pin(4), |_, _| panic!())
        .unwrap();
    cache.release(&db, pin(2).id).unwrap();
    cache.release(&db, pin(3).id).unwrap();
    cache.budget = base.size_bytes;
    cache.release(&db, pin(4).id).unwrap();
    assert!(base.path().exists());
    assert!(!copy.path().exists());
    assert_eq!(
        cache.size_hint(&db, copy.key()).unwrap(),
        Some(copy.size_bytes)
    );
    cache.budget = 0;
    for n in [5, 6] {
        cache
            .derivative(&db, spec(&id, 1), pin(n), |_, _| panic!())
            .unwrap();
    }
    cache.release(&db, pin(5).id).unwrap();
    assert!(base.path().exists());
    cache.release(&db, pin(6).id).unwrap();
    assert!(!base.path().exists());
}

#[test]
fn failed_production_and_restart_clear_partials_orphans_missing_rows_and_old_pins() {
    let temp = tempfile::tempdir().unwrap();
    let db = store();
    let cache = TranscodeCache::open(&db, temp.path(), pin(1).id, 0).unwrap();
    let id = encoder();
    assert!(
        cache
            .derivative(&db, spec(&id, 1), pin(2), |p, _| {
                fs::write(p, b"incomplete")?;
                Err(io::Error::other("encoder failed"))
            })
            .is_err()
    );
    assert_eq!(temp.path().join("tmp").read_dir().unwrap().count(), 0);
    let entry = cache
        .derivative(&db, spec(&id, 1), pin(2), |p, _| fs::write(p, b"pinned"))
        .unwrap();
    let missing = cache
        .derivative(&db, spec(&id, 2), pin(2), |p, _| fs::write(p, b"missing"))
        .unwrap();
    fs::remove_file(missing.path()).unwrap();
    let orphan = temp
        .path()
        .join("transcodes/aa")
        .join(format!("{}.mp3", "aa".repeat(32)));
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    fs::write(&orphan, b"orphan").unwrap();
    let stale = temp.path().join("tmp/transcode-lost.part");
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("audio.part"), b"part").unwrap();
    let loose = temp.path().join("tmp/lost.part");
    fs::write(&loose, b"part").unwrap();
    TranscodeCache::open(&db, temp.path(), pin(3).id, 0).unwrap();
    assert!([entry.path(), &stale, &orphan].iter().all(|p| !p.exists()));
    assert!(
        loose.exists(),
        "the cover cache owns its loose staging files"
    );
    let count = db
        .read(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM transcode_entry", [], |r| {
                    r.get::<_, i64>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn sampled_hash_uses_each_end_once() {
    let size = 3 << 20;
    let mut bytes = vec![0u8; size];
    bytes[1 << 20] = 1;
    bytes[size - 1] = 2;
    let mut file = tempfile::tempfile().unwrap();
    use std::io::Write;
    file.write_all(&bytes).unwrap();
    file.rewind().unwrap();
    let (length, full, sampled) = hashes(&mut file, &AtomicBool::new(false)).unwrap();
    assert_eq!(length, size as u64);
    assert_eq!(full, blake3::hash(&bytes));
    let mut ends = blake3::Hasher::new();
    ends.update(&bytes[..1 << 20]);
    ends.update(&bytes[2 << 20..]);
    assert_eq!(sampled, ends.finalize());
}

#[test]
fn a_hit_protects_the_recently_used_derivative_at_eviction() {
    let temp = tempfile::tempdir().unwrap();
    let clock = Arc::new(FixedClock::at(10));
    let db =
        ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default())).unwrap();
    let cache = TranscodeCache::open(&db, temp.path(), pin(1).id, 2).unwrap();
    let id = encoder();
    let build = |n| {
        cache
            .derivative(&db, spec(&id, n), pin(2), |path, _| fs::write(path, [n]))
            .unwrap()
    };
    let first = build(1);
    cache.release(&db, pin(2).id).unwrap();
    clock.set(20);
    let second = build(2);
    cache.release(&db, pin(2).id).unwrap();
    clock.set(30);
    cache
        .derivative(&db, spec(&id, 1), pin(2), |_, _| panic!())
        .unwrap();
    cache.release(&db, pin(2).id).unwrap();
    clock.set(40);
    let third = build(3);
    assert!(first.path().exists() && third.path().exists());
    assert!(!second.path().exists());
}

#[test]
fn competing_producers_return_the_same_published_bytes_and_both_hold_pins() {
    let temp = tempfile::tempdir().unwrap();
    let db = store();
    let cache = TranscodeCache::open(&db, temp.path(), pin(1).id, 0).unwrap();
    let id = encoder();
    let ready = std::sync::Barrier::new(2);
    let produce = |n| {
        cache
            .derivative(&db, spec(&id, 1), pin(n), |path, _| {
                fs::write(path, [n])?;
                ready.wait();
                Ok(())
            })
            .unwrap()
    };
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| produce(2));
        let b = produce(3);
        (a.join().unwrap(), b)
    });
    assert_eq!(a.file_hash, b.file_hash);
    assert_eq!(a.file_hash, blake3::hash(&fs::read(a.path()).unwrap()));
    cache.release(&db, pin(2).id).unwrap();
    assert!(a.path().exists());
    cache.release(&db, pin(3).id).unwrap();
    assert!(!a.path().exists());
}

#[test]
fn eviction_can_skip_an_unremovable_file_and_meet_the_budget() {
    let temp = tempfile::tempdir().unwrap();
    let db = store();
    let cache = TranscodeCache::open(&db, temp.path(), pin(1).id, 2).unwrap();
    let id = encoder();
    let build = |n| {
        cache
            .derivative(&db, spec(&id, n), pin(n + 1), |out, _| fs::write(out, [n]))
            .unwrap()
    };
    let first = build(1);
    let second = build(2);
    cache.release(&db, pin(2).id).unwrap();
    cache.release(&db, pin(3).id).unwrap();
    fs::remove_file(first.path()).unwrap();
    fs::create_dir(first.path()).unwrap();
    let third = build(3);
    assert!(third.path().is_file());
    assert!(!second.path().exists());
}
