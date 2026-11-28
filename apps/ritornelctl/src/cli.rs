//! The command line.
//!
//! ```text
//! ritornelctl status [--dismiss] [--data-dir <dir>] [--json]
//! ritornelctl server stop [--data-dir <dir>] [--json]
//! ritornelctl backup [--data-dir <dir>] [--json]
//! ritornelctl export <folder> [--data-dir <dir>] [--json]
//! ritornelctl library create [--data-dir <dir>] [--json]
//! ritornelctl library stats [--data-dir <dir>] [--json]
//! ritornelctl scan <folder> [--name <name>] [--no-wait] [--data-dir <dir>] [--json]
//! ritornelctl artists pictures <folder> [--name <name>] [--data-dir <dir>] [--json]
//! ritornelctl jobs [<job>] [--cancel] [--data-dir <dir>] [--json]
//! ritornelctl identity re-resolve --dry-run [--detail] [<policy values>] [--data-dir <dir>] [--json]
//! ritornelctl identity review [--data-dir <dir>] [--json]
//! ritornelctl corrections discs [<folder>] [--data-dir <dir>] [--json]
//! ritornelctl corrections musicbrainz <on | off> [--data-dir <dir>] [--json]
//! ritornelctl corrections lookup <folder> [--root <name>] | --album <release> [--again] [--data-dir <dir>] [--json]
//! ritornelctl corrections list [--data-dir <dir>] [--json]
//! ritornelctl corrections show <proposal> [--data-dir <dir>] [--json]
//! ritornelctl corrections preview <proposal>... [--candidate <release group>] [--release <release>] [--data-dir <dir>] [--json]
//! ritornelctl corrections preview --undo <answer>... [--data-dir <dir>] [--json]
//! ritornelctl corrections confirm <proposal>... [--candidate <release group>] [--release <release>] [--fingerprint <f>] [--data-dir <dir>] [--json]
//! ritornelctl corrections undo <answer>... [--data-dir <dir>] [--json]
//! ritornelctl corrections decline <proposal> [--candidate <release group>] [--release <release>] [--data-dir <dir>] [--json]
//! ritornelctl corrections stop <proposal> [--data-dir <dir>] [--json]
//! ritornelctl corrections apart <release> <release> [--data-dir <dir>] [--json]
//! ritornelctl corrections answers [--before <answer>] [--limit <n>] [--data-dir <dir>] [--json]
//! ritornelctl corrections answer <answer> [--data-dir <dir>] [--json]
//! ritornelctl play <folder | track>... [--album <release>] [--start <n>] [--follow]
//! ritornelctl pause | resume | stop | next | previous | now | queue
//! ritornelctl seek <position>
//! ritornelctl volume [<level> | mute | unmute]
//! ritornelctl history [--limit <n>]
//! ritornelctl history artist | album <name or ID>
//! ritornelctl history artists | albums [--by <plays | first | last>] [--limit <n>]
//! ritornelctl history on-this-day [--day <MM-DD>]
//! ritornelctl history rebuild [--dry-run]
//! ritornelctl history zones
//! ritornelctl history zone add <zone> [--from <YYYY-MM-DD>] [--until <YYYY-MM-DD>]
//! ritornelctl history zone clear
//! ritornelctl show <track>
//! ritornelctl lastfm status | signin | signout
//! ritornelctl lastfm sync [--full] [--no-wait]
//! ritornelctl lastfm report [--limit <n>]
//! ritornelctl lastfm scrobbling <on | off>
//! ritornelctl link invite [--route <host:port>]...
//! ritornelctl link join <link>
//! ritornelctl link status
//! ritornelctl settings
//! ritornelctl settings awake <while-active [<minutes>] | always | never>
//! ritornelctl spotify read <archive>
//! ritornelctl spotify import <archive> [--samples]
//! ritornelctl spotify apply | undo <run>
//! ritornelctl spotify runs
//! ```
//!
//! Every command takes `--json`, and every command that talks to a server
//! `--data-dir <dir>`.
//!
//! clap parses it. The help is [`USAGE`] and the messages are the fixed
//! ones in [`usage`], so nothing a person typed is ever printed back.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;

use clap::error::{ContextKind, ErrorKind};
use clap::{Args, Parser, Subcommand};
use ritornel_client::contract::SkipDirection;
use ritornel_client::contract::local::Env;

use crate::{
    artists, backup, corrections, export, history, identity, jobs, lastfm, library, link, playback,
    scan, settings, spotify, status,
};

const USAGE: &str = "\
The command-line client of a Ritornel server. It talks to the server that is
running on a data directory, and never starts one.

Usage:
  ritornelctl status [--dismiss] [--data-dir <dir>] [--json]
  ritornelctl server stop [--data-dir <dir>] [--json]
  ritornelctl transcode encoders | stats | prune [--data-dir <dir>] [--json]
  ritornelctl transcode run --profile <name> <appearance>... [--data-dir <dir>] [--json]
  ritornelctl devices list [--data-dir <dir>] [--json]
  ritornelctl devices show | plan | formats | plays | sync | stop | set | rename
                      | ignore | forget | eject <device> [--data-dir <dir>] [--json]
  ritornelctl devices add-folder <folder> [--name <name>] [--for <purpose>]
                                 [--removable] [--limit <size>]
                                 [--data-dir <dir>] [--json]
  ritornelctl backup [--data-dir <dir>] [--json]
  ritornelctl export <folder> [--data-dir <dir>] [--json]
  ritornelctl library create [--data-dir <dir>] [--json]
  ritornelctl library stats [--data-dir <dir>] [--json]
  ritornelctl scan <folder> [--name <name>] [--no-wait] [--data-dir <dir>] [--json]
  ritornelctl artists pictures <folder> [--name <name>] [--data-dir <dir>] [--json]
  ritornelctl jobs [<job>] [--cancel] [--data-dir <dir>] [--json]
  ritornelctl identity re-resolve --dry-run [--detail] [<policy values>]
                                  [--data-dir <dir>] [--json]
  ritornelctl identity review [--data-dir <dir>] [--json]
  ritornelctl corrections discs [<folder>] [--data-dir <dir>] [--json]
  ritornelctl corrections musicbrainz <on | off> [--data-dir <dir>] [--json]
  ritornelctl corrections lookup <folder> [--root <name>] | --album <release>
                                 [--again] [--data-dir <dir>] [--json]
  ritornelctl corrections list [--data-dir <dir>] [--json]
  ritornelctl corrections show <proposal> [--data-dir <dir>] [--json]
  ritornelctl corrections preview <proposal>... [--candidate <release group>]
                                  [--release <release>] [--data-dir <dir>] [--json]
  ritornelctl corrections preview --undo <answer>... [--data-dir <dir>] [--json]
  ritornelctl corrections confirm <proposal>... [--candidate <release group>]
                                  [--release <release>] [--fingerprint <f>]
                                  [--data-dir <dir>] [--json]
  ritornelctl corrections undo <answer>... [--data-dir <dir>] [--json]
  ritornelctl corrections decline <proposal> [--candidate <release group>]
                                  [--release <release>] [--data-dir <dir>]
                                  [--json]
  ritornelctl corrections stop <proposal> [--data-dir <dir>] [--json]
  ritornelctl corrections apart <release> <release> [--data-dir <dir>] [--json]
  ritornelctl corrections answers [--before <answer>] [--limit <n>]
                                  [--data-dir <dir>] [--json]
  ritornelctl corrections answer <answer> [--data-dir <dir>] [--json]
  ritornelctl play <folder | track>... [--album <release>] [--start <n>]
                   [--follow] [--data-dir <dir>] [--json]
  ritornelctl pause | resume | stop | next | previous [--data-dir <dir>] [--json]
  ritornelctl seek <position> [--data-dir <dir>] [--json]
  ritornelctl volume [<level> | mute | unmute] [--data-dir <dir>] [--json]
  ritornelctl now | queue [--data-dir <dir>] [--json]
  ritornelctl history [--limit <n>] [--data-dir <dir>] [--json]
  ritornelctl history artist | album <name or ID> [--data-dir <dir>] [--json]
  ritornelctl history artists | albums [--by <plays | first | last>] [--limit <n>]
                                       [--data-dir <dir>] [--json]
  ritornelctl history on-this-day [--day <MM-DD>] [--data-dir <dir>] [--json]
  ritornelctl history rebuild [--dry-run] [--data-dir <dir>] [--json]
  ritornelctl history zones [--data-dir <dir>] [--json]
  ritornelctl history zone add <zone> [--from <YYYY-MM-DD>] [--until <YYYY-MM-DD>]
                               [--data-dir <dir>] [--json]
  ritornelctl history zone clear [--data-dir <dir>] [--json]
  ritornelctl show <track> [--data-dir <dir>] [--json]
  ritornelctl lastfm status | signin | signout [--data-dir <dir>] [--json]
  ritornelctl lastfm sync [--full] [--no-wait] [--data-dir <dir>] [--json]
  ritornelctl lastfm report [--limit <n>] [--data-dir <dir>] [--json]
  ritornelctl lastfm scrobbling <on | off> [--data-dir <dir>] [--json]
  ritornelctl link invite [--route <host:port>]... [--data-dir <dir>] [--json]
  ritornelctl link join <link> [--data-dir <dir>] [--json]
  ritornelctl link status [--data-dir <dir>] [--json]
  ritornelctl settings [--data-dir <dir>] [--json]
  ritornelctl settings awake <while-active [<minutes>] | always | never>
                             [--data-dir <dir>] [--json]
  ritornelctl spotify read <archive> [--json]
  ritornelctl spotify import <archive> [--samples] [--data-dir <dir>] [--json]
  ritornelctl spotify apply | undo <run> [--data-dir <dir>] [--json]
  ritornelctl spotify runs [--data-dir <dir>] [--json]

