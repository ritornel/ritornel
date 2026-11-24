//! `ritornelctl corrections discs`: the disc layouts the library's
//! sidecars give, and the disc ID each would be asked about by. See "Disc
//! layouts" in docs/internals/corrections.md.
//!
//! `ritornelctl corrections musicbrainz` and `lookup`: the switch that
//! lets the library be asked about at MusicBrainz, and asking about one
//! album or folder, which is the only way it is asked. See "MusicBrainz
//! lookups" there.
//!
//! `ritornelctl corrections list`, `show` and `preview`: what the replies
//! propose each disc is, and what a yes would change. See "Album
//! proposals" there.

use std::io::Write;

use ritornel_client::contract::methods::{
    CorrectionsAnswer, CorrectionsAnswers, CorrectionsConfirm, CorrectionsDiscs, CorrectionsGet,
    CorrectionsList, CorrectionsLookup, CorrectionsPreview, CorrectionsUndo, LibrarySetSettings,
};
use ritornel_client::contract::{
    AlbumShape, Answer, AnswerGot, AnswerList, AnswerParams, AnswerVerb, Answered, AnswersParams,
    Choice, ConfirmParams, Confirmation, Disc, DiscList, DiscsParams, FileValue, Id, LayoutMethod,
    LibrarySettingsSet, LookupDone, LookupOutcome, LookupParams, NoParams, PreviewParams,
    PreviewShown, Proposal, ProposalCandidate, ProposalChange, ProposalGetParams, ProposalGot,
    ProposalList, SetLibrarySettingsParams, UndoParams,
};
use ritornel_client::local;

use crate::cli::{
    AnswerArgs, AnswersArgs, ConfirmArgs, DiscsArgs, Error, LookupArgs, PreviewArgs, ProposalArgs,
    ServerArgs, SwitchArgs, UndoArgs,
};
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

pub(crate) fn list(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let list = server::ask(args, env, async |client| {
        client.call::<CorrectionsList>(&NoParams {}).await
    })?;
    server::print(args, out, &list, serde_json::to_string_pretty, render_list)
}

