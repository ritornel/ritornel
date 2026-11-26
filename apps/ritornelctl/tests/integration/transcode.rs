//! Verify transcode commands against a running library hub.

use super::support::{Scratch, Server, json, ok, ritornelctl};
use std::path::Path;

#[test]
fn transcode_inspects_the_hub_packages_lossless_tracks_and_prunes() {
    use ritornel_client::contract::{
        CONTRACT_VERSION, ClientKind, HelloParams, ListKind, ListOpenParams, Row,
    };

    let scratch = Scratch::new("ritornelctl-transcode-");
    let dir = scratch.data_dir();
    let music = scratch.0.join("masters");
    std::fs::create_dir_all(&music).unwrap();
    for name in ["a.flac", "b.no-artist.flac"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio")
                .join(name),
            music.join(name),
        )
        .unwrap();
    }
    let server = Server::start(
        &dir,
        &[
            ("RITORNEL_TRANSCODE_WORKERS", "2"),
            ("RITORNEL_TRANSCODE_CACHE_SIZE", "8 MiB"),
        ],
    );
    ok(&dir, &["library", "create"]);
    ok(&dir, &["scan", music.to_str().unwrap()]);
    let ids = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let c = ritornel_client::local::find(&dir)
                .await
                .unwrap()
                .connect()
                .await
                .unwrap();
            c.hello(&HelloParams {
                name: "test".into(),
                version: "1".into(),
                kind: ClientKind::Cli,
                contract_version: CONTRACT_VERSION,
                presence: None,
            })
            .await
            .unwrap();
            let songs = c
                .list_open(&ListOpenParams {
                    kind: ListKind::Songs,
                    query: Default::default(),
                    window: Some(ritornel_client::contract::Window {
                        offset: 0,
                        limit: 10,
                    }),
                    replaces: None,
                })
                .await
                .unwrap();
            songs
                .rows
                .into_iter()
                .filter_map(|row| match row {
                    Row::Song(s) => Some(s.appearance_id.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
    assert_eq!(ids.len(), 2);
    let stats = json(&dir, &["transcode", "stats"]);
    let encoders = json(&dir, &["transcode", "encoders"]);
    assert_eq!(encoders, stats["encoders"]);
    for encoder in encoders.as_array().unwrap() {
        assert!(encoder["version"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(!encoder["formats"].as_array().unwrap().is_empty());
    }
    assert_eq!(stats["budgetBytes"]["value"], 8 << 20);
    assert_eq!(stats["workers"]["setBy"], "environment");
    assert_eq!(
        stats["profiles"]["value"]["passthrough-flac"]["target"]["codec"],
        "flac"
    );
    assert_eq!(stats, json(&dir, &["settings"])["transcode"]);
    assert_eq!(stats["usedBytes"], 0);
    assert_eq!(
        stats["profiles"]["value"]["opus-160"]["target"]["bitrateKbps"],
        160
    );
    assert_eq!(
        stats["profiles"]["value"]["opus-160"]["dsp"]["replayGain"],
        "off"
    );
    let (code, _, error) = ritornelctl(
        &dir,
        &[
            "transcode",
            "run",
            "--profile",
            "passthrough-flac",
            "--derivative",
            &ids[0],
        ],
    );
    assert_eq!(code, Some(1));
    assert!(
        error.contains("FLAC pass-through needs the packaged layer"),
        "{error}"
    );
    let single = json(
        &dir,
        &["transcode", "run", "--profile", "passthrough-flac", &ids[0]],
    );
    assert_eq!(single["job"]["state"], "succeeded");
    let batch = json(
        &dir,
        &[
            "transcode",
            "run",
            "--profile",
            "passthrough-flac",
            &ids[0],
            &ids[1],
        ],
    );
    let artifacts = batch["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0], single["artifacts"][0]);
    for (a, id) in artifacts.iter().zip(ids.iter()) {
        assert_eq!(&a["appearanceId"], id.as_str());
        assert_eq!(a["layer"], "packaged");
        assert!(a["sizeBytes"].as_u64().unwrap() > 0);
        assert_eq!(a["key"].as_str().unwrap().len(), 64);
        assert_eq!(a["fileHash"], a["sampledHash"]);
    }
    assert_eq!(json(&dir, &["transcode", "stats"])["packages"], 2);
    for encoder in encoders.as_array().unwrap() {
        let profile = match encoder["family"].as_str().unwrap() {
            "lame" => "mp3-v2",
            "opusenc" => "opus-160",
            family => panic!("unknown encoder {family}"),
        };
        let result = json(
            &dir,
            &[
                "transcode",
                "run",
                "--profile",
                profile,
                "--derivative",
                &ids[0],
            ],
        );
        assert_eq!(result["artifacts"][0]["layer"], "derivative");
        assert!(result["artifacts"][0]["sizeBytes"].as_u64().unwrap() > 0);
    }
    let (code, _, stderr) =
        ritornelctl(&dir, &["transcode", "run", "--profile", "missing", &ids[0]]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("unknown transcode profile"), "{stderr}");
    assert_eq!(
        json(&dir, &["transcode", "prune"])["job"]["state"],
        "succeeded"
    );
    assert_eq!(json(&dir, &["transcode", "stats"])["usedBytes"], 0);
    server.stop();
}
