//! Extractor 2: turns one stored payload into claims. See "Scanning one
//! file" step 6 and decision 3 in docs/design/identity.md.
//!
//! The tag names are the ones MusicBrainz Picard and beets write, taken
//! from Picard's tag mapping appendix and from beets' `mediafile`, for
//! Vorbis comments (FLAC and Ogg), ID3v2 (MP3, AIFF, WAV) and MP4 atoms. A
//! name is matched whatever its case. An ID3v2 `TXXX` frame is named
//! `TXXX:<description>`, a `UFID` frame `UFID:<owner>`, and an MP4 freeform
//! atom `<mean>:<name>`, which is how the reader names a `----` atom. The three-letter frames of ID3v2.2 are read
//! as the frames they became.
//!
//! One file can hold several blocks of tags that say the same thing
//! differently: a FLAC with Vorbis comments and an ID3v2 tag in front, an
//! MP3 with ID3v2 and an ID3v1 tag cut to thirty characters. A field is
//! read from one block, the best that has it: the container's own tags
//! before ID3v2 or APE, and ID3v1 last.

use crate::payload::{Payload, Tag};

/// The version of this extractor. Claims are rebuilt when it changes.
pub const EXTRACTOR_VERSION: i64 = 2;

/// The kind of entity a claim describes. It is never a pointer to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// A recording.
    Recording,
    /// One recording at one position on a release.
    TrackAppearance,
    /// A release.
    Release,
    /// A release group.
    ReleaseGroup,
    /// An artist.
    Artist,
}

impl Scope {
    /// The scope as the schema writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recording => "recording",
            Self::TrackAppearance => "track_appearance",
            Self::Release => "release",
            Self::ReleaseGroup => "release_group",
            Self::Artist => "artist",
        }
    }

    /// The scope the schema writes as `text`.
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Recording,
            Self::TrackAppearance,
            Self::Release,
            Self::ReleaseGroup,
            Self::Artist,
        ]
        .into_iter()
        .find(|scope| scope.as_str() == text)
    }
}

/// Where a claim came from, which is its rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    /// The file's name. The lowest rank.
    Inferred,
    /// The file's tags and its audio stream.
    FileTags,
}

impl Source {
    /// The `metadata_source.kind` of this source.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inferred => "inferred",
            Self::FileTags => "file_tags",
        }
    }

    /// The source `metadata_source.kind` names, of the two a scan makes.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "inferred" => Some(Self::Inferred),
            "file_tags" => Some(Self::FileTags),
            _ => None,
        }
    }
}

/// One thing a source item stated: an identifier or a tag value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Whether it goes in `identifier_claim`. Otherwise `metadata_claim`.
    pub identifier: bool,
    /// The kind of entity described.
    pub scope: Scope,
    /// The identifier's namespace, or the metadata field.
    pub name: String,
    /// The value.
    pub value: String,
    /// The position among the values of a multi-valued tag.
    pub ordinal: u32,
    /// Where it came from.
    pub source: Source,
}

