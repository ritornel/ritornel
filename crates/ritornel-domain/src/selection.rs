//! Choosing which copy of a track to play. See "Copy selection" in
//! docs/internals/playback.md: selection is a pure function that
//! returns the candidates in order, and the player opens the first that
//! works.
//!
//! M1 has one kind of candidate, a master on this server, so the order is
//! among masters alone: lossless first, then the higher sample rate and bit
//! depth, then the higher bitrate, then the lowest asset.

/// One copy the player could open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The source item of the file.
    pub source_item: i64,
    /// The audio it holds.
    pub asset: i64,
    /// Whether that audio is lossless.
    pub lossless: bool,
    /// Its sample rate.
    pub sample_rate_hz: i64,
    /// Its bit depth, where the codec has one.
    pub bit_depth: Option<i64>,
    /// Its bitrate, where it is known.
    pub bitrate_kbps: Option<i64>,
    /// Whether the last scan found the file gone.
    pub missing: bool,
}

/// The candidates worth trying, best first. A file the last scan did not
/// find is left out.
pub fn select(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut chosen: Vec<Candidate> = candidates
        .iter()
        .filter(|candidate| !candidate.missing)
        .cloned()
        .collect();
    chosen.sort_by(|a, b| {
        b.lossless
            .cmp(&a.lossless)
            .then(b.sample_rate_hz.cmp(&a.sample_rate_hz))
            .then(b.bit_depth.cmp(&a.bit_depth))
            .then(b.bitrate_kbps.cmp(&a.bitrate_kbps))
            .then(a.asset.cmp(&b.asset))
            .then(a.source_item.cmp(&b.source_item))
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy(source_item: i64, asset: i64, lossless: bool, rate: i64) -> Candidate {
        Candidate {
            source_item,
            asset,
            lossless,
            sample_rate_hz: rate,
            bit_depth: lossless.then_some(16),
            bitrate_kbps: (!lossless).then_some(320),
            missing: false,
        }
    }

    #[test]
    fn lossless_comes_first_then_the_finer_copy_then_the_lowest_asset() {
        let mut hi_res = copy(4, 9, true, 96_000);
        hi_res.bit_depth = Some(24);
        let missing = Candidate {
            missing: true,
            ..copy(5, 1, true, 192_000)
        };
        let chosen = select(&[
            copy(1, 3, false, 44_100),
            copy(2, 7, true, 44_100),
            hi_res.clone(),
            copy(3, 6, true, 44_100),
            missing,
        ]);
        let order: Vec<i64> = chosen.iter().map(|copy| copy.source_item).collect();
        assert_eq!(order, [4, 3, 2, 1]);
    }
}
