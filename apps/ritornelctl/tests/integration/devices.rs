//! A folder synced from a running hub, end to end: the plan, the first
//! sync, the second that does nothing, and what is never touched.

use super::support::{Scratch, Server, json, ok, ritornelctl};
use std::path::{Path, PathBuf};

fn files(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut walk = vec![folder.to_owned()];
    while let Some(at) = walk.pop() {
        for entry in std::fs::read_dir(at).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                walk.push(entry.path());
            } else {
                found.push(entry.path());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_folder_is_planned_synced_and_kept_in_step_and_what_is_not_ritornels_is_left() {
    let scratch = Scratch::new("ritornelctl-devices-");
    let dir = scratch.data_dir();
    let music = scratch.0.join("masters");
    let card = scratch.0.join("card");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::create_dir_all(&card).unwrap();
    for name in ["a.flac", "b.no-artist.flac", "a.mp3"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/audio")
                .join(name),
            music.join(name),
        )
        .unwrap();
    }
    let server = Server::start(&dir, &[]);
    ok(&dir, &["library", "create"]);
    ok(&dir, &["scan", music.to_str().unwrap()]);
    assert_eq!(
        json(&dir, &["devices", "list"])["devices"],
        serde_json::json!([])
    );

    // A sync writes and removes files: never among the library's own, or
    // the server's data.
    for inside in [
        music.clone(),
        music.join("sub"),
        dir.clone(),
        scratch.0.clone(),
    ] {
        std::fs::create_dir_all(&inside).unwrap();
        let (code, _, error) =
            ritornelctl(&dir, &["devices", "add-folder", inside.to_str().unwrap()]);
        assert_eq!(code, Some(1), "{}", inside.display());
        assert!(error.contains("Choose a separate folder"), "{error}");
    }
    let added = json(
        &dir,
        &[
            "devices",
            "add-folder",
            card.to_str().unwrap(),
            "--name",
            "Card",
            "--for",
            "player",
        ],
    );
    assert_eq!(added["device"]["setup"], "new");
    assert_eq!(added["device"]["icon"]["default"], "folder.player");
    assert_eq!(added["folder"]["madeFor"], "other_player");
    assert_eq!(
        added["capabilities"]["codecs"],
        serde_json::json!(["flac", "mp3"])
    );
    // Nothing is written until its first sync, and first contact is asked for.
    assert!(files(&card).is_empty());
    let plan = &added["plan"];
    let tracks = plan["counts"]["add"]["tracks"].as_u64().unwrap();
    assert!(tracks >= 2, "{plan}");
    assert_eq!(plan["fits"], true);
    let (code, _, error) = ritornelctl(&dir, &["devices", "sync", "card"]);
    assert_eq!(code, Some(1));
    assert!(error.contains("not set up"), "{error}");
    assert!(files(&card).is_empty());

    let synced = json(&dir, &["devices", "sync", "card", "--setup"]);
    assert_eq!(synced["run"]["outcome"], "complete", "{synced}");
    assert_eq!(synced["run"]["actionsDone"], tracks);
    assert_eq!(synced["device"]["setup"], "ready");
    let written = files(&card.join("Music"));
    assert_eq!(written.len() as u64, tracks);
    assert!(card.join(".ritornel/root.json").is_file());
    // Lossless as it is, and lossy as it is: nothing was encoded.
    assert!(
        written
            .iter()
            .any(|path| path.extension().unwrap() == "flac")
    );
    assert!(
        written
            .iter()
            .any(|path| path.extension().unwrap() == "mp3")
    );
    // The whole second run is nothing.
    let again = json(&dir, &["devices", "show", "card"]);
    assert_eq!(again["plan"]["figures"]["toCopyBytes"], 0);
    assert_eq!(again["plan"]["counts"]["add"]["tracks"], 0);
    assert_eq!(
        json(&dir, &["devices", "plan", "card"])["rows"],
        serde_json::json!([])
    );

    // A file Ritornel did not write is left and counted; one changed on the
    // device is a conflict and stays; one that went missing is sent again.
    std::fs::write(card.join("Music/mine.mp3"), b"not Ritornel's").unwrap();
    let flacs: Vec<&PathBuf> = written
        .iter()
        .filter(|path| path.extension().unwrap() == "flac")
        .collect();
    let (edited, lost) = (flacs[0], flacs[1]);
    let mut bytes = std::fs::read(edited).unwrap();
    bytes.extend_from_slice(b"edited on the device");
    std::fs::write(edited, &bytes).unwrap();
    std::fs::remove_file(lost).unwrap();
    let after = json(&dir, &["devices", "show", "card"]);
    assert_eq!(after["unmanaged"]["files"], 1);
    assert_eq!(after["plan"]["counts"]["conflict"]["tracks"], 1);
    assert_eq!(after["plan"]["counts"]["add"]["tracks"], 1);
    assert_eq!(after["device"]["attention"]["level"], "needs_you");
    let resynced = json(&dir, &["devices", "sync", "card"]);
    assert_eq!(resynced["run"]["outcome"], "complete", "{resynced}");
    assert!(lost.is_file());
    assert_eq!(std::fs::read(edited).unwrap(), bytes);
    assert_eq!(
        std::fs::read(card.join("Music/mine.mp3")).unwrap(),
        b"not Ritornel's"
    );

    // Left out, its files go, and nothing else does.
    let none = json(
        &dir,
        &[
            "devices", "set", "card", "--source", "added:0", "--plays", "flac,mp3",
        ],
    );
    let removing = none["plan"]["counts"]["remove"]["tracks"].as_u64().unwrap();
    assert_eq!(removing, tracks - 1, "{none}");
    let emptied = json(&dir, &["devices", "sync", "card"]);
    assert_eq!(emptied["run"]["outcome"], "complete", "{emptied}");
    let left = files(&card.join("Music"));
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(edited) && left.contains(&card.join("Music/mine.mp3")));

    // Forgotten, its files stay as they are and its marker goes.
    ok(&dir, &["devices", "forget", "card"]);
    assert_eq!(
        json(&dir, &["devices", "list"])["devices"],
        serde_json::json!([])
    );
    assert!(!card.join(".ritornel/root.json").exists());
    assert_eq!(files(&card.join("Music")).len(), 2);
    server.stop();
}