/// The names each field is read from, across the three tag formats.
mod names {
    pub(super) const TITLE: &[&str] = &["TITLE", "TIT2", "TT2", "\u{a9}nam"];
    pub(super) const ARTIST: &[&str] = &["ARTIST", "TPE1", "TP1", "\u{a9}ART"];
    pub(super) const ARTISTS: &[&str] = &["ARTISTS", "TXXX:ARTISTS", "com.apple.iTunes:ARTISTS"];
    pub(super) const ARTIST_SORT: &[&str] = &["ARTISTSORT", "TSOP", "TSP", "soar"];
    pub(super) const ALBUM: &[&str] = &["ALBUM", "TALB", "TAL", "\u{a9}alb"];
    pub(super) const ALBUM_ARTIST: &[&str] = &[
        "ALBUMARTIST",
        "ALBUM ARTIST",
        "ALBUM_ARTIST",
        "TPE2",
        "TP2",
        "aART",
    ];
    pub(super) const ALBUM_ARTIST_SORT: &[&str] = &[
        "ALBUMARTISTSORT",
        "TSO2",
        "TS2",
        "TXXX:ALBUMARTISTSORT",
        "soaa",
    ];
    pub(super) const TRACK: &[&str] = &["TRACKNUMBER", "TRACK", "TRCK", "TRK", "trkn"];
    pub(super) const TRACK_TOTAL: &[&str] = &["TRACKTOTAL", "TOTALTRACKS", "TRACKC"];
    pub(super) const DISC: &[&str] = &["DISCNUMBER", "DISC", "TPOS", "TPA", "disk"];
    pub(super) const DISC_TOTAL: &[&str] = &["DISCTOTAL", "TOTALDISCS", "DISCC"];
    pub(super) const DATE: &[&str] = &["DATE", "YEAR", "TDRC", "TYER", "TYE", "\u{a9}day"];
    pub(super) const ORIGINAL_DATE: &[&str] = &[
        "ORIGINALDATE",
        "TDOR",
        "com.apple.iTunes:ORIGINALDATE",
        "TXXX:ORIGINALDATE",
        "ORIGINALYEAR",
        "TORY",
        "TOR",
        "com.apple.iTunes:ORIGINAL YEAR",
        "com.apple.iTunes:ORIGINALYEAR",
        "TXXX:ORIGINALYEAR",
    ];
    pub(super) const COMPILATION: &[&str] = &["COMPILATION", "TCMP", "TCP", "cpil"];
    pub(super) const RELEASE_TYPE: &[&str] = &[
        "RELEASETYPE",
        "MUSICBRAINZ_ALBUMTYPE",
        "TXXX:MusicBrainz Album Type",
        "com.apple.iTunes:MusicBrainz Album Type",
    ];
    pub(super) const MB_RECORDING: &[&str] = &[
        "MUSICBRAINZ_TRACKID",
        "UFID:http://musicbrainz.org",
        "com.apple.iTunes:MusicBrainz Track Id",
    ];
    pub(super) const MB_TRACK: &[&str] = &[
        "MUSICBRAINZ_RELEASETRACKID",
        "TXXX:MusicBrainz Release Track Id",
        "com.apple.iTunes:MusicBrainz Release Track Id",
    ];
    pub(super) const MB_RELEASE: &[&str] = &[
        "MUSICBRAINZ_ALBUMID",
        "TXXX:MusicBrainz Album Id",
        "com.apple.iTunes:MusicBrainz Album Id",
    ];
    pub(super) const MB_RELEASE_GROUP: &[&str] = &[
        "MUSICBRAINZ_RELEASEGROUPID",
        "TXXX:MusicBrainz Release Group Id",
        "com.apple.iTunes:MusicBrainz Release Group Id",
    ];
    pub(super) const MB_ARTIST: &[&str] = &[
        "MUSICBRAINZ_ARTISTID",
        "TXXX:MusicBrainz Artist Id",
        "com.apple.iTunes:MusicBrainz Artist Id",
    ];
    pub(super) const MB_ALBUM_ARTIST: &[&str] = &[
        "MUSICBRAINZ_ALBUMARTISTID",
        "TXXX:MusicBrainz Album Artist Id",
        "com.apple.iTunes:MusicBrainz Album Artist Id",
    ];
    pub(super) const ISRC: &[&str] = &["ISRC", "TSRC", "TRC", "com.apple.iTunes:ISRC"];
    pub(super) const ACOUSTID: &[&str] = &[
        "ACOUSTID_ID",
        "TXXX:Acoustid Id",
        "com.apple.iTunes:Acoustid Id",
    ];
}

/// MusicBrainz's primary types, as a release-type tag writes them.
pub const PRIMARY_TYPES: [&str; 5] = ["album", "single", "ep", "broadcast", "other"];

/// MusicBrainz's secondary types.
pub const SECONDARY_TYPES: [&str; 12] = [
    "compilation",
    "live",
    "soundtrack",
    "remix",
    "dj-mix",
    "mixtape/street",
    "demo",
    "spokenword",
    "interview",
    "audiobook",
    "audio drama",
    "field recording",
];

/// A tag's name as the tables above write it. The user-defined text frame
/// and the unique file identifier of ID3v2.2 take the names they have in
/// ID3v2.3 and later.
fn name_of(tag: &Tag) -> String {
    let key = match tag.key.as_str() {
        "TXX" => "TXXX",
        "UFI" => "UFID",
        // A freeform atom given as `----` with its mean and name beside it
        // is named as the reader names it, by the mean and name alone.
        "----" => {
            if let Some(sub) = &tag.sub {
                return sub.clone();
            }
            "----"
        }
        key => key,
    };
    match &tag.sub {
        Some(sub) => format!("{key}:{sub}"),
        None => key.to_owned(),
    }
}

/// How far a block of tags is trusted for a field another block also has:
/// the container's own tags first, ID3v2 or APE beside them next, and
/// ID3v1, which cuts every value to thirty characters, last.
fn rank(tag: &Tag) -> u8 {
    match tag.block.as_deref() {
        Some("id3v1") => 0,
        Some(block) if block.starts_with("id3v2") || block.starts_with("ape") => 1,
        _ => 2,
    }
}