Commands:
  status           Show the running server's ID, name and role, the versions
                   it is built from, the library it holds, the free disk
                   space, where its logs are, its last backup, and whether
                   the listening record advances: each listen saved and
                   in history, each scrobble answered, each import done.
                   Scrobbles that ended unanswered or were not sent are
                   said until --dismiss sets them aside
  server stop      Stop the server on the data directory, cleanly, as the
                   system does at shutdown: the player pauses and keeps its
                   place. Only the user's own server, on this machine. The
                   desktop app starts it again when it opens
  backup           Copy the server's databases, with the listening history,
                   into a new folder in the server's backup folder, while
                   it goes on playing. The backup folder is named when the
                   server starts (`ritornel-server serve --backup-dir`), and
                   with none named there is no backup
  export <folder>  Write the listening record into a new folder in
                   <folder>, to be read without Ritornel: every listen,
                   imported scrobble and correction, and every rating and
                   love, as JSON Lines, and the plays as a CSV table.
                   manifest.json says what each file holds
  library create   Create a library on the server and make the server its
                   hub. A server holds one library, and one that has a
                   library already refuses
  library stats    Show what the library holds and what the scans made of
                   it: files, what could not be read and why, artists,
                   albums and songs, how each file was matched, and how
                   many matches want a look
  scan <folder>    Scan a folder of music into the library, and show the
                   scan until it ends. The folder is added as a source if
                   it is not one. The server only ever reads it. The scan
                   is the server's: it carries on if this program stops
  artists pictures <folder>
                   Take each artist's portrait from a picture folder, by
                   MusicBrainz ID, or by name for an artist with none. The
                   folder is added as a root, and the server only ever
                   reads it. Run it again after the folder changes: what it
                   no longer chooses is dropped
  transcode encoders, transcode stats
                   Show detected encoders or cache usage, workers and profiles
  transcode run    Encode each appearance ID with --profile into the cache,
                   and say what became of each: encoded, as it is, blocked
                   or failed
  transcode prune  Remove the cached encodes nothing holds
  devices list     List the players and folders the library syncs to. In
                   every other devices command a device is named by its ID
                   or by its name
  devices add-folder <folder>
                   Add a folder, a drive or a card on this server to sync
                   to. --for says what it is made for (car, phone, player
                   or copy), which sets the formats it plays; --removable
                   says it comes and goes, and is ejected. Nothing is
                   written there until its first sync
  devices show, plan, formats, plays <device>
                   Show a device's page, the rows of its next sync
                   (--tracks for one a track, --kind to filter), what its
                   music would take in each format, or its plays
  devices sync <device>
                   Sync it at the plan as it stands, and wait. --setup sets
                   a new device up first, --accept-cut agrees to what does
                   not fit, --skip-retags leaves tag changes for another day
  devices stop <device>
                   Stop the sync under way after the file in hand
  devices set <device>
                   Change what goes on it and in what form: --source,
                   --always and --leave-out take everything, artist:<id>,
                   album:<id> or added:<days>; --format takes as-is, mp3-v0,
                   mp3-v2, mp3-320 or opus-160; --cover <px>, --id3v1 on|off,
                   --limit <size>, --for <purpose>, --plays <codecs>
  devices rename, ignore, forget, eject <device>
                   Rename it, leave it alone (--off to stop), end it, or
                   make it safe to unplug
  jobs [<job>]     List the server's jobs, newest first, or show one with
                   what its scan found and what could not be read
  identity re-resolve --dry-run
                   Run the resolver again over what the library has
                   stored, and say for each file whether the result is
                   unchanged, would change, is pinned by your own
                   assignment or blocked by your exclusion. It writes
                   nothing. It decides by the policy scans use, policy
                   2, unless the values below ask what another policy or
                   other values would change
  identity review  List the matches marked for review, grouped by cause:
                   each file, the one it nearly matched, the distance and
                   what made it up
  corrections discs [<folder>]
                   List the CD layouts read from the rip logs, CUE sheets
                   and checksum reports beside the audio, each with its
                   MusicBrainz disc ID and how it was found. With a
                   folder, under its root, show that folder's alone, with
                   the file at each track
  corrections musicbrainz on | off
                   Let `corrections lookup` ask MusicBrainz about this
                   library's discs, or stop it. It is off until it is
                   turned on, and even on, nothing is asked by itself:
                   only the album or folder a lookup names. What is sent
                   is disc IDs, never a path, a file or anything about you
  corrections lookup <folder>
                   Ask MusicBrainz about the discs of a folder, under its
                   root, or of a release with --album, and keep each
                   reply. A disc asked about before is not asked again
                   unless --again says to
  corrections list Show what MusicBrainz's replies propose each disc is,
                   by group: the album each disc's files may be on, and
                   whether saying yes would move files
  corrections show <proposal>
                   Show one proposal whole: its files, the discs it rests
                   on, and each candidate with the tag values a yes would
                   put on each file
  corrections preview <proposal>...
                   Show what saying yes to proposals together would
                   change: the albums now and after, and the songs it
                   would join or part. With --undo, show what undoing
                   answers would give back, and every answer that rests
                   on them and would go with them. It writes nothing
  corrections confirm <proposal>...
                   Say yes to proposals together, each with its likeliest
                   candidate or the one --candidate names: the album's ID,
                   kind and first year go on the files, and a bonus disc
                   joins its album. A yes to an edition, with its
                   likeliest release or the one --release names, puts the
                   discs on that release, each file at its disc and track;
                   it is said with or after the album's yeses. Each
                   proposal is read as it is when this runs, or, for one,
                   as --fingerprint says `show` printed it, and the answer
                   is refused if it changed since. Every answer can be
                   undone
  corrections undo <answer>...
                   Undo answers, each alone or together: the files go back
                   to what they were, on the albums they were on. An
                   answer that took its album's title or credit from one
                   undone rests on it, and is undone with it: those are
                   listed first
  corrections decline <proposal>
                   Say no to a proposal's likeliest candidate, or the one
                   --candidate or --release names: it is not offered these
                   files again. It moves nothing, and can be undone
  corrections stop <proposal>
                   Ask no more about a proposal's files, by any proposal,
                   until the answer is undone
  corrections apart <release> <release>
                   Keep two albums apart: the files on each are never put
                   on one release or in one release group with the
                   other's, by a proposal or by the catalog. Undone like
                   any answer
  corrections answers
                   Show the answers, newest first, a page at a time: 50,
                   or --limit, and the next page with the --before it
                   names
  corrections answer <answer>
                   Show one answer whole: the album it said yes to and the
                   others it was chosen among, the discs and replies it
                   rested on, and the values it put on each file, as they
                   were when it was given
  play             Play the scanned tracks in a folder, album by album in
                   disc and track order, or tracks by their IDs, or a
                   release with --album. It replaces the queue. The server
                   plays on when this program ends
  pause, resume    Pause, and play on. After the server restarts, resume
                   carries on where the last run stopped
  stop             Stop playing. Resume plays the track from its beginning.
                   The server runs on; `server stop` stops it
  next, previous   Skip forward, or back. Previous starts the track again
                   when it is more than three seconds in
  seek <position>  Move the playing track to a position: seconds, or m:ss
  volume [<level>] Show the player's own volume, or set it from 0 to 100,
                   or mute or unmute it. It is not the system's volume
  now              Show what is playing, where, from which copy and output
  queue            Show the queue, the current entry marked
  history          Show recent listening, newest first: each play, what
                   became of it, and whether it counts
  history artist <artist>, history album <album>
                   Show what an artist's page or an album's says of your
                   history with it: its sentence, its plays by year and,
                   for an artist, the records played most. Only plays
                   matched to a song in the library count. Name it by its
                   ID, or by its name when that names one
  history artists, history albums
                   List every artist or album you have heard, with files
                   or not: how often, and from when to when. Only plays
                   that count are counted
  history on-this-day
                   Show what you listened to on this day in every year
  history rebuild  Drop the plays and derive them again from every listen
                   recorded, in one transaction
  history zones    Show where you were, by date: the zone a play is dated
                   in when its source gave none, as Last.fm does not
  history zone add <zone>
                   Say you were in a zone, by its IANA name such as
                   Europe/London, from a day and until a day, or always.
                   The plays are dated again with it
  history zone clear
                   Forget every zone
  show <track>     Show a track by its ID: what it is, how often it was
                   played, and who loved it
  lastfm status    Show whether the server has Last.fm, the account it is
                   signed in to, what was imported and what scrobbled
  lastfm signin    Sign the server in to Last.fm: open the address it
                   prints and approve it there. The session is kept on the
                   server. It needs the server to have an API key and
                   secret, in SCROBL_API_KEY and SCROBL_API_SECRET where it
                   was started, or in lastfm.env in its data directory
  lastfm signout   Forget the session. What was imported stays
  lastfm sync      Import what is new in your Last.fm history, and your
                   loved tracks, and show it until it ends. The first sync
                   reads all of it; each after reads from a fortnight
                   before the last. It is the server's job: it carries on
                   if this program stops, and after a restart
  lastfm report    Show how much of the imported history matched the
                   library, and the tracks it was not sure of
  lastfm scrobbling on | off
                   Send the plays that count to Last.fm from now on, or
                   stop
  link invite      On a library's hub that listens on the private network
                   (`ritornel-server serve --listen private:<port>`), make a
                   link another server joins with to become its satellite:
                   it browses this library, plays its music and sends its
                   listens here. The link works once, for ten minutes, and
                   holds a secret. --route names where the hub is reached,
                   an address or a name such as a tailnet's, in place of
                   the routes it finds: its .local name and its addresses
  link join <link> Make this server, which holds no library, the satellite
                   of the hub that made the link
  link status      Show this server's hub, or its satellites, and when it
                   keeps its machine awake
  settings         Show the running server's settings, a satellite's own on
                   a satellite, each with what gave it: the default, the
                   server's command line, its environment, config.toml in
                   its data directory, or a call such as `settings awake`
  settings awake   When this server keeps its machine from sleeping for its
                   linked servers: while one plays or has made a call
                   within the minutes given, 30 unless said, which is the
                   default; always; or never. The display may sleep. The
                   server keeps it
  spotify read <archive>
                   Read Spotify's extended streaming history, the zip file
                   Spotify sent or the folder it was unpacked to, where it
                   is, and say what it holds: its streams of music, the
                   rows that are not music, repeats and platforms. It keeps
                   when each stream ended, how long it played, the track
                   and its names, why it began and ended, shuffle, skips,
                   offline and private sessions, and the kind of device as
                   one word. It never keeps the IP address, user agent,
                   account name, country or the device. It needs no server,
                   and stores and sends nothing
  spotify import <archive>
                   Read the history as `spotify read` does, send what it
                   keeps to the library's hub, and show what importing it
                   would do, worked out without writing anything: which
                   streams are the same listens as Last.fm's scrobbles,
                   how many of the rest count, and the plays by year
                   before and after. The hub keeps what was sent for a day
  spotify apply <run>
                   Import a run that `spotify import` sent: store its
                   streams that the library does not have, and record the
                   run, in one change. A run applied again changes nothing
  spotify undo <run>
                   Take an applied run out of force. Its streams stay
                   stored, and those another run in force holds stay in
                   force. Importing the archive again brings it back
  spotify runs     List the runs applied to the library, and whether each
                   is in force

