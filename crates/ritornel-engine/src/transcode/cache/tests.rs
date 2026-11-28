use super::*;
use crate::testing::{CountingEntropy, FixedClock};
use crate::transcode::{Artwork, CopySource, GainTag, TaggedCopy, Tags};
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
        owner: PinOwner::Batch,
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
        gain: GainTags {
            track: GainTag {
                gain_db: Some(-6.25),
                peak: Some(0.988),
            },
            album: GainTag {
                gain_db: Some(-5.5),
                peak: None,
            },
        },
        ritornel_id: Some("1:library:item:appearance".into()),
    };
    let cancel = AtomicBool::new(false);
    for codec in [Codec::Flac, Codec::Mp3, Codec::Opus] {
        let master = fixture(codec);
        let audio = audio_hash(&master);
        let hash = blake3::hash(&fs::read(&master).unwrap());
        let source = CopySource::Master {
            asset: AudioAsset {
                scheme: 1,
                hash: blake3::Hash::from_hex(&audio).unwrap(),
            },
            path: &master,
            facts: crate::playback::FileFacts::of(&master.metadata().unwrap()),
            codec,
        };
        let copy = TaggedCopy {
            tags: &tags,
            artwork: Some(Artwork {
                bytes: &cover,
                max_side: 10,
            }),
            id3v1: codec == Codec::Mp3,
        };
        let made = dir.path().join(format!("made.{}", codec.extension()));
        copy.write(source, &made, &cancel).unwrap();
        let parsed = lofty::read_from_path(&made).unwrap();
        let tag = parsed.primary_tag().unwrap();
        assert_eq!(tag.title().as_deref(), Some("Cloud Orchard 雪"));
        assert_eq!(tag.artist().as_deref(), Some("Invented Trio"));
        assert_eq!(tag.album().as_deref(), Some("Winter Branches"));
        assert_eq!(tag.track(), Some(9));
        assert_eq!(tag.disk(), Some(2));
        assert_eq!(tag.genre().as_deref(), Some("Folk"));
        let text = |key| tag.get_string(key);
        use lofty::tag::ItemKey;
        assert_eq!(text(ItemKey::AlbumArtist), Some("Winter Trio"));
        assert_eq!(text(ItemKey::RecordingDate), Some("2024"));
        // ReplayGain as tags the player applies: Vorbis comments, or in an
        // MP3 `TXXX` frames, which read back under the same keys. A peak
        // the master had no figure for is not invented.
        assert_eq!(text(ItemKey::ReplayGainTrackGain), Some("-6.25 dB"));
        assert_eq!(text(ItemKey::ReplayGainTrackPeak), Some("0.988000"));
        assert_eq!(text(ItemKey::ReplayGainAlbumGain), Some("-5.50 dB"));
        assert_eq!(text(ItemKey::ReplayGainAlbumPeak), None);
        // Ritornel's own identifier, under its own name in every format.
        let bytes = fs::read(&made).unwrap();
        let holds = |needle: &[u8]| bytes.windows(needle.len()).any(|at| at == needle);
        assert!(holds(crate::transcode::RITORNEL_ID.as_bytes()), "{codec:?}");
        assert!(holds(b"1:library:item:appearance"), "{codec:?}");
        assert_eq!(tag.pictures().len(), 1);
        let data = tag.pictures()[0].data();
        // A baseline JPEG (SOF0), never a progressive one (SOF2).
        assert!(data.starts_with(&[0xff, 0xd8]) && data.windows(2).any(|m| m == [0xff, 0xc0]));
        assert!(!data.windows(2).any(|m| m == [0xff, 0xc2]));
        assert_eq!(image::load_from_memory(data).unwrap().dimensions(), (10, 5));
        if codec == Codec::Mp3 {
            assert_eq!(
                parsed.tag(TagType::Id3v1).unwrap().title().as_deref(),
                Some("Cloud Orchard ?")
            );
        }
        assert_eq!(audio_hash(&made), audio);
        assert_eq!(hash, blake3::hash(&fs::read(&master).unwrap()));
        // A tagged copy is the same bytes every time it is made.
        let again = dir.path().join(format!("again.{}", codec.extension()));
        copy.write(source, &again, &cancel).unwrap();
        assert_eq!(fs::read(&again).unwrap(), bytes, "{codec:?} rebuild");
        // The writer never overwrites: a copy is made at a new name.
        assert!(copy.write(source, &again, &cancel).is_err());
        let bad_art = TaggedCopy {
            tags: &tags,
            artwork: Some(Artwork {
                bytes: b"invalid",
                max_side: 10,
            }),
            id3v1: false,
        };
        let bad = dir.path().join(format!("bad.{}", codec.extension()));
        assert!(bad_art.write(source, &bad, &cancel).is_err());
        let removed = TaggedCopy {
            tags: &Tags::default(),
            artwork: None,
            id3v1: false,
        };
        let bare = dir.path().join(format!("bare.{}", codec.extension()));
        removed.write(source, &bare, &cancel).unwrap();
        let parsed = lofty::read_from_path(&bare).unwrap();
        assert!(
            parsed
                .tags()
                .iter()
                .all(|t| t.title().is_none() && t.pictures().is_empty())
        );
        // An ID3v1 trailer is an MP3's alone.
        let trailer = TaggedCopy {
            tags: &tags,
            artwork: None,
            id3v1: true,
        };
        let trailed = dir.path().join(format!("trailed.{}", codec.extension()));
        assert_eq!(
            trailer.write(source, &trailed, &cancel).is_ok(),
            codec == Codec::Mp3
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
fn a_hit_only_touches_the_index_and_a_size_outlives_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let db = store();
    let mut cache = TranscodeCache::open(&db, dir.path(), pin(1).id, u64::MAX).unwrap();
    let id = encoder();
    let original = cache
        .derivative(&db, spec(&id, 1), pin(2), |out, _| {
            fs::copy(fixture(Codec::Mp3), out).map(|_| ())
        })
        .unwrap();
    let mtime = original.path().metadata().unwrap().modified().unwrap();
    for n in [3, 4] {
        let hit = cache
            .derivative(&db, spec(&id, 1), pin(n), |_, _| {
                panic!("cache hit encoded again")
            })
            .unwrap();
        assert_eq!(hit.file_hash, original.file_hash);
    }
    assert_eq!(
        mtime,
        original.path().metadata().unwrap().modified().unwrap()
    );
    assert!(cache.holds(&db, original.key()).unwrap());
    assert_eq!(cache.usage(&db).unwrap(), (original.size_bytes, 1));
    // Let go and evicted, its exact size is still known, for an estimate.
    cache.budget = 0;
    for n in [2, 3, 4] {
        cache.release(&db, pin(n).id).unwrap();
    }
    assert!(!original.path().exists());
    assert!(!cache.holds(&db, original.key()).unwrap());
    assert_eq!(
        cache.size_hint(&db, original.key()).unwrap(),
        Some(original.size_bytes)
    );
}

#[test]
fn the_budget_caps_kept_encodes_and_respects_every_pin() {
    let temp = tempfile::tempdir().unwrap();
    let db = store();
    let mut cache = TranscodeCache::open(&db, temp.path(), pin(1).id, u64::MAX).unwrap();
    let id = encoder();
    let base = cache
        .derivative(&db, spec(&id, 1), pin(2), |p, _| {
            fs::copy(fixture(Codec::Mp3), p).map(|_| ())
        })
        .unwrap();
    // A tagged copy is a scratch file of its transfer's own: it is no row
    // of the index and no part of the budget.
    let scratch = cache.scratch().unwrap();
    fs::write(&scratch, vec![0; 4096]).unwrap();
    assert_eq!(cache.usage(&db).unwrap(), (base.size_bytes, 1));
    cache.budget = 0;
    for n in [5, 6] {
        cache
            .derivative(&db, spec(&id, 1), pin(n), |_, _| panic!())
            .unwrap();
    }
    // Over its budget only by the encodes in use at that moment.
    cache.release(&db, pin(2).id).unwrap();
    cache.release(&db, pin(5).id).unwrap();
    assert!(base.path().exists());
    cache.release(&db, pin(6).id).unwrap();
    assert!(!base.path().exists());
    assert!(scratch.exists());
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
    // A tagged copy whose transfer a dead run held, and what an older
    // cache kept as its packaged layer.
    let scratch = cache.scratch().unwrap().keep().unwrap();
    let packaged = temp.path().join("packaged/ab");
    fs::create_dir_all(&packaged).unwrap();
    fs::write(packaged.join("old.flac"), b"tagged").unwrap();
    TranscodeCache::open(&db, temp.path(), pin(3).id, 0).unwrap();
    assert!([entry.path(), &stale, &orphan].iter().all(|p| !p.exists()));
    assert!(!scratch.exists() && !temp.path().join("packaged").exists());
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
