//! The command line.
//!
//! ```text
//! ritornelctl status [--dismiss] [--data-dir <dir>] [--json]
//! ritornelctl backup [--data-dir <dir>] [--json]
//! ritornelctl export <folder> [--data-dir <dir>] [--json]
//! ritornelctl library create [--data-dir <dir>] [--json]
//! ritornelctl library stats [--data-dir <dir>] [--json]
//! ritornelctl scan <folder> [--name <name>] [--no-wait] [--data-dir <dir>] [--json]
//! ritornelctl artists pictures <folder> [--name <name>] [--data-dir <dir>] [--json]
//! ritornelctl jobs [<job>] [--cancel] [--data-dir <dir>] [--json]
//! ritornelctl identity re-resolve --dry-run [--detail] [<policy values>] [--data-dir <dir>] [--json]
//! ritornelctl identity review [--data-dir <dir>] [--json]
//! ritornelctl play <folder | track>... [--album <release>] [--start <n>] [--follow]
//! ritornelctl pause | resume | stop | next | previous | now | queue
//! ritornelctl seek <position>
//! ritornelctl volume [<level> | mute | unmute]
//! ritornelctl history [--limit <n>]
//! ritornelctl history artist | album <name or ID>
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
//! ```
//!
//! Every command takes `--data-dir <dir>` and `--json`.
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
    artists, backup, export, history, identity, jobs, lastfm, library, playback, scan, status,
};

const USAGE: &str = "\
The command-line client of a Ritornel server. It talks to the server that is
running on a data directory, and never starts one.

Usage:
  ritornelctl status [--dismiss] [--data-dir <dir>] [--json]
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
  ritornelctl play <folder | track>... [--album <release>] [--start <n>]
                   [--follow] [--data-dir <dir>] [--json]
  ritornelctl pause | resume | stop | next | previous [--data-dir <dir>] [--json]
  ritornelctl seek <position> [--data-dir <dir>] [--json]
  ritornelctl volume [<level> | mute | unmute] [--data-dir <dir>] [--json]
  ritornelctl now | queue [--data-dir <dir>] [--json]
  ritornelctl history [--limit <n>] [--data-dir <dir>] [--json]
  ritornelctl history artist | album <name or ID> [--data-dir <dir>] [--json]
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

Commands:
  status           Show the running server's ID, name and role, the versions
                   it is built from, the library it holds, the free disk
                   space, where its logs are, its last backup, and whether
                   the listening record advances: each listen saved and
                   in history, each scrobble answered, each import done.
                   Scrobbles that ended unanswered or were not sent are
                   said until --dismiss sets them aside
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
  play             Play the scanned tracks in a folder, album by album in
                   disc and track order, or tracks by their IDs, or a
                   release with --album. It replaces the queue. The server
                   plays on when this program ends
  pause, resume    Pause, and play on. After the server restarts, resume
                   carries on where the last run stopped
  stop             Stop. Resume plays the track from its beginning
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

Options:
  --name <name>         scan, artists pictures: the name the folder is
                        added under. With none it is the folder's own name
  --no-wait             scan: start the scan and return at once
  --cancel              jobs: ask the job to stop. It stops after the files
                        it is reading, and what it has recorded stays
  --detail              re-resolve: list every file that is not unchanged

Policy values, for re-resolve --dry-run. A switch is on or off, and on
when given alone:
  --policy <n>          Start from policy 1 or 2 rather than the one scans
                        use
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
  --dry-run             history rebuild: say what would change, and change
                        nothing
  --from <YYYY-MM-DD>   history zone add: the first day
  --until <YYYY-MM-DD>  history zone add: the day after the last
  --full                lastfm sync: read the whole history again
  --no-wait             lastfm sync: start it and return at once
  --limit <n>           lastfm report: how many doubtful tracks. The
                        default is 50
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
    /// What `play` was given is no folder there is.
    PlayFolder(std::io::Error),
    /// The job that was waited for did not succeed. The text is its state.
    Job(&'static str),
    /// There was nothing to do it to.
    Nothing(&'static str),
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
            Self::PlayFolder(error) => write!(f, "cannot play that folder: {error}"),
            Self::Job(state) => write!(f, "the job {state}"),
            Self::Nothing(what) => f.write_str(what),
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
#[command(name = "ritornelctl", version, propagate_version = true)]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    Status(StatusArgs),
    Backup(ServerArgs),
    Export(ExportArgs),
    #[command(subcommand)]
    Library(LibraryCommand),
    Scan(ScanArgs),
    #[command(subcommand)]
    Artists(ArtistsCommand),
    Jobs(JobsArgs),
    #[command(subcommand)]
    Identity(IdentityCommand),
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
    /// The named policy to start from: 1 or 2. The one scans use when it
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
    let command = match parse(args) {
        Ok(cli) => cli.command,
        // Help wins wherever it is, and it is the same help.
        Err(error) if error.kind() == ErrorKind::DisplayHelp => {
            return out.write_all(USAGE.as_bytes()).map_err(Error::Output);
        }
        Err(error) if error.kind() == ErrorKind::DisplayVersion => {
            return writeln!(
                out,
                "{} {}",
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION")
            )
            .map_err(Error::Output);
        }
        Err(error) => return Err(usage(&error)),
    };
    match command {
        Command::Status(args) => status::status(&args, env, out),
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
        Command::Play(args) => playback::play(&args, env, out),
        Command::Pause(args) => playback::transport("pause", &args, env, out),
        Command::Resume(args) => playback::transport("resume", &args, env, out),
        Command::Stop(args) => playback::transport("stop", &args, env, out),
        Command::Next(args) => playback::skip(SkipDirection::Next, &args, env, out),
        Command::Previous(args) => playback::skip(SkipDirection::Previous, &args, env, out),
        Command::Seek(args) => playback::seek(&args, env, out),
        Command::Volume(args) => playback::volume(&args, env, out),
        Command::Now(args) => playback::now(&args, env, out),
        Command::Queue(args) => playback::queue(&args, env, out),
        Command::History(args) => history::history(&args, env, out),
        Command::Show(args) => lastfm::show(&args, env, out),
        Command::Lastfm(command) => lastfm::lastfm(&command, env, out, err),
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
                concat!("ritornelctl ", env!("CARGO_PKG_VERSION"), "\n")
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
        let dir = std::env::temp_dir().join(format!("ritornelctl-unit-{}", std::process::id()));
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