/// Every value of the tags with one of these names, in the file's order,
/// from the one block of tags that is trusted most among those that have
/// any. A value that is there twice is there once.
fn values<'a>(payload: &'a Payload, names: &[&str]) -> Vec<&'a str> {
    let named: Vec<&Tag> = payload
        .tags
        .iter()
        .filter(|tag| tag.binary_len.is_none() || matches!(tag.key.as_str(), "UFID" | "UFI"))
        .filter(|tag| {
            let name = name_of(tag);
            names
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(&name))
        })
        .filter(|tag| tag.values.iter().any(|value| !value.trim().is_empty()))
        .collect();
    let Some(best) = named
        .iter()
        .copied()
        .max_by_key(|tag| (rank(tag), std::cmp::Reverse(position(payload, tag))))
    else {
        return Vec::new();
    };
    let mut found: Vec<&str> = Vec::new();
    for tag in named.iter().filter(|tag| tag.block == best.block) {
        for value in tag.values.iter().map(|value| value.trim()) {
            if !value.is_empty() && !found.contains(&value) {
                found.push(value);
            }
        }
    }
    found
}

/// Where a tag is in the file's order.
fn position(payload: &Payload, tag: &Tag) -> usize {
    payload
        .tags
        .iter()
        .position(|other| std::ptr::eq(other, tag))
        .unwrap_or(usize::MAX)
}

/// The whole number a value begins with, such as the 3 of `3/12`.
fn leading_number(value: &str) -> Option<u32> {
    let digits: String = value
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The number after the slash of `3/12`.
fn after_slash(value: &str) -> Option<u32> {
    leading_number(value.split_once('/')?.1)
}

/// The MusicBrainz IDs in a value: one, or several joined as ID3v2.3 joins
/// them. Anything that is not shaped like a UUID is left out.
fn mbids(value: &str) -> Vec<String> {
    value
        .split(|c: char| c == '/' || c == ';' || c == ',' || c.is_whitespace())
        .filter(|part| {
            part.len() == 36
                && part.chars().enumerate().all(|(index, c)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        c == '-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                })
        })
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The bytes of a `UFID` frame, which the payload holds as hex, as text.
fn hex_text(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) || !value.is_ascii() {
        return None;
    }
    let digits: Vec<u32> = value
        .chars()
        .map(|c| c.to_digit(16))
        .collect::<Option<_>>()?;
    let bytes: Vec<u8> = digits
        .chunks(2)
        .filter_map(|pair| match pair {
            [high, low] => u8::try_from(high * 16 + low).ok(),
            _ => None,
        })
        .collect();
    String::from_utf8(bytes).ok()
}

/// A date as a release-date tag holds one: `YYYY`, `YYYY-MM` or
/// `YYYY-MM-DD`, with its year. Anything after the date is ignored, and a
/// value that does not begin with a year is no date.
pub fn parse_date(value: &str) -> Option<(String, i64)> {
    let value = value.trim();
    let year = value.get(..4)?;
    if !year.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number: i64 = year.parse().ok()?;
    if !(1000..3000).contains(&number) {
        return None;
    }
    let part = |from: usize, most: u32| -> Option<&str> {
        let text = value.get(from..from + 3)?;
        let digits = text.strip_prefix('-')?;
        let n: u32 = digits.parse().ok()?;
        (digits.chars().all(|c| c.is_ascii_digit()) && (1..=most).contains(&n)).then_some(text)
    };
    let mut date = year.to_owned();
    if let Some(month) = part(4, 12) {
        date.push_str(month);
        if let Some(day) = part(7, 31) {
            date.push_str(day);
        }
    }
    Some((date, number))
}

/// The release types in the values of a release-type tag, in lower case.
/// Picard joins several with `/` in ID3v2.3, and others with `;` or `,`.
fn release_types(values: &[&str]) -> Vec<String> {
    let mut types = Vec::new();
    for value in values {
        let lower = value.to_lowercase();
        // The one type with a slash in its own name.
        let lower = lower.replace("mixtape/street", "mixtape\u{1}street");
        for part in lower.split(['/', ';', ',']) {
            let part = part.trim().replace('\u{1}', "/");
            if !part.is_empty() && !types.contains(&part) {
                types.push(part);
            }
        }
    }
    types
}

/// What the file's own name says: a title, and a track number if the name
/// begins with one.
fn from_file_name(path: &str) -> (Option<u32>, Option<String>) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
    if stem.is_empty() {
        return (None, None);
    }
    let digits = stem.chars().take_while(char::is_ascii_digit).count();
    let rest = stem
        .get(digits..)
        .unwrap_or("")
        .trim_start_matches([' ', '-', '.', '_']);
    let separated = stem
        .get(digits..)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| matches!(c, ' ' | '-' | '.' | '_'));
    if (1..=3).contains(&digits) && separated && !rest.is_empty() {
        let track = stem.get(..digits).and_then(|digits| digits.parse().ok());
        (track, Some(rest.to_owned()))
    } else {
        (None, Some(stem.to_owned()))
    }
}