Options:
  --on <name>           playback and queue commands: choose a player by name
  --name <name>         scan, artists pictures: the name the folder is
                        added under. With none it is the folder's own name
  --no-wait             scan: start the scan and return at once
  --cancel              jobs: ask the job to stop. It stops after the files
                        it is reading, and what it has recorded stays
  --detail              re-resolve: list every file that is not unchanged
  --album <release>     corrections lookup: a release instead of a folder
  --again               corrections lookup: ask about discs that have a reply
  --root <name>         corrections lookup: the root the folder is in, when
                        more than one root has it
  --candidate <release group>
                        corrections preview, confirm: the candidate, for one
                        proposal

Policy values, for re-resolve --dry-run. A switch is on or off, and on
when given alone:
  --policy <n>          Start from policy 1, 2 or 3
  --bind-max <d>        A match by tags binds at this distance or less
  --min-gap <d>         and only this far ahead of the runner-up
  --review-below <d>    A new recording is marked for review if a candidate
                        was nearer than this
  --duration-grace-ms <ms>
                        A difference in length costs nothing up to this
  --fold-accents [on|off]
                        Take accents off names before comparing them
  --discs-from-folders [on|off]
                        Sibling folders of one album whose track numbers
                        collide, which share no title and no year, and
                        whose names each give a disc, are its discs
  --disc-from-album-title [on|off]
                        A trailing \"Disc 2\" or \"CD 2\" in an album title is
                        the disc number
  --tags-across-albums [on|off]
                        A match by tags and length alone may bind a file
                        to a recording on another album
  --across-albums-within-ms <ms>
                        When it may not, it still may if the lengths are
                        this close; 0 for never
  --tracks-of-one-release-differ [on|off]
                        A match by tags never binds a file to the
                        recording of another track of its own release
  --marker-list <n>     The version markers: 1, or 2 with every spelling
                        of \"a cappella\" and part numbers
  --album <release>     play: a release, by its ID
  --start <n>           play: the track to start at, from 1
  --follow              play: show each track as it starts, until the
                        queue ends
  --limit <n>           history: how many plays. The default is 20
  --day <MM-DD>         history on-this-day: the day. The default is today
  --by <order>          history artists, albums: plays, most first, the
                        default; first, first heard longest ago first; or
                        last, last heard most recently first
  --limit <n>           history artists, albums: how many. The default is
                        100
  --dry-run             history rebuild: say what would change, and change
                        nothing
  --from <YYYY-MM-DD>   history zone add: the first day
  --until <YYYY-MM-DD>  history zone add: the day after the last
  --full                lastfm sync: read the whole history again
  --no-wait             lastfm sync: start it and return at once
  --limit <n>           lastfm report: how many doubtful tracks. The
                        default is 50
  --samples             spotify import: show a sample of what tiers B and C
                        join, for judging them
  --data-dir <dir>      The data directory of the server to talk to
  --json                Print the answer as JSON, in the shape the server
                        sends it in
  -h, --help            Show this help
  -V, --version         Show the version