pub(crate) fn show(
    args: &ProposalArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let proposal_id = args
        .proposal
        .parse::<Id>()
        .map_err(|_| Error::Usage("show needs a proposal ID"))?;
    let got = server::ask(&args.server, env, async |client| {
        client
            .call::<CorrectionsGet>(&ProposalGetParams { proposal_id })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &got,
        serde_json::to_string_pretty,
        |got: &ProposalGot| render_proposal(&got.proposal),
    )
}

pub(crate) fn preview(
    args: &PreviewArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if args.candidate.is_some() && args.proposals.len() != 1 {
        return Err(Error::Usage("--candidate is for one proposal"));
    }
    if args.undo {
        if args.candidate.is_some() {
            return Err(Error::Usage("--candidate is not for an undo"));
        }
        let undo = answer_ids(&args.proposals)?;
        let shown = server::ask_for(&args.server, env, Some(LOOKUP_WAIT), async |client| {
            client
                .call::<CorrectionsPreview>(&PreviewParams {
                    choices: Vec::new(),
                    undo,
                })
                .await
        })?;
        return server::print(
            &args.server,
            out,
            &shown,
            serde_json::to_string_pretty,
            render_preview,
        );
    }
    let choices = args
        .proposals
        .iter()
        .map(|text| {
            Ok(Choice {
                proposal_id: text
                    .parse::<Id>()
                    .map_err(|_| Error::Usage("preview needs proposal IDs"))?,
                release_group: args.candidate.clone(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let shown = server::ask_for(&args.server, env, Some(LOOKUP_WAIT), async |client| {
        client
            .call::<CorrectionsPreview>(&PreviewParams {
                choices,
                undo: Vec::new(),
            })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &shown,
        serde_json::to_string_pretty,
        render_preview,
    )
}

/// A count of things, in words.
fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// A folder under its root, or the root itself.
fn place(root: &str, folder: &str) -> String {
    if folder.is_empty() {
        root.to_owned()
    } else {
        format!("{root}/{folder}")
    }
}

pub(crate) fn confirm(
    args: &ConfirmArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if args.candidate.is_some() && args.proposals.len() != 1 {
        return Err(Error::Usage("--candidate is for one proposal"));
    }
    if args.fingerprint.is_some() && args.proposals.len() != 1 {
        return Err(Error::Usage("--fingerprint is for one proposal"));
    }
    let ids = args
        .proposals
        .iter()
        .map(|text| {
            text.parse::<Id>()
                .map_err(|_| Error::Usage("confirm needs proposal IDs"))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let (candidate, fingerprint) = (args.candidate.clone(), args.fingerprint.clone());
    let done = server::ask_for(&args.server, env, Some(LOOKUP_WAIT), async |client| {
        // Each proposal as it is now: its fingerprint, and its candidate.
        let mut answers = Vec::new();
        for proposal_id in ids {
            let got = client
                .call::<CorrectionsGet>(&ProposalGetParams { proposal_id })
                .await?;
            let release_group = candidate.clone().or_else(|| {
                got.proposal
                    .candidates
                    .first()
                    .map(|first| first.release_group.clone())
            });
            answers.push(Confirmation {
                proposal_id,
                release_group: release_group.unwrap_or_default(),
                fingerprint: fingerprint.clone().unwrap_or(got.proposal.fingerprint),
            });
        }
        client
            .call::<CorrectionsConfirm>(&ConfirmParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                answers,
            })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &done,
        serde_json::to_string_pretty,
        render_answered,
    )
}

/// Answer IDs, as typed.
fn answer_ids(texts: &[String]) -> Result<Vec<Id>, Error> {
    texts
        .iter()
        .map(|text| {
            text.parse::<Id>()
                .map_err(|_| Error::Usage("undo needs answer IDs"))
        })
        .collect()
}

/// Undoes answers. The answers resting on them, which went with them, are
/// listed first.
pub(crate) fn undo(args: &UndoArgs, env: local::Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let answers = answer_ids(&args.answers)?;
    let asked = answers.clone();
    let done = server::ask_for(&args.server, env, Some(LOOKUP_WAIT), async |client| {
        client
            .call::<CorrectionsUndo>(&UndoParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                answers,
            })
            .await
    })?;
    let resting: Vec<Id> = done
        .answers
        .iter()
        .filter_map(|written| written.undoes)
        .filter(|answer| !asked.contains(answer))
        .collect();
    if !args.server.json && !resting.is_empty() {
        let mut text = String::from("These rested on it, and are undone with it:\n");
        for answer in resting {
            text.push_str(&format!("  {answer}\n"));
        }
        text.push('\n');
        out.write_all(text.as_bytes()).map_err(Error::Output)?;
    }
    server::print(
        &args.server,
        out,
        &done,
        serde_json::to_string_pretty,
        render_answered,
    )
}

pub(crate) fn answers(
    args: &AnswersArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let before = args
        .before
        .as_deref()
        .map(|text| {
            text.parse::<Id>()
                .map_err(|_| Error::Usage("--before needs an answer ID"))
        })
        .transpose()?;
    let params = AnswersParams {
        before,
        limit: args.limit,
    };
    let list = server::ask(&args.server, env, async |client| {
        client.call::<CorrectionsAnswers>(&params).await
    })?;
    server::print(
        &args.server,
        out,
        &list,
        serde_json::to_string_pretty,
        render_answers,
    )
}

pub(crate) fn answer(
    args: &AnswerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let answer_id = args
        .answer
        .parse::<Id>()
        .map_err(|_| Error::Usage("answer needs an answer ID"))?;
    let got = server::ask(&args.server, env, async |client| {
        client
            .call::<CorrectionsAnswer>(&AnswerParams { answer_id })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &got,
        serde_json::to_string_pretty,
        render_answer,
    )
}

fn render_answered(done: &Answered) -> String {
    let mut text = String::new();
    for answer in &done.answers {
        match (answer.proposal_id, answer.undoes) {
            (Some(proposal), _) => {
                text.push_str(&format!("{}  yes to {proposal}\n", answer.answer_id))
            }
            (_, Some(undone)) => text.push_str(&format!("{}  undid {undone}\n", answer.answer_id)),
            _ => {}
        }
    }
    text.push_str(if done.moved {
        "The library changed.\n"
    } else {
        "Nothing shown changed.\n"
    });
    text
}

fn render_answers(list: &AnswerList) -> String {
    if list.answers.is_empty() {
        return "No answers yet.\n".to_owned();
    }
    let mut text = String::new();
    for answer in &list.answers {
        text.push_str(&answer_line(answer));
    }
    if let Some(next) = list.next {
        text.push_str(&format!("\nMore with --before {next}\n"));
    }
    text
}

fn render_answer(got: &AnswerGot) -> String {
    let mut text = answer_line(&got.answer);
    let evidence = &got.evidence;
    if let Some(candidate) = &evidence.candidate {
        text.push_str(&format!(
            "\nSaid yes to: {} by {} ({})\n",
            candidate.title.as_deref().unwrap_or("an untitled album"),
            candidate.artist.as_deref().unwrap_or("?"),
            candidate.release_group
        ));
    }
    if evidence.candidates.len() > 1 {
        text.push_str("Chosen among:\n");
        for shown in &evidence.candidates {
            text.push_str(&format!(
                "  {} ({})\n",
                shown.title.as_deref().unwrap_or("an untitled album"),
                shown.release_group
            ));
        }
    }
    if !evidence.replies.is_empty() {
        text.push_str("Rested on:\n");
        for reply in &evidence.replies {
            let found: Vec<String> = evidence
                .layouts
                .iter()
                .filter(|layout| layout.disc_id == reply.disc_id)
                .map(|layout| {
                    let ways: Vec<&str> = layout.methods.iter().map(|way| method(*way)).collect();
                    format!("{} ({})", layout.folder, ways.join(", "))
                })
                .collect();
            text.push_str(&format!("  disc {}: {}\n", reply.disc_id, found.join("; ")));
        }
    }
    if !evidence.values.is_empty() {
        text.push_str("On the files:\n");
        for value in &evidence.values {
            text.push_str(&format!(
                "  {}/{}  {}\n",
                value.root,
                value.path,
                change_of(value)
            ));
        }
    }
    if !evidence.rests_on.is_empty() {
        text.push_str("Undone with:\n");
        for answer in &evidence.rests_on {
            text.push_str(&format!("  {answer}\n"));
        }
    }
    text
}

fn answer_line(answer: &Answer) -> String {
    let said = match answer.verb {
        AnswerVerb::Confirm => format!(
            "yes: {} ({}), {}",
            answer.title.as_deref().unwrap_or("an album"),
            answer.release_group.as_deref().unwrap_or("?"),
            plural(answer.files, "file", "files")
        ),
        AnswerVerb::Undo => format!(
            "undid {}",
            answer.undoes.map(|id| id.to_string()).unwrap_or_default()
        ),
    };
    let undone = answer
        .undone_by
        .map_or_else(String::new, |by| format!(", undone by {by}"));
    format!("{}  {said}{undone}\n", answer.answer_id)
}

fn change(change: ProposalChange) -> &'static str {
    match change {
        ProposalChange::Knows => "adds an ID",
        ProposalChange::Gains => "adds a kind or a first year",
        ProposalChange::Moves => "moves files",
    }
}

fn candidate(candidate: &ProposalCandidate) -> String {
    let year = candidate
        .first_year
        .map_or_else(String::new, |year| format!(" ({year})"));
    let title = candidate.title.as_deref().unwrap_or("an untitled album");
    let releases = if candidate.releases == 1 {
        "1 release".to_owned()
    } else {
        format!("{} releases", candidate.releases)
    };
    format!(
        "{title} by {}{year}, {releases}: {}",
        candidate.artist,
        change(candidate.change)
    )
}

fn render_list(list: &ProposalList) -> String {
    if list.proposals.is_empty() {
        return "Nothing is proposed.\n".to_owned();
    }
    let mut text = String::new();
    let mut group = None;
    for proposal in &list.proposals {
        if group != Some(proposal.group_id) {
            if group.is_some() {
                text.push('\n');
            }
            group = Some(proposal.group_id);
        }
        let files = if proposal.file_count == 1 {
            "1 file".to_owned()
        } else {
            format!("{} files", proposal.file_count)
        };
        text.push_str(&format!(
            "{}  {}, {files}\n",
            proposal.proposal_id,
            place(&proposal.root, &proposal.folder)
        ));
        if let Some(first) = proposal.candidates.first() {
            text.push_str(&format!("    {}\n", candidate(first)));
        }
    }
    text
}

fn render_proposal(proposal: &Proposal) -> String {
    let mut text = format!(
        "{}  {}\nFingerprint {}\n\nRests on:\n",
        proposal.proposal_id,
        place(&proposal.root, &proposal.folder),
        proposal.fingerprint
    );
    for evidence in &proposal.rests_on {
        let methods: Vec<&str> = evidence.methods.iter().copied().map(method).collect();
        text.push_str(&format!(
            "  disc {}, from {}; MusicBrainz names {}\n",
            evidence.disc_id,
            methods.join("; "),
            plural(evidence.releases, "release", "releases")
        ));
    }
    text.push_str("\nFiles:\n");
    for file in &proposal.files {
        text.push_str(&format!("  {file}\n"));
    }
    for (number, option) in (1..).zip(&proposal.candidates) {
        text.push_str(&format!(
            "\nCandidate {number}: {}\n  {}\n",
            option.release_group,
            candidate(option)
        ));
        for value in &option.values {
            text.push_str(&format!("  {}  {}\n", value.path, change_of(value)));
        }
    }
    text
}

fn shape(album: &AlbumShape) -> String {
    let title = album.title.as_deref().unwrap_or("an untitled album");
    let artist = album
        .artist
        .as_deref()
        .map_or_else(String::new, |artist| format!(" by {artist}"));
    let year = album
        .first_year
        .map_or_else(String::new, |year| format!(" ({year})"));
    let editions = if album.releases > 1 {
        format!(" in {} editions", album.releases)
    } else {
        String::new()
    };
    format!(
        "  {title}{artist}{year}: {}, {}{editions}\n",
        plural(album.discs, "disc", "discs"),
        plural(album.songs, "song", "songs")
    )
}

/// One value as it would change: the field, what it says now if known, and
/// what it would say.
fn change_of(value: &FileValue) -> String {
    let after = value
        .value
        .as_deref()
        .map_or_else(|| "nothing".to_owned(), |value| format!("\"{value}\""));
    match &value.now {
        Some(now) => format!("{}: \"{now}\" -> {after}", value.field),
        None => format!("{} = {after}", value.field),
    }
}

fn render_preview(shown: &PreviewShown) -> String {
    let mut text = String::new();
    if !shown.undoes.is_empty() {
        text.push_str("Undoes:\n");
        for answer in &shown.undoes {
            text.push_str(&format!("  {answer}\n"));
        }
        text.push('\n');
    }
    text.push_str("Now:\n");
    for album in &shown.now {
        text.push_str(&shape(album));
    }
    text.push_str("\nAfter:\n");
    for album in &shown.after {
        text.push_str(&shape(album));
    }
    // Songs that would be joined or parted are said first, as they move
    // what a song's plays and ratings are counted on.
    if shown.songs_joined > 0 {
        text.push_str(&format!(
            "\nJoins {} into others, as one song each.\n",
            plural(shown.songs_joined, "song", "songs")
        ));
    }
    if shown.songs_parted > 0 {
        text.push_str(&format!(
            "\nParts {} from the one each is now.\n",
            plural(shown.songs_parted, "song", "songs")
        ));
    }
    // What would change on each file, leaving out what it says already.
    let changing: Vec<&FileValue> = shown
        .values
        .iter()
        .filter(|value| value.now != value.value)
        .collect();
    if !changing.is_empty() {
        text.push_str("\nOn the files:\n");
        for value in changing {
            text.push_str(&format!(
                "  {}/{}  {}\n",
                value.root,
                value.path,
                change_of(value)
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