/// Extracts every claim one payload makes.
pub fn extract(payload: &Payload) -> Vec<Claim> {
    let mut claims = Vec::new();
    let mut metadata = |scope, name: &str, values: Vec<String>, source| {
        for (ordinal, value) in values.into_iter().enumerate() {
            claims.push(Claim {
                identifier: false,
                scope,
                name: name.to_owned(),
                value,
                ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                source,
            });
        }
    };
    let owned = |values: Vec<&str>| values.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let first = |values: Vec<&str>| {
        values
            .into_iter()
            .take(1)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let number = |n: Option<u32>| n.map(|n| n.to_string()).into_iter().collect::<Vec<_>>();
    let tags = Source::FileTags;
    use Scope::{Release, ReleaseGroup, TrackAppearance};

    metadata(
        TrackAppearance,
        "title",
        first(values(payload, names::TITLE)),
        tags,
    );
    metadata(
        TrackAppearance,
        "artist",
        owned(values(payload, names::ARTIST)),
        tags,
    );
    metadata(
        TrackAppearance,
        "artists",
        owned(values(payload, names::ARTISTS)),
        tags,
    );
    metadata(
        TrackAppearance,
        "artist_sort",
        first(values(payload, names::ARTIST_SORT)),
        tags,
    );

    // An MP4 `trkn` or `disk` atom is read as two tags, the number and
    // then the total. The others hold `3` or `3/12`.
    let track = values(payload, names::TRACK);
    let disc = values(payload, names::DISC);
    let total = |position: &[&str], totals: &[&str]| {
        position
            .first()
            .and_then(|value| after_slash(value))
            .or_else(|| totals.first().and_then(|value| leading_number(value)))
            .or_else(|| position.get(1).and_then(|value| leading_number(value)))
    };
    let track_total = total(&track, &values(payload, names::TRACK_TOTAL));
    let disc_total = total(&disc, &values(payload, names::DISC_TOTAL));
    let position = |values: &[&str]| {
        values
            .first()
            .and_then(|v| leading_number(v))
            .filter(|n| *n > 0)
    };
    metadata(
        TrackAppearance,
        "track_number",
        number(position(&track)),
        tags,
    );
    metadata(TrackAppearance, "track_total", number(track_total), tags);
    metadata(
        TrackAppearance,
        "disc_number",
        number(position(&disc)),
        tags,
    );
    metadata(TrackAppearance, "disc_total", number(disc_total), tags);
    if let Some(audio) = &payload.audio {
        metadata(
            TrackAppearance,
            "duration_ms",
            vec![audio.duration_ms.to_string()],
            tags,
        );
    }

    metadata(Release, "album", first(values(payload, names::ALBUM)), tags);
    metadata(
        Release,
        "album_artist",
        owned(values(payload, names::ALBUM_ARTIST)),
        tags,
    );
    metadata(
        Release,
        "album_artist_sort",
        first(values(payload, names::ALBUM_ARTIST_SORT)),
        tags,
    );
    metadata(Release, "date", first(values(payload, names::DATE)), tags);
    let compilation = values(payload, names::COMPILATION)
        .first()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    if compilation {
        metadata(Release, "compilation", vec!["1".to_owned()], tags);
    }

    let types = release_types(&values(payload, names::RELEASE_TYPE));
    let primary = types
        .iter()
        .find(|kind| PRIMARY_TYPES.contains(&kind.as_str()));
    metadata(
        ReleaseGroup,
        "primary_type",
        primary.cloned().into_iter().collect(),
        tags,
    );
    let mut secondary: Vec<String> = types
        .iter()
        .filter(|kind| SECONDARY_TYPES.contains(&kind.as_str()))
        .cloned()
        .collect();
    secondary.sort();
    metadata(ReleaseGroup, "secondary_type", secondary, tags);
    metadata(
        ReleaseGroup,
        "original_date",
        first(values(payload, names::ORIGINAL_DATE)),
        tags,
    );

    let (track_inferred, title_inferred) = from_file_name(&payload.path);
    metadata(
        TrackAppearance,
        "title",
        title_inferred.into_iter().collect(),
        Source::Inferred,
    );
    metadata(
        TrackAppearance,
        "track_number",
        number(track_inferred),
        Source::Inferred,
    );

    let mut identifier = |scope, namespace: &str, values: Vec<String>| {
        for (ordinal, value) in values.into_iter().enumerate() {
            claims.push(Claim {
                identifier: true,
                scope,
                name: namespace.to_owned(),
                value,
                ordinal: u32::try_from(ordinal).unwrap_or(u32::MAX),
                source: Source::FileTags,
            });
        }
    };
    let ids = |names: &[&str]| -> Vec<String> {
        let mut found = Vec::new();
        for value in values(payload, names) {
            let text = hex_text(value).filter(|text| !mbids(text).is_empty());
            for id in mbids(text.as_deref().unwrap_or(value)) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        found
    };
    let one = |mut ids: Vec<String>| {
        ids.truncate(1);
        ids
    };
    identifier(
        Scope::Recording,
        "mb_recording",
        one(ids(names::MB_RECORDING)),
    );
    identifier(TrackAppearance, "mb_track", one(ids(names::MB_TRACK)));
    identifier(Release, "mb_release", one(ids(names::MB_RELEASE)));
    identifier(
        ReleaseGroup,
        "mb_release_group",
        one(ids(names::MB_RELEASE_GROUP)),
    );
    identifier(Scope::Artist, "mb_artist", ids(names::MB_ARTIST));
    identifier(
        Scope::Artist,
        "mb_album_artist",
        ids(names::MB_ALBUM_ARTIST),
    );
    let plain = |names: &[&str]| -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for value in values(payload, names) {
            let value = value.to_ascii_uppercase();
            if !found.contains(&value) {
                found.push(value);
            }
        }
        found
    };
    identifier(Scope::Recording, "isrc", plain(names::ISRC));
    identifier(
        Scope::Recording,
        "acoustid",
        plain(names::ACOUSTID)
            .into_iter()
            .map(|id| id.to_lowercase())
            .collect(),
    );
    claims
}

/// What one item's claims say, as the resolver and the projector read
/// them. The values are the highest-ranked ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// The title from a tag.
    pub title: Option<String>,
    /// The title from the file's name.
    pub title_inferred: Option<String>,
    /// The artist tag's values: one string, or one per value.
    pub artist: Vec<String>,
    /// The values of a multi-valued `ARTISTS` tag.
    pub artists: Vec<String>,
    /// The artist's sort name.
    pub artist_sort: Option<String>,
    /// The artist MusicBrainz IDs.
    pub artist_mbids: Vec<String>,
    /// The album title.
    pub album: Option<String>,
    /// The album artist tag's values.
    pub album_artist: Vec<String>,
    /// The album artist's sort name.
    pub album_artist_sort: Option<String>,
    /// The album artist MusicBrainz IDs.
    pub album_artist_mbids: Vec<String>,
    /// The disc number.
    pub disc: Option<i64>,
    /// The track number, from a tag or else from the file's name.
    pub track: Option<i64>,
    /// The release date, as claimed.
    pub date: Option<String>,
    /// The date the record first came out, as claimed.
    pub original_date: Option<String>,
    /// Whether the compilation flag is set.
    pub compilation: bool,
    /// The primary release type.
    pub primary_type: Option<String>,
    /// The secondary release types, sorted.
    pub secondary_types: Vec<String>,
    /// The recording MusicBrainz ID.
    pub recording_mbid: Option<String>,
    /// The release MusicBrainz ID.
    pub release_mbid: Option<String>,
    /// The release-group MusicBrainz ID.
    pub release_group_mbid: Option<String>,
    /// The ISRCs.
    pub isrcs: Vec<String>,
    /// The AcoustIDs.
    pub acoustids: Vec<String>,
    /// The duration of the audio, in milliseconds.
    pub duration_ms: Option<i64>,
}