Environment:
  RITORNEL_DATA_DIR     The data directory, when --data-dir is not given.
                        With neither it is
                        ~/Library/Application Support/Ritornel on macOS,
                        and $XDG_DATA_HOME/ritornel elsewhere, which is
                        ~/.local/share/ritornel unless you moved it

Exit codes:
  0    Done
  1    Failed
  2    The command line was wrong
  4    No server is running on the data directory
";

/// The exit code of a command that failed for a reason with no code of its
/// own.
pub(crate) const EXIT_FAILED: u8 = 1;

/// The exit code when the command line was wrong.
pub(crate) const EXIT_USAGE: u8 = 2;

/// The exit code reserved for one case: no server is running on the data
/// directory. It is not 3, which `ritornel-server` uses for the opposite
/// case, a data directory that a server has.
pub(crate) const EXIT_NO_SERVER: u8 = 4;

/// Why a command did not do what it was asked.
///
/// `Display` is one line that is safe to print.
#[derive(Debug)]
pub(crate) enum Error {
    /// The command line was wrong. The text is fixed, so it repeats nothing
    /// it was given.
    Usage(&'static str),
    /// The server could not be found or reached, or it answered with an
    /// error.
    Client(ritornel_client::Error),
    /// The server took too long to answer.
    TimedOut,
    /// The async runtime could not be started.
    Runtime(std::io::Error),
    /// The answer could not be written to standard output.
    Output(std::io::Error),
    /// The folder to scan could not be found.
    Folder(std::io::Error),
    /// Spotify's history could not be read.
    Archive(spotify::ArchiveError),
    /// What `play` was given is no folder there is.
    PlayFolder(std::io::Error),
    /// The job that was waited for did not succeed. The text is its state.
    Job(&'static str),
    /// There was nothing to do it to.
    Nothing(&'static str),
    /// The server was asked to stop and had not when the wait ended.
    StillStopping,
}

impl Error {
    /// The exit code a process ends with after this error.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Client(ritornel_client::Error::NotRunning { .. }) => EXIT_NO_SERVER,
            _ => EXIT_FAILED,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Client(error) => write!(f, "{error}"),
            Self::TimedOut => f.write_str("the server did not answer in time"),
            Self::Runtime(error) => write!(f, "cannot start: {error}"),
            Self::Output(error) => write!(f, "cannot write to standard output: {error}"),
            Self::Folder(error) => write!(f, "cannot scan that folder: {error}"),
            Self::Archive(error) => write!(f, "{error}"),
            Self::PlayFolder(error) => write!(f, "cannot play that folder: {error}"),
            Self::Job(state) => write!(f, "the job {state}"),
            Self::Nothing(what) => f.write_str(what),
            Self::StillStopping => f.write_str("the server was asked to stop and has not yet"),
        }
    }
}

impl From<ritornel_client::Error> for Error {
    fn from(error: ritornel_client::Error) -> Self {
        Self::Client(error)
    }
}

/// The command line, as clap reads it.
#[derive(Debug, PartialEq, Eq, Parser)]
#[command(name = "ritornelctl", version = env!("RITORNEL_VERSION_DISPLAY"), propagate_version = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[arg(long, global = true, value_name = "name")]
    on: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    Status(StatusArgs),
    Backup(ServerArgs),
    #[command(subcommand)]
    Transcode(crate::transcode::TranscodeCommand),
    #[command(subcommand)]
    Devices(crate::devices::DevicesCommand),
    Export(ExportArgs),
    #[command(subcommand)]
    Library(LibraryCommand),
    Scan(ScanArgs),
    #[command(subcommand)]
    Artists(ArtistsCommand),
    Jobs(JobsArgs),
    #[command(subcommand)]
    Identity(IdentityCommand),
    #[command(subcommand)]
    Corrections(CorrectionsCommand),
    Play(PlayArgs),
    Pause(ServerArgs),
    Resume(ServerArgs),
    Stop(ServerArgs),
    Next(ServerArgs),
    Previous(ServerArgs),
    Seek(SeekArgs),
    Volume(VolumeArgs),
    Now(ServerArgs),
    Queue(ServerArgs),
    History(HistoryArgs),
    Show(ShowArgs),
    #[command(subcommand)]
    Lastfm(LastfmCommand),
    #[command(subcommand)]
    Link(LinkCommand),
    #[command(subcommand)]
    Server(ServerCommand),
    Settings(SettingsArgs),
    #[command(subcommand)]
    Spotify(SpotifyCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum SpotifyCommand {
    Read(SpotifyReadArgs),
    Import(SpotifyImportArgs),
    Apply(SpotifyRunArgs),
    Undo(SpotifyRunArgs),
    Runs(ServerArgs),
}

/// What `spotify apply` and `spotify undo` take.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SpotifyRunArgs {
    /// The run, by its ID.
    pub(crate) run: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `spotify import` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SpotifyImportArgs {
    /// The zip file Spotify sent, or the folder it was unpacked to.
    pub(crate) archive: PathBuf,
    /// Show a sample of what tiers B and C join.
    #[arg(long)]
    pub(crate) samples: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `spotify read` takes. It talks to no server.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SpotifyReadArgs {
    /// The zip file Spotify sent, or the folder it was unpacked to.
    pub(crate) archive: PathBuf,
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum ServerCommand {
    Stop(ServerArgs),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum LinkCommand {
    Invite(InviteArgs),
    Join(JoinArgs),
    Status(ServerArgs),
}

/// What `settings` takes. Its options are for showing the settings, and
/// one given before a command is refused, as for `history`.
#[derive(Debug, Default, PartialEq, Eq, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct SettingsArgs {
    #[command(subcommand)]
    pub(crate) command: Option<SettingsCommand>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum SettingsCommand {
    Awake(AwakeArgs),
}

/// What `settings awake` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct AwakeArgs {
    /// `while-active`, `always` or `never`.
    pub(crate) mode: String,
    /// For `while-active`, how many minutes after a linked server's last
    /// call.
    pub(crate) minutes: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `link invite` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct InviteArgs {
    /// Where the hub is reached, in order.
    #[arg(long)]
    pub(crate) route: Vec<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `link join` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct JoinArgs {
    /// The link `link invite` made on the hub.
    pub(crate) link: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `status` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct StatusArgs {
    /// Set aside every scrobble note and warning first: the scrobbles that
    /// ended unknown, withdrawn or refused.
    #[arg(long)]
    pub(crate) dismiss: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `export` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ExportArgs {
    /// The folder the export's own folder is made in.
    pub(crate) folder: PathBuf,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `show` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ShowArgs {
    /// The track, by its ID.
    pub(crate) track: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum LastfmCommand {
    Status(ServerArgs),
    Signin(ServerArgs),
    Signout(ServerArgs),
    Sync(SyncArgs),
    Report(ReportArgs),
    Scrobbling(ScrobblingArgs),
}

/// What `lastfm sync` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SyncArgs {
    /// Read the whole history again.
    #[arg(long)]
    pub(crate) full: bool,
    /// Start it and return at once.
    #[arg(long)]
    pub(crate) no_wait: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `lastfm report` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ReportArgs {
    /// How many doubtful tracks.
    #[arg(long)]
    pub(crate) limit: Option<u32>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `lastfm scrobbling` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ScrobblingArgs {
    /// `on` or `off`.
    pub(crate) state: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `play` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct PlayArgs {
    /// A folder, or track IDs.
    pub(crate) what: Vec<String>,
    /// A release, by its ID.
    #[arg(long)]
    pub(crate) album: Option<String>,
    /// The track to start at, from 1.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) start: Option<u32>,
    /// Show each track as it starts, until the queue ends.
    #[arg(long)]
    pub(crate) follow: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `seek` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SeekArgs {
    /// Seconds, or m:ss.
    pub(crate) position: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `volume` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct VolumeArgs {
    /// 0 to 100, `mute` or `unmute`. With none, the volume is shown.
    pub(crate) level: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `history` takes. Its options are for the recent plays alone, and
/// one given before a command is refused: each command takes its own, and
/// one read here and then ignored would act on the wrong data directory.
#[derive(Debug, Default, PartialEq, Eq, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct HistoryArgs {
    #[command(subcommand)]
    pub(crate) command: Option<HistoryCommand>,
    /// How many plays.
    #[arg(long)]
    pub(crate) limit: Option<u32>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum HistoryCommand {
    Artist(OfArgs),
    Album(OfArgs),
    Artists(HeardArgs),
    Albums(HeardArgs),
    OnThisDay(DayArgs),
    Rebuild(RebuildArgs),
    Zones(ServerArgs),
    #[command(subcommand)]
    Zone(ZoneCommand),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum ZoneCommand {
    Add(ZoneArgs),
    Clear(ServerArgs),
}

/// What `history zone add` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ZoneArgs {
    /// The zone, by its IANA name.
    pub(crate) zone: String,
    /// The first day, as YYYY-MM-DD.
    #[arg(long)]
    pub(crate) from: Option<String>,
    /// The day after the last, as YYYY-MM-DD.
    #[arg(long)]
    pub(crate) until: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `history artist` and `history album` take.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct OfArgs {
    /// The artist or the album, by its ID or by its name.
    pub(crate) name: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `history artists` and `history albums` take.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct HeardArgs {
    /// The order: plays, first or last.
    #[arg(long, value_parser = ["plays", "first", "last"])]
    pub(crate) by: Option<String>,
    /// How many.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) limit: Option<u32>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `history on-this-day` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct DayArgs {
    /// The day, as MM-DD.
    #[arg(long)]
    pub(crate) day: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `history rebuild` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct RebuildArgs {
    /// Say what would change, and change nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum LibraryCommand {
    Create(ServerArgs),
    Stats(ServerArgs),
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum CorrectionsCommand {
    Discs(DiscsArgs),
    Musicbrainz(SwitchArgs),
    Lookup(LookupArgs),
    List(ServerArgs),
    Show(ProposalArgs),
    Preview(PreviewArgs),
    Confirm(ConfirmArgs),
    Undo(UndoArgs),
    Decline(DeclineArgs),
    Stop(ProposalArgs),
    Apart(ApartArgs),
    Answers(AnswersArgs),
    Answer(AnswerArgs),
}

/// What `corrections decline` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct DeclineArgs {
    /// The proposal, by its ID.
    pub(crate) proposal: String,
    /// The candidate to say no to, by its release group ID: the likeliest
    /// when left out.
    #[arg(long)]
    pub(crate) candidate: Option<String>,
    /// For an edition: the release to say no to.
    #[arg(long)]
    pub(crate) release: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections apart` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ApartArgs {
    /// Two releases, by their IDs.
    #[arg(required = true, num_args = 2)]
    pub(crate) releases: Vec<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections answers` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct AnswersArgs {
    /// The answers older than this one, as the last page's `--before`
    /// said; from the newest when left out.
    #[arg(long)]
    pub(crate) before: Option<String>,
    /// How many at most.
    #[arg(long)]
    pub(crate) limit: Option<u32>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections answer` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct AnswerArgs {
    /// The answer, by its ID.
    pub(crate) answer: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections undo` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct UndoArgs {
    /// The answers, by their IDs.
    #[arg(required = true)]
    pub(crate) answers: Vec<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections show` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ProposalArgs {
    /// The proposal, by its ID.
    pub(crate) proposal: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections preview` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct PreviewArgs {
    /// The proposals, by their IDs, or with `--undo` the answers.
    #[arg(required = true)]
    pub(crate) proposals: Vec<String>,
    /// The candidate, by its release group ID, for one proposal.
    #[arg(long)]
    pub(crate) candidate: Option<String>,
    /// For one edition: the candidate release, by its ID.
    #[arg(long)]
    pub(crate) release: Option<String>,
    /// Preview undoing these answers instead.
    #[arg(long)]
    pub(crate) undo: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections confirm` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ConfirmArgs {
    /// The proposals, by their IDs.
    #[arg(required = true)]
    pub(crate) proposals: Vec<String>,
    /// The candidate, by its release group ID, for one proposal.
    #[arg(long)]
    pub(crate) candidate: Option<String>,
    /// For one edition: the candidate release, by its ID.
    #[arg(long)]
    pub(crate) release: Option<String>,
    /// The fingerprint `corrections show` printed, for one proposal: the
    /// answer is refused if the proposal changed since.
    #[arg(long)]
    pub(crate) fingerprint: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections musicbrainz` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct SwitchArgs {
    /// `on` or `off`.
    pub(crate) state: String,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections lookup` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct LookupArgs {
    /// A folder, by its path under its root.
    pub(crate) folder: Option<String>,
    /// A release, by its ID.
    #[arg(long)]
    pub(crate) album: Option<String>,
    /// Ask again about discs that have a reply.
    #[arg(long)]
    pub(crate) again: bool,
    /// The root the folder is in, when more than one has it.
    #[arg(long)]
    pub(crate) root: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `corrections discs` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct DiscsArgs {
    /// One folder, by its path under its root.
    pub(crate) folder: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum IdentityCommand {
    ReResolve(Box<ReResolveArgs>),
    Review(ServerArgs),
}

/// What `identity re-resolve` takes. The thresholds are text here and read
/// by [`identity`], so that a wrong one is told with a fixed message.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ReResolveArgs {
    /// Say what would change and write nothing: the only way it runs.
    #[arg(long)]
    pub(crate) dry_run: bool,
    /// List every file that is not unchanged.
    #[arg(long)]
    pub(crate) detail: bool,
    /// The named policy to start from: 1, 2 or 3. The one scans use when it
    /// is left out.
    #[arg(long)]
    pub(crate) policy: Option<String>,
    #[arg(long)]
    pub(crate) bind_max: Option<String>,
    #[arg(long)]
    pub(crate) min_gap: Option<String>,
    #[arg(long)]
    pub(crate) review_below: Option<String>,
    #[arg(long)]
    pub(crate) duration_grace_ms: Option<String>,
    #[arg(long)]
    pub(crate) across_albums_within_ms: Option<String>,
    /// Each switch is `on` or `off`, and on when given alone.
    #[arg(long, num_args = 0..=1, default_missing_value = "on")]
    pub(crate) fold_accents: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "on")]
    pub(crate) discs_from_folders: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "on")]
    pub(crate) disc_from_album_title: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "on")]
    pub(crate) tags_across_albums: Option<String>,
    #[arg(long, num_args = 0..=1, default_missing_value = "on")]
    pub(crate) tracks_of_one_release_differ: Option<String>,
    #[arg(long)]
    pub(crate) marker_list: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `scan` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ScanArgs {
    /// The folder to scan.
    pub(crate) folder: PathBuf,
    /// The name the folder is added under, if it is not a source yet.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Start the scan and return at once.
    #[arg(long)]
    pub(crate) no_wait: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
pub(crate) enum ArtistsCommand {
    Pictures(PicturesArgs),
}

/// What `artists pictures` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct PicturesArgs {
    /// The picture folder.
    pub(crate) folder: PathBuf,
    /// The name the folder is added under.
    #[arg(long)]
    pub(crate) name: Option<String>,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What `jobs` takes.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct JobsArgs {
    /// The job to show. With none, the jobs are listed.
    pub(crate) job: Option<String>,
    /// Ask the job to stop.
    #[arg(long)]
    pub(crate) cancel: bool,
    #[command(flatten)]
    pub(crate) server: ServerArgs,
}

/// What every command takes: which server to talk to, and how to print
/// what it answers.
#[derive(Debug, Default, PartialEq, Eq, Args)]
pub(crate) struct ServerArgs {
    /// A token that looks like an option is never the value: `--data-dir
    /// --json` is a missing directory, not a directory named `--json`.
    /// One whose name does begin with a hyphen is written `./-name`.
    #[arg(long)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) json: bool,
}

/// What a wrong command line is answered with. The messages are fixed and
/// repeat no argument: clap's own would print back what was typed.
fn usage(error: &clap::Error) -> Error {
    Error::Usage(match error.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => {
            "unknown argument; try `ritornelctl --help`"
        }
        ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            "expected a command; try `ritornelctl --help`"
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation if names(error, "--data-dir") => {
            "--data-dir needs a directory"
        }
        _ => "the command line is wrong; try `ritornelctl --help`",
    })
}

/// Whether the option an error is about is `option`. The context holds the
/// option as this program declares it, never what was typed.
fn names(error: &clap::Error, option: &str) -> bool {
    error
        .get(ContextKind::InvalidArg)
        .is_some_and(|arg| arg.to_string().starts_with(option))
}

/// Runs the program and returns its exit code. What was asked for goes to
/// `out`, and an error to `err`.
pub(crate) fn run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match try_run(args, env, out, err) {
        Ok(()) => 0,
        Err(error) => {
            // Nothing useful can be done if the terminal is gone.
            let _ = writeln!(err, "ritornelctl: {error}");
            error.exit_code()
        }
    }
}

/// Parses the arguments after the program name.
fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Cli, clap::Error> {
    Cli::try_parse_from(std::iter::once(OsString::from(env!("CARGO_PKG_NAME"))).chain(args))
}

fn try_run(
    args: impl IntoIterator<Item = OsString>,
    env: Env<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Error> {
    let cli = match parse(args) {
        Ok(cli) => cli,
        // Help wins wherever it is, and it is the same help.
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            return out.write_all(USAGE.as_bytes()).map_err(Error::Output);
        }
        Err(error) if error.kind() == ErrorKind::DisplayVersion => {
            return writeln!(
                out,
                "{} {}",
                env!("CARGO_PKG_NAME"),
                env!("RITORNEL_VERSION_DISPLAY")
            )
            .map_err(Error::Output);
        }
        Err(error) => return Err(usage(&error)),
    };
    let on = cli.on.as_deref();
    match cli.command {
        Command::Play(args) => playback::play(&args, on, env, out),
        Command::Pause(args) => playback::transport("pause", &args, on, env, out),
        Command::Resume(args) => playback::transport("resume", &args, on, env, out),
        Command::Stop(args) => playback::transport("stop", &args, on, env, out),
        Command::Next(args) => playback::skip(SkipDirection::Next, &args, on, env, out),
        Command::Previous(args) => playback::skip(SkipDirection::Previous, &args, on, env, out),
        Command::Seek(args) => playback::seek(&args, on, env, out),
        Command::Volume(args) => playback::volume(&args, on, env, out),
        Command::Now(args) => playback::now(&args, on, env, out),
        Command::Queue(args) => playback::queue(&args, on, env, out),
        _ if on.is_some() => Err(Error::Usage("--on needs a playback or queue command")),
        Command::Status(args) => status::status(&args, env, out),
        Command::Server(ServerCommand::Stop(args)) => crate::server::stop(&args, env, out),
        Command::Transcode(command) => crate::transcode::run(command, env, out),
        Command::Devices(command) => crate::devices::run(command, env, out),
        Command::Backup(args) => backup::backup(&args, env, out),
        Command::Export(args) => export::export(&args, env, out),
        Command::Library(LibraryCommand::Create(args)) => library::create(&args, env, out),
        Command::Library(LibraryCommand::Stats(args)) => library::stats(&args, env, out),
        Command::Scan(args) => scan::scan(&args, env, out),
        Command::Artists(ArtistsCommand::Pictures(args)) => artists::pictures(&args, env, out),
        Command::Jobs(args) => jobs::jobs(&args, env, out),
        Command::Identity(IdentityCommand::ReResolve(args)) => {
            identity::re_resolve(&args, env, out)
        }
        Command::Identity(IdentityCommand::Review(args)) => identity::review(&args, env, out),
        Command::Corrections(CorrectionsCommand::Discs(args)) => {
            corrections::discs(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Musicbrainz(args)) => {
            corrections::musicbrainz(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Lookup(args)) => {
            corrections::lookup(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::List(args)) => corrections::list(&args, env, out),
        Command::Corrections(CorrectionsCommand::Show(args)) => corrections::show(&args, env, out),
        Command::Corrections(CorrectionsCommand::Preview(args)) => {
            corrections::preview(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Confirm(args)) => {
            corrections::confirm(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Undo(args)) => corrections::undo(&args, env, out),
        Command::Corrections(CorrectionsCommand::Decline(args)) => {
            corrections::decline(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Stop(args)) => corrections::stop(&args, env, out),
        Command::Corrections(CorrectionsCommand::Apart(args)) => {
            corrections::apart(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Answer(args)) => {
            corrections::answer(&args, env, out)
        }
        Command::Corrections(CorrectionsCommand::Answers(args)) => {
            corrections::answers(&args, env, out)
        }
        Command::History(args) => history::history(&args, env, out),
        Command::Show(args) => lastfm::show(&args, env, out),
        Command::Lastfm(command) => lastfm::lastfm(&command, env, out, err),
        Command::Link(command) => link::link(&command, env, out),
        Command::Settings(args) => settings::settings(&args, env, out),
        Command::Spotify(SpotifyCommand::Read(args)) => spotify::read_command(&args, out),
        Command::Spotify(SpotifyCommand::Import(args)) => spotify::import(&args, env, out),
        Command::Spotify(SpotifyCommand::Apply(args)) => spotify::apply(&args, env, out),
        Command::Spotify(SpotifyCommand::Undo(args)) => spotify::undo(&args, env, out),
        Command::Spotify(SpotifyCommand::Runs(args)) => spotify::runs(&args, env, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &[&str]) -> Vec<OsString> {
        line.iter().map(OsString::from).collect()
    }

    /// Runs the program with an empty environment and returns its exit
    /// code, standard output and standard error.
    fn program(line: &[&str]) -> (u8, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args(line), &|_| None, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn a_command_is_parsed_with_its_options_in_any_order() {
        assert_eq!(
            parse(args(&["status"])).unwrap().command,
            Command::Status(StatusArgs::default())
        );
        let given = || ServerArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            json: true,
        };
        for line in [
            &["status", "--data-dir", "/srv/ritornel", "--json"][..],
            &["status", "--json", "--data-dir", "/srv/ritornel"],
            &["status", "--json", "--data-dir=/srv/ritornel"],
        ] {
            assert_eq!(
                parse(args(line)).unwrap().command,
                Command::Status(StatusArgs {
                    dismiss: false,
                    server: given()
                }),
                "{line:?}"
            );
        }
        assert_eq!(
            parse(args(&["library", "create"])).unwrap().command,
            Command::Library(LibraryCommand::Create(ServerArgs::default()))
        );
        assert_eq!(
            parse(args(&[
                "library",
                "create",
                "--json",
                "--data-dir",
                "/srv/ritornel"
            ]))
            .unwrap()
            .command,
            Command::Library(LibraryCommand::Create(given()))
        );
    }

    #[test]
    fn scan_and_jobs_take_what_they_work_on_and_their_own_options() {
        assert_eq!(
            parse(args(&[
                "scan",
                "/music",
                "--no-wait",
                "--name",
                "main",
                "--json"
            ]))
            .unwrap()
            .command,
            Command::Scan(ScanArgs {
                folder: PathBuf::from("/music"),
                name: Some("main".to_owned()),
                no_wait: true,
                server: ServerArgs {
                    data_dir: None,
                    json: true
                },
            })
        );
        assert_eq!(
            parse(args(&[
                "artists",
                "pictures",
                "/pictures",
                "--name",
                "faces"
            ]))
            .unwrap()
            .command,
            Command::Artists(ArtistsCommand::Pictures(PicturesArgs {
                folder: PathBuf::from("/pictures"),
                name: Some("faces".to_owned()),
                server: ServerArgs::default(),
            }))
        );
        assert_eq!(
            parse(args(&["jobs"])).unwrap().command,
            Command::Jobs(JobsArgs::default())
        );
        assert_eq!(
            parse(args(&["jobs", "abc", "--cancel"])).unwrap().command,
            Command::Jobs(JobsArgs {
                job: Some("abc".to_owned()),
                cancel: true,
                server: ServerArgs::default(),
            })
        );
        assert_eq!(
            parse(args(&["library", "stats", "--json"]))
                .unwrap()
                .command,
            Command::Library(LibraryCommand::Stats(ServerArgs {
                data_dir: None,
                json: true
            }))
        );
        // What is wrong with these is known before any server is looked
        // for.
        for (line, message) in [
            (
                &["scan"][..],
                "the command line is wrong; try `ritornelctl --help`",
            ),
            (
                &["scan", "a", "b"],
                "unknown argument; try `ritornelctl --help`",
            ),
            (&["jobs", "--cancel"], "--cancel needs a job's ID"),
            (&["jobs", "not-an-id"], "that is not a job's ID"),
        ] {
            let (code, out, err) = program(line);
            assert_eq!((code, out.as_str()), (EXIT_USAGE, ""), "{line:?}");
            assert_eq!(err, format!("ritornelctl: {message}\n"), "{line:?}");
        }
        let (code, _, err) = program(&["scan", "/there/is/no/such/folder"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(
            err.starts_with("ritornelctl: cannot scan that folder: "),
            "{err}"
        );
    }

    #[test]
    fn corrections_confirm_takes_a_candidate_and_the_fingerprint_shown() {
        assert_eq!(
            parse(args(&[
                "corrections",
                "confirm",
                "proposal",
                "--candidate",
                "group",
                "--fingerprint",
                "shown",
            ]))
            .unwrap()
            .command,
            Command::Corrections(CorrectionsCommand::Confirm(ConfirmArgs {
                proposals: vec!["proposal".to_owned()],
                candidate: Some("group".to_owned()),
                release: None,
                fingerprint: Some("shown".to_owned()),
                server: ServerArgs::default(),
            }))
        );
    }

    #[test]
    fn corrections_answers_pages_and_answer_opens_one_and_preview_takes_an_undo() {
        assert_eq!(
            parse(args(&[
                "corrections",
                "answers",
                "--before",
                "earlier",
                "--limit",
                "10",
            ]))
            .unwrap()
            .command,
            Command::Corrections(CorrectionsCommand::Answers(AnswersArgs {
                before: Some("earlier".to_owned()),
                limit: Some(10),
                server: ServerArgs::default(),
            }))
        );
        assert_eq!(
            parse(args(&["corrections", "answer", "one"]))
                .unwrap()
                .command,
            Command::Corrections(CorrectionsCommand::Answer(AnswerArgs {
                answer: "one".to_owned(),
                server: ServerArgs::default(),
            }))
        );
        assert_eq!(
            parse(args(&["corrections", "preview", "--undo", "one"]))
                .unwrap()
                .command,
            Command::Corrections(CorrectionsCommand::Preview(PreviewArgs {
                proposals: vec!["one".to_owned()],
                candidate: None,
                release: None,
                undo: true,
                server: ServerArgs::default(),
            }))
        );
    }

    #[test]
    fn corrections_says_no_three_ways() {
        assert_eq!(
            parse(args(&[
                "corrections",
                "decline",
                "one",
                "--candidate",
                "group"
            ]))
            .unwrap()
            .command,
            Command::Corrections(CorrectionsCommand::Decline(DeclineArgs {
                proposal: "one".to_owned(),
                candidate: Some("group".to_owned()),
                release: None,
                server: ServerArgs::default(),
            }))
        );
        assert_eq!(
            parse(args(&["corrections", "stop", "one"]))
                .unwrap()
                .command,
            Command::Corrections(CorrectionsCommand::Stop(ProposalArgs {
                proposal: "one".to_owned(),
                server: ServerArgs::default(),
            }))
        );
        assert_eq!(
            parse(args(&["corrections", "apart", "a", "b"]))
                .unwrap()
                .command,
            Command::Corrections(CorrectionsCommand::Apart(ApartArgs {
                releases: vec!["a".to_owned(), "b".to_owned()],
                server: ServerArgs::default(),
            }))
        );
        assert!(parse(args(&["corrections", "apart", "a"])).is_err());
    }

    #[test]
    fn identity_takes_its_dry_run_and_the_values_to_ask_about() {
        assert_eq!(
            parse(args(&[
                "identity",
                "re-resolve",
                "--dry-run",
                "--fold-accents",
                "--duration-grace-ms",
                "3000",
                "--detail",
                "--json"
            ]))
            .unwrap()
            .command,
            Command::Identity(IdentityCommand::ReResolve(Box::new(ReResolveArgs {
                dry_run: true,
                detail: true,
                duration_grace_ms: Some("3000".to_owned()),
                fold_accents: Some("on".to_owned()),
                server: ServerArgs {
                    data_dir: None,
                    json: true
                },
                ..ReResolveArgs::default()
            })))
        );
        assert_eq!(
            parse(args(&[
                "identity",
                "re-resolve",
                "--dry-run",
                "--policy",
                "1",
                "--tags-across-albums",
                "off",
                "--disc-from-album-title",
            ]))
            .unwrap()
            .command,
            Command::Identity(IdentityCommand::ReResolve(Box::new(ReResolveArgs {
                dry_run: true,
                policy: Some("1".to_owned()),
                tags_across_albums: Some("off".to_owned()),
                disc_from_album_title: Some("on".to_owned()),
                ..ReResolveArgs::default()
            })))
        );
        assert_eq!(
            parse(args(&["identity", "review"])).unwrap().command,
            Command::Identity(IdentityCommand::Review(ServerArgs::default()))
        );
        // Without --dry-run nothing is asked of a server.
        let (code, out, err) = program(&["identity", "re-resolve"]);
        assert_eq!((code, out.as_str()), (EXIT_USAGE, ""));
        assert_eq!(
            err,
            "ritornelctl: re-resolve only runs with --dry-run: a scan resolves the library at its end\n"
        );
    }

    #[test]
    fn an_option_before_a_history_command_is_refused_and_never_dropped() {
        // `history` takes options of its own for the recent plays. Placed
        // before one of its commands they were once read and then
        // ignored, so `zone clear` acted on the default data directory.
        for line in [
            &["history", "--data-dir", "/srv/ritornel", "zone", "clear"][..],
            &["history", "--data-dir", "/srv/ritornel", "rebuild"],
            &[
                "history",
                "--data-dir",
                "/srv/ritornel",
                "zone",
                "add",
                "UTC",
            ],
            &["history", "--json", "zones"],
            &["history", "--limit", "5", "on-this-day"],
        ] {
            assert!(parse(args(line)).is_err(), "{line:?}");
            let (code, out, _) = program(line);
            assert_eq!((code, out.as_str()), (EXIT_USAGE, ""), "{line:?}");
        }
        // After the command, or with no command, they are read as before.
        let given = ServerArgs {
            data_dir: Some(PathBuf::from("/srv/ritornel")),
            json: false,
        };
        assert_eq!(
            parse(args(&[
                "history",
                "zone",
                "clear",
                "--data-dir",
                "/srv/ritornel"
            ]))
            .unwrap()
            .command,
            Command::History(HistoryArgs {
                command: Some(HistoryCommand::Zone(ZoneCommand::Clear(ServerArgs {
                    data_dir: Some(PathBuf::from("/srv/ritornel")),
                    json: false,
                }))),
                limit: None,
                server: ServerArgs::default(),
            })
        );
        assert_eq!(
            parse(args(&[
                "history",
                "--data-dir",
                "/srv/ritornel",
                "--limit",
                "5"
            ]))
            .unwrap()
            .command,
            Command::History(HistoryArgs {
                command: None,
                limit: Some(5),
                server: given,
            })
        );
    }

    #[test]
    fn settings_takes_its_options_after_its_command() {
        let line = ["settings", "--data-dir", "/srv/ritornel", "awake", "never"];
        assert!(parse(args(&line)).is_err());
        assert_eq!(
            parse(args(&[
                "settings",
                "awake",
                "while-active",
                "45",
                "--data-dir",
                "/srv/ritornel"
            ]))
            .unwrap()
            .command,
            Command::Settings(SettingsArgs {
                command: Some(SettingsCommand::Awake(AwakeArgs {
                    mode: "while-active".to_owned(),
                    minutes: Some("45".to_owned()),
                    server: ServerArgs {
                        data_dir: Some(PathBuf::from("/srv/ritornel")),
                        json: false,
                    },
                })),
                server: ServerArgs::default(),
            })
        );
        assert_eq!(
            parse(args(&["settings", "--json"])).unwrap().command,
            Command::Settings(SettingsArgs {
                command: None,
                server: ServerArgs {
                    data_dir: None,
                    json: true,
                },
            })
        );
    }

    #[test]
    fn status_takes_dismiss_before_or_after_its_other_options() {
        for line in [
            &["status", "--dismiss", "--json"][..],
            &["status", "--json", "--dismiss"],
        ] {
            assert_eq!(
                parse(args(line)).unwrap().command,
                Command::Status(StatusArgs {
                    dismiss: true,
                    server: ServerArgs {
                        data_dir: None,
                        json: true
                    },
                }),
                "{line:?}"
            );
        }
    }

    #[test]
    fn play_start_counts_from_one() {
        assert!(parse(args(&["play", "--start", "0", "track"])).is_err());
    }

    #[test]
    fn a_wrong_command_line_is_refused_with_the_usage_exit_code() {
        for line in [
            &[][..],
            &["--json"],
            &["--data-dir", "/srv/ritornel", "status"],
            &["status", "status"],
            &["status", "--data-dir"],
            &["status", "--data-dir", ""],
            &["status", "--data-dir", "--json"],
            &["status", "--data-dir", "-x"],
            &["status", "--json", "--json"],
            &["status", "--yaml"],
            &["serve"],
            &["library"],
            &["library", "--json"],
            &["library", "destroy"],
            &["library", "create", "now"],
            &["library", "create", "--data-dir"],
            &["help"],
        ] {
            let (code, out, err) = program(line);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert_eq!(out, "", "{line:?}");
            assert!(err.starts_with("ritornelctl: "), "{line:?}: {err}");
            assert_eq!(err.lines().count(), 1, "{line:?}: {err}");
        }
    }

    #[test]
    fn each_way_of_being_wrong_has_its_own_fixed_message() {
        for (line, message) in [
            (&["serve"][..], "unknown argument; try `ritornelctl --help`"),
            (
                &["status", "--yaml"],
                "unknown argument; try `ritornelctl --help`",
            ),
            (&[], "expected a command; try `ritornelctl --help`"),
            (&["library"], "expected a command; try `ritornelctl --help`"),
            (&["status", "--data-dir"], "--data-dir needs a directory"),
            (
                &["status", "--data-dir", ""],
                "--data-dir needs a directory",
            ),
            (
                &["library", "create", "--data-dir", "--json"],
                "--data-dir needs a directory",
            ),
            (
                &["status", "--json", "--json"],
                "the command line is wrong; try `ritornelctl --help`",
            ),
        ] {
            let (_, _, err) = program(line);
            assert_eq!(err, format!("ritornelctl: {message}\n"), "{line:?}");
        }
    }

    #[test]
    fn an_error_message_repeats_no_argument() {
        for line in [
            &["status", "--tokne", "SENTINEL_ARGUMENT_0001"][..],
            &["status", "SENTINEL_ARGUMENT_0001"],
            &["SENTINEL_ARGUMENT_0001"],
            &["status", "--SENTINEL_ARGUMENT_0001"],
            &["status", "--json=SENTINEL_ARGUMENT_0001"],
            &["library", "create", "--data-dir", "-SENTINEL_ARGUMENT_0001"],
        ] {
            let (code, _, err) = program(line);
            assert_eq!(code, EXIT_USAGE, "{line:?}");
            assert!(!err.contains("SENTINEL"), "{err}");
        }
    }

    #[test]
    fn spotify_read_takes_an_archive_and_json_and_no_data_directory() {
        assert_eq!(
            parse(args(&["spotify", "read", "/archive.zip", "--json"]))
                .unwrap()
                .command,
            Command::Spotify(SpotifyCommand::Read(SpotifyReadArgs {
                archive: PathBuf::from("/archive.zip"),
                json: true,
            }))
        );
        for line in [
            &["spotify", "read"][..],
            &[
                "spotify",
                "read",
                "/archive.zip",
                "--data-dir",
                "/srv/ritornel",
            ],
            &["spotify"],
        ] {
            let (code, out, _) = program(line);
            assert_eq!((code, out.as_str()), (EXIT_USAGE, ""), "{line:?}");
        }
        // It needs no server: a folder with no history says so.
        let (code, _, err) = program(&["spotify", "read", "/there/is/no/such/archive"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(
            err.starts_with("ritornelctl: cannot read that archive: "),
            "{err}"
        );
    }

    #[test]
    fn help_and_version_print_and_succeed() {
        let (code, out, err) = program(&["--help"]);
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("ritornelctl status"), "{out}");
        assert!(out.contains("ritornelctl library create"), "{out}");
        assert!(out.contains("4    No server is running"), "{out}");
        // Help wins wherever it is.
        assert_eq!(program(&["status", "--json", "-h"]).1, out);
        assert_eq!(program(&["library", "--help"]).1, out);
        assert_eq!(program(&["library", "create", "-h"]).1, out);

        for line in [&["--version"][..], &["-V"], &["status", "-V"]] {
            let (code, out, _) = program(line);
            assert_eq!(code, 0, "{line:?}");
            assert_eq!(
                out,
                concat!("ritornelctl ", env!("RITORNEL_VERSION_DISPLAY"), "\n")
            );
        }
    }

    #[test]
    fn with_no_data_directory_to_use_it_says_how_to_name_one() {
        for line in [&["status"][..], &["library", "create"]] {
            let (code, out, err) = program(line);
            assert_eq!(code, EXIT_FAILED, "{line:?}");
            assert_eq!(out, "");
            assert_eq!(
                err,
                "ritornelctl: no data directory: give --data-dir or set RITORNEL_DATA_DIR\n"
            );
        }
    }

    #[test]
    fn with_no_server_it_says_so_and_exits_with_the_code_kept_for_that() {
        // A folder that is not there, inside one that is this test's own.
        let scratch = tempfile::Builder::new()
            .prefix("ritornelctl-unit-")
            .tempdir()
            .unwrap();
        let dir = scratch.path().join("absent");
        for line in [&["status"][..], &["library", "create"]] {
            let mut line = line.to_vec();
            line.extend(["--data-dir", dir.to_str().unwrap()]);
            let (code, out, err) = program(&line);
            assert_eq!(code, EXIT_NO_SERVER, "{line:?}");
            assert_eq!(out, "");
            assert_eq!(
                err,
                format!("ritornelctl: no server is running on {}\n", dir.display())
            );
        }
        // Looking made nothing.
        assert!(!dir.exists());
    }
}
