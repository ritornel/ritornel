//! Decide before opening a decoder. A pass-through uses the selected master
//! file directly, so its gapless metadata and compressed audio stay intact.

use super::{Profile, Target};
use ritornel_domain::selection::{Candidate, preference};

#[derive(Debug, PartialEq, Eq)]
pub enum SourceChoice<'a> {
    PassThrough(&'a Candidate),
    Encode(&'a Candidate),
    Blocked,
}

/// Candidates carry their scanned codec alongside the playback quality rank.
/// An accepted lossy copy is usable even when a better lossy copy has the
/// wrong codec. FLAC pass-through requires an actual FLAC master.
pub fn choose_source<'a>(
    copies: &'a [(Candidate, &str)],
    profile: Profile,
    accepts: impl Fn(&str) -> bool,
    allow_lossy_encode: bool,
) -> std::io::Result<SourceChoice<'a>> {
    profile.validate()?;
    let available = copies.iter().filter(|(c, _)| !c.missing);
    let best = |a: &&(Candidate, &str), b: &&(Candidate, &str)| preference(&a.0, &b.0);
    let Some((first, _)) = available.clone().min_by(best) else {
        return Ok(SourceChoice::Blocked);
    };
    if first.lossless {
        if matches!(profile.target, Target::Flac {}) {
            return Ok(available
                .filter(|(c, codec)| c.lossless && *codec == "flac")
                .min_by(best)
                .map_or(SourceChoice::Blocked, |(c, _)| SourceChoice::PassThrough(c)));
        }
        return Ok(SourceChoice::Encode(first));
    }
    if let Some((accepted, _)) = available.filter(|(_, codec)| accepts(codec)).min_by(best) {
        return Ok(SourceChoice::PassThrough(accepted));
    }
    Ok(
        if allow_lossy_encode && !matches!(profile.target, Target::Flac {}) {
            SourceChoice::Encode(first)
        } else {
            SourceChoice::Blocked
        },
    )
}
