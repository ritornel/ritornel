//! `ritornelctl corrections discs`: the disc layouts the library's
//! sidecars give, and the disc ID each would be asked about by. See "Disc
//! layouts" in docs/internals/corrections.md.
//!
//! `ritornelctl corrections musicbrainz` and `lookup`: the switch that
//! lets the library be asked about at MusicBrainz, and asking about one
//! album or folder, which is the only way it is asked. See "MusicBrainz lookups" there.

use std::io::Write;

use ritornel_client::contract::methods::{CorrectionsDiscs, CorrectionsLookup, LibrarySetSettings};
use ritornel_client::contract::{
    Disc, DiscList, DiscsParams, Id, LayoutMethod, LibrarySettingsSet, LookupDone, LookupOutcome,
    LookupParams, SetLibrarySettingsParams,
};
use ritornel_client::local;

use crate::cli::{DiscsArgs, Error, LookupArgs, SwitchArgs};
use crate::server;

pub(crate) fn discs(
    args: &DiscsArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let params = DiscsParams {
        folder: args.folder.clone(),
    };
    let list = server::ask(&args.server, env, async |client| {
        client.call::<CorrectionsDiscs>(&params).await
    })?;
    server::print(
        &args.server,
        out,
        &list,
        serde_json::to_string_pretty,
        render,
    )
}

pub(crate) fn musicbrainz(
    args: &SwitchArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let enabled = match args.state.as_str() {
        "on" => true,
        "off" => false,
        _ => return Err(Error::Usage("musicbrainz is `on` or `off`")),
    };
    let set = server::ask(&args.server, env, async |client| {
        client
            .call::<LibrarySetSettings>(&SetLibrarySettingsParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                lookups: Some(enabled),
            })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &set,
        serde_json::to_string_pretty,
        |set: &LibrarySettingsSet| {
            if set.settings.lookups.value {
                "MusicBrainz may be asked about this library, one album or folder at a time, \
                 with `ritornelctl corrections lookup`. Nothing is asked by itself.\n"
                    .to_owned()
            } else {
                "MusicBrainz is not asked about this library.\n".to_owned()
            }
        },
    )
}

pub(crate) fn lookup(
    args: &LookupArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let album = args
        .album
        .as_deref()
        .map(|text| {
            text.parse::<Id>()
                .map_err(|_| Error::Usage("--album needs a release ID"))
        })
        .transpose()?;
    if album.is_some() == args.folder.is_some() {
        return Err(Error::Usage("lookup needs a folder or --album, not both"));
    }
    let (folder, root, again) = (args.folder.clone(), args.root.clone(), args.again);
    let done = server::ask_for(&args.server, env, Some(LOOKUP_WAIT), async |client| {
        client
            .call::<CorrectionsLookup>(&LookupParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                album,
                folder,
                root,
                again,
            })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &done,
        serde_json::to_string_pretty,
        render_lookup,
    )
}

/// How long a lookup may take: a request a second, for each disc.
const LOOKUP_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

fn render_lookup(done: &LookupDone) -> String {
    let mut text = String::new();
    for disc in &done.discs {
        let said = match disc.outcome {
            LookupOutcome::Replied => "asked",
            LookupOutcome::NotFound => "asked: MusicBrainz does not know this disc",
            LookupOutcome::Kept => "asked before",
            LookupOutcome::Failed => "could not be asked",
        };
        text.push_str(&format!("{}  {said}\n", disc.disc_id));
        if let Some(failure) = &disc.failure {
            text.push_str(&format!("    {failure}\n"));
        }
        for album in &disc.albums {
            let year = album
                .first_year
                .map_or_else(String::new, |year| format!(" ({year})"));
            let releases = if album.releases == 1 {
                "1 release".to_owned()
            } else {
                format!("{} releases", album.releases)
            };
            text.push_str(&format!(
                "    {} by {}{year}, {releases}\n",
                album.title, album.artist
            ));
        }
    }
    text
}

fn method(method: LayoutMethod) -> &'static str {
    match method {
        LayoutMethod::Log => "rip log",
        LayoutMethod::Cue => "CUE sheet",
        LayoutMethod::RenamedCue => "CUE sheet of renamed files",
        LayoutMethod::AccurateRip => "lengths and AccurateRip ID",
        LayoutMethod::Cddb => "lengths and CDDB ID",
    }
}

fn disc(disc: &Disc) -> String {
    let place = if disc.folder.is_empty() {
        disc.root.clone()
    } else {
        format!("{}/{}", disc.root, disc.folder)
    };
    let methods: Vec<&str> = disc.methods.iter().copied().map(method).collect();
    let tracks = if disc.tracks == 1 {
        "1 track".to_owned()
    } else {
        format!("{} tracks", disc.tracks)
    };
    let mut notes = vec![tracks, format!("from {}", methods.join("; "))];
    if disc.whole_file {
        notes.push("one file".to_owned());
    } else if !disc.matched {
        notes.push("files not matched".to_owned());
    }
    if disc.provisional {
        notes.push("provisional: its CDDB ID disagrees".to_owned());
    }
    let mut text = format!("{}  {place}\n    {}\n", disc.disc_id, notes.join(", "));
    for file in &disc.files {
        let position = if file.position == 0 {
            "all".to_owned()
        } else {
            file.position.to_string()
        };
        text.push_str(&format!("    {position:>3}  {}\n", file.path));
    }
    text
}

fn render(list: &DiscList) -> String {
    let layouts = if list.discs.len() == 1 {
        "1 layout".to_owned()
    } else {
        format!("{} layouts", list.discs.len())
    };
    let mut text = format!(
        "{} of {} folders of audio have a disc layout. {layouts}.\n",
        list.with_layout, list.folders
    );
    for found in &list.discs {
        text.push('\n');
        text.push_str(&disc(found));
    }
    text
}

#[cfg(test)]
mod tests {
    use ritornel_client::contract::DiscFile;

    use super::*;

    #[test]
    fn a_disc_is_shown_with_how_it_was_found_and_its_files() {
        let list = DiscList {
            library_revision: 3,
            folders: 2,
            with_layout: 1,
            discs: vec![Disc {
                root: "Music".to_owned(),
                folder: "Someone/An Album".to_owned(),
                disc_id: "49HHV7Eb8UKF3aQiNmu1GR8vKTY-".to_owned(),
                tracks: 2,
                sectors: 95_312,
                methods: vec![LayoutMethod::Log, LayoutMethod::Cue],
                provisional: false,
                whole_file: false,
                matched: true,
                files: vec![
                    DiscFile {
                        position: 1,
                        path: "Someone/An Album/01.flac".to_owned(),
                    },
                    DiscFile {
                        position: 2,
                        path: "Someone/An Album/02.flac".to_owned(),
                    },
                ],
            }],
        };
        assert_eq!(
            render(&list),
            "1 of 2 folders of audio have a disc layout. 1 layout.\n\n\
             49HHV7Eb8UKF3aQiNmu1GR8vKTY-  Music/Someone/An Album\n    \
             2 tracks, from rip log; CUE sheet\n      \
             1  Someone/An Album/01.flac\n      \
             2  Someone/An Album/02.flac\n"
        );
    }
}