impl Facts {
    /// Reads one item's claims.
    pub fn from_claims(claims: &[Claim]) -> Self {
        let all = |identifier: bool, name: &str, source: Source| -> Vec<String> {
            let mut found: Vec<&Claim> = claims
                .iter()
                .filter(|c| c.identifier == identifier && c.name == name && c.source == source)
                .collect();
            found.sort_by_key(|claim| claim.ordinal);
            found.into_iter().map(|claim| claim.value.clone()).collect()
        };
        let tag = |name: &str| all(false, name, Source::FileTags);
        let one = |name: &str| tag(name).into_iter().next();
        let id = |name: &str| all(true, name, Source::FileTags);
        let number = |name: &str, source| {
            all(false, name, source)
                .first()
                .and_then(|value| value.parse::<i64>().ok())
        };
        Self {
            title: one("title"),
            title_inferred: all(false, "title", Source::Inferred).into_iter().next(),
            artist: tag("artist"),
            artists: tag("artists"),
            artist_sort: one("artist_sort"),
            artist_mbids: id("mb_artist"),
            album: one("album"),
            album_artist: tag("album_artist"),
            album_artist_sort: one("album_artist_sort"),
            album_artist_mbids: id("mb_album_artist"),
            disc: number("disc_number", Source::FileTags),
            track: number("track_number", Source::FileTags)
                .or_else(|| number("track_number", Source::Inferred)),
            date: one("date"),
            original_date: one("original_date"),
            compilation: one("compilation").is_some(),
            primary_type: one("primary_type"),
            secondary_types: tag("secondary_type"),
            recording_mbid: id("mb_recording").into_iter().next(),
            release_mbid: id("mb_release").into_iter().next(),
            release_group_mbid: id("mb_release_group").into_iter().next(),
            isrcs: id("isrc"),
            acoustids: id("acoustid"),
            duration_ms: number("duration_ms", Source::FileTags),
        }
    }

    /// The artist as one string: the tag's one value, or its several
    /// joined.
    pub fn artist_text(&self) -> Option<String> {
        (!self.artist.is_empty()).then(|| self.artist.join(", "))
    }

    /// The album artist as one string.
    pub fn album_artist_text(&self) -> Option<String> {
        (!self.album_artist.is_empty()).then(|| self.album_artist.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{Audio, PAYLOAD_VERSION};

    fn tag(key: &str, value: &str) -> Tag {
        let (key, sub) = match key.split_once('|') {
            Some((key, sub)) => (key, Some(sub.to_owned())),
            None => (key, None),
        };
        Tag {
            key: key.to_owned(),
            block: None,
            sub,
            values: vec![value.to_owned()],
            binary_len: None,
        }
    }

    fn from(block: &str, key: &str, value: &str) -> Tag {
        Tag {
            block: Some(block.to_owned()),
            ..tag(key, value)
        }
    }

    fn payload(path: &str, tags: Vec<Tag>) -> Payload {
        Payload {
            v: PAYLOAD_VERSION,
            path: path.to_owned(),
            size: 1,
            mtime_ms: 1,
            container: None,
            audio: Some(Audio {
                hash_scheme: 1,
                hash: "00".repeat(32),
                codec: "flac".to_owned(),
                sample_rate: Some(44_100),
                channels: Some(2),
                bits: Some(16),
                duration_ms: 201_000,
                packets: 1,
                payload_bytes: 1,
                flac_md5: None,
            }),
            tags,
            pictures: Vec::new(),
            folder_images: Vec::new(),
            error: None,
        }
    }

    const REC: &str = "11111111-2222-4333-8444-555555555555";
    const REL: &str = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    const GRP: &str = "99999999-8888-4777-8666-555555555555";
    const ART: &str = "12121212-3434-4565-8787-909090909090";

    #[test]
    fn vorbis_comments_as_picard_writes_them_give_every_claim() {
        let facts = Facts::from_claims(&extract(&payload(
            "Moss Arcade/Tin Weather/02 Paper Kites.flac",
            vec![
                tag("TITLE", "Paper Kites"),
                tag("ARTIST", "Moss Arcade"),
                tag("ALBUM", "Tin Weather"),
                tag("ALBUMARTIST", "Moss Arcade"),
                tag("TRACKNUMBER", "2"),
                tag("TOTALTRACKS", "9"),
                tag("DISCNUMBER", "1"),
                tag("DATE", "2019-03-08"),
                tag("ORIGINALDATE", "2004-10-11"),
                tag("RELEASETYPE", "ep"),
                tag("RELEASETYPE", "live"),
                tag("MUSICBRAINZ_TRACKID", REC),
                tag("MUSICBRAINZ_ALBUMID", REL),
                tag("MUSICBRAINZ_RELEASEGROUPID", GRP),
                tag("MUSICBRAINZ_ARTISTID", ART),
                tag("MUSICBRAINZ_ALBUMARTISTID", ART),
                tag("ISRC", "zz-abc-04-00002"),
                tag("COMPILATION", "1"),
                tag("X-INVENTED", "kept in the payload, not understood"),
            ],
        )));
        assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
        assert_eq!(facts.title_inferred.as_deref(), Some("Paper Kites"));
        assert_eq!(facts.artist, ["Moss Arcade"]);
        assert_eq!(facts.album.as_deref(), Some("Tin Weather"));
        assert_eq!(facts.album_artist, ["Moss Arcade"]);
        assert_eq!((facts.disc, facts.track), (Some(1), Some(2)));
        assert_eq!(facts.date.as_deref(), Some("2019-03-08"));
        assert_eq!(facts.original_date.as_deref(), Some("2004-10-11"));
        assert_eq!(facts.primary_type.as_deref(), Some("ep"));
        assert_eq!(facts.secondary_types, ["live"]);
        assert_eq!(facts.recording_mbid.as_deref(), Some(REC));
        assert_eq!(facts.release_mbid.as_deref(), Some(REL));
        assert_eq!(facts.release_group_mbid.as_deref(), Some(GRP));
        assert_eq!(facts.artist_mbids, [ART]);
        assert_eq!(facts.album_artist_mbids, [ART]);
        assert_eq!(facts.isrcs, ["ZZ-ABC-04-00002"]);
        assert!(facts.compilation);
        assert_eq!(facts.duration_ms, Some(201_000));
    }

    #[test]
    fn id3_frames_are_read_by_frame_id_description_and_owner() {
        let hex: String = REC.bytes().map(|b| format!("{b:02x}")).collect();
        let mut ufid = tag("UFID|http://musicbrainz.org", &hex);
        ufid.binary_len = Some(36);
        let facts = Facts::from_claims(&extract(&payload(
            "x.mp3",
            vec![
                tag("TIT2", "Paper Kites"),
                tag("TPE1", "Moss Arcade"),
                tag("TALB", "Tin Weather"),
                tag("TPE2", "Various Artists"),
                tag("TRCK", "3/12"),
                tag("TPOS", "2/2"),
                tag("TDRC", "2019"),
                tag("TDOR", "2004"),
                tag("TCMP", "1"),
                tag("TXXX|MusicBrainz Album Type", "album/compilation"),
                tag("TXXX|MusicBrainz Album Id", REL),
                tag("TXXX|MusicBrainz Release Group Id", GRP),
                tag("TXXX|MusicBrainz Artist Id", &format!("{ART}/{REL}")),
                tag("TXXX|Acoustid Id", GRP),
                ufid,
            ],
        )));
        assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
        assert_eq!(facts.album_artist, ["Various Artists"]);
        assert_eq!((facts.disc, facts.track), (Some(2), Some(3)));
        assert_eq!(facts.original_date.as_deref(), Some("2004"));
        assert_eq!(facts.primary_type.as_deref(), Some("album"));
        assert_eq!(facts.secondary_types, ["compilation"]);
        assert_eq!(facts.recording_mbid.as_deref(), Some(REC));
        assert_eq!(facts.release_mbid.as_deref(), Some(REL));
        assert_eq!(facts.artist_mbids, [ART, REL]);
        assert_eq!(facts.acoustids, [GRP]);
        assert!(facts.compilation);
    }

    #[test]
    fn mp4_atoms_are_read_by_name_and_freeform_atoms_by_mean_and_name() {
        let facts = Facts::from_claims(&extract(&payload(
            "x.m4a",
            vec![
                tag("\u{a9}nam", "Paper Kites"),
                tag("\u{a9}ART", "Moss Arcade"),
                tag("\u{a9}alb", "Tin Weather"),
                tag("aART", "Moss Arcade"),
                tag("trkn", "4"),
                tag("trkn", "10"),
                tag("disk", "1"),
                tag("\u{a9}day", "2019-03-08"),
                tag("cpil", "true"),
                // As the reader names a freeform atom: its mean and its
                // name, with no `----` in front. The real reader is
                // tested on a fixture in ritornel-engine's media tests.
                tag("com.apple.iTunes:MusicBrainz Track Id", REC),
                tag("com.apple.iTunes:MusicBrainz Album Type", "single"),
                tag("com.apple.iTunes:ORIGINAL YEAR", "1999"),
            ],
        )));
        assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
        assert_eq!((facts.disc, facts.track), (Some(1), Some(4)));
        assert_eq!(facts.recording_mbid.as_deref(), Some(REC));
        assert_eq!(facts.primary_type.as_deref(), Some("single"));
        assert_eq!(facts.original_date.as_deref(), Some("1999"));
        assert!(facts.compilation);
    }

    #[test]
    fn a_field_is_read_from_one_block_of_tags_and_a_value_there_twice_is_one() {
        // A FLAC with an ID3v2 tag in front of its Vorbis comments and an
        // ID3v1 tag after them: three blocks that say the same thing, and
        // one artist.
        let facts = Facts::from_claims(&extract(&payload(
            "x.flac",
            vec![
                from("id3v2", "TIT2", "Paper Kites (as ID3v2 has it)"),
                from("id3v2", "TPE1", "Moss Arcade"),
                from("id3v2", "TPOS", "2"),
                from("flac", "TITLE", "Paper Kites"),
                from("flac", "ARTIST", "Moss Arcade"),
                from("flac", "ARTIST", "Moss Arcade"),
                from("flac", "ALBUM ARTIST", "Moss Arcade"),
                from("flac", "ALBUMARTIST", "Moss Arcade"),
                from("id3v1", "TITLE", "Paper Kites (cut to thirty ch"),
                from("id3v1", "ARTIST", "Moss Arcade"),
                from("id3v1", "ALBUM", "Only ID3v1 has the album"),
            ],
        )));
        assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
        assert_eq!(facts.artist, ["Moss Arcade"]);
        assert_eq!(facts.album_artist, ["Moss Arcade"]);
        // What only a lesser block has is still read.
        assert_eq!(facts.disc, Some(2));
        assert_eq!(facts.album.as_deref(), Some("Only ID3v1 has the album"));

        // An MP3 whose ID3v1 tag comes first in what was read: ID3v2 wins.
        let facts = Facts::from_claims(&extract(&payload(
            "x.mp3",
            vec![
                from("id3v1", "ARTIST", "A Name That Is Longer Than Thi"),
                from(
                    "id3v2",
                    "TPE1",
                    "A Name That Is Longer Than Thirty Characters",
                ),
            ],
        )));
        assert_eq!(
            facts.artist,
            ["A Name That Is Longer Than Thirty Characters"]
        );
    }

    #[test]
    fn the_three_letter_frames_of_id3v2_2_are_read_as_what_they_became() {
        let facts = Facts::from_claims(&extract(&payload(
            "x.mp3",
            vec![
                from("id3v2", "TT2", "Paper Kites"),
                from("id3v2", "TP1", "Moss Arcade"),
                from("id3v2", "TP2", "Various Artists"),
                from("id3v2", "TAL", "Tin Weather"),
                from("id3v2", "TRK", "3/12"),
                from("id3v2", "TPA", "1/2"),
                from("id3v2", "TYE", "2004"),
                from("id3v2", "TCP", "1"),
                from("id3v2", "TXX|MusicBrainz Album Id", REL),
            ],
        )));
        assert_eq!(facts.title.as_deref(), Some("Paper Kites"));
        assert_eq!(facts.artist, ["Moss Arcade"]);
        assert_eq!(facts.album_artist, ["Various Artists"]);
        assert_eq!(facts.album.as_deref(), Some("Tin Weather"));
        assert_eq!((facts.disc, facts.track), (Some(1), Some(3)));
        assert_eq!(facts.date.as_deref(), Some("2004"));
        assert_eq!(facts.release_mbid.as_deref(), Some(REL));
        assert!(facts.compilation);
    }

    #[test]
    fn a_file_with_no_tags_claims_its_name_and_its_duration() {
        let claims = extract(&payload("Loose/07 - Harbour Lights.flac", Vec::new()));
        let facts = Facts::from_claims(&claims);
        assert_eq!(facts.title, None);
        assert_eq!(facts.title_inferred.as_deref(), Some("Harbour Lights"));
        assert_eq!(facts.track, Some(7));
        assert_eq!(facts.album, None);
        assert!(claims.iter().all(|claim| !claim.identifier));

        let named = |path: &str| Facts::from_claims(&extract(&payload(path, Vec::new())));
        assert_eq!(named("1979.flac").title_inferred.as_deref(), Some("1979"));
        assert_eq!(named("1979.flac").track, None);
        assert_eq!(named("a/03.flac").title_inferred.as_deref(), Some("03"));
    }

    #[test]
    fn a_multi_valued_artist_keeps_each_value_in_order() {
        let facts = Facts::from_claims(&extract(&payload(
            "x.flac",
            vec![tag("ARTIST", "Ada Wren"), tag("ARTIST", "Low Tide Choir")],
        )));
        assert_eq!(facts.artist, ["Ada Wren", "Low Tide Choir"]);
        assert_eq!(
            facts.artist_text().as_deref(),
            Some("Ada Wren, Low Tide Choir")
        );
    }

    #[test]
    fn a_date_is_as_precise_as_it_is_written() {
        assert_eq!(parse_date("2004"), Some(("2004".to_owned(), 2004)));
        assert_eq!(parse_date("2004-10"), Some(("2004-10".to_owned(), 2004)));
        assert_eq!(
            parse_date("2004-10-11T00:00:00Z"),
            Some(("2004-10-11".to_owned(), 2004))
        );
        assert_eq!(parse_date("2004-13-01"), Some(("2004".to_owned(), 2004)));
        assert_eq!(parse_date("October 2004"), None);
        assert_eq!(parse_date("0000"), None);
        assert_eq!(parse_date(""), None);
    }

    #[test]
    fn a_release_type_that_musicbrainz_does_not_have_is_no_claim() {
        let facts = Facts::from_claims(&extract(&payload(
            "x.flac",
            vec![tag("RELEASETYPE", "Bootleg; Mixtape/Street")],
        )));
        assert_eq!(facts.primary_type, None);
        assert_eq!(facts.secondary_types, ["mixtape/street"]);
    }
}
