//! Text normalisation: what makes two spellings of a name the same key.
//! It is mechanical (NFKC, case fold, punctuation strip, whitespace
//! collapse), it only finds candidates, and it never decides. See decision
//! 12 and "Displayed metadata" in docs/design/identity.md.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// The version of [`normalize`] and [`core`]. A stored key is only
/// comparable with one made by the same version.
///
/// 2: a mark is kept wherever NFKC leaves it, so a virama or a tone mark
/// keeps two names apart; punctuation that parts words (a dash, a slash, a
/// plus) parts them as a space does; and "&" reads as "and".
pub const NORMALIZER_VERSION: i64 = 2;

/// Whether a mark between two words parts them as a space does: dashes,
/// slashes and the like. Everything else that is not a letter, a digit or a
/// mark joins what is on either side of it, as an apostrophe does.
fn parts_words(c: char) -> bool {
    matches!(
        c,
        '-' | '\u{2010}'
            ..='\u{2015}'
                | '\u{2212}'
                | '\u{2E3A}'
                | '\u{2E3B}'
                | '/'
                | '\\'
                | '|'
                | '+'
                | ','
                | ';'
                | ':'
                | '_'
                | '~'
                | '\u{00B7}'
                | '\u{2022}'
    )
}

/// The key of a name: NFKC, lower case, "&" as "and", punctuation that
/// parts words made a space, other punctuation and symbols removed, runs of
/// white space made one space. A name that is nothing but punctuation
/// keeps it, so that `!!!` and `???` stay two names.
pub fn normalize(text: &str) -> String {
    // Lower case can leave a letter and a mark that compose: composed
    // again, and the dot that a capital dotted I leaves on its small i
    // taken off, as Turkish reads it.
    let folded: String = text
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .nfc()
        .collect::<String>()
        .replace("i\u{0307}", "i");
    // "&" alone is a name of punctuation, not the word "and".
    if !folded.chars().any(char::is_alphanumeric) {
        return folded.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let mut spaced = String::with_capacity(folded.len());
    for c in folded.chars() {
        match c {
            '&' => spaced.push_str(" and "),
            c if parts_words(c) => spaced.push(' '),
            c => spaced.push(c),
        }
    }
    let mut key = String::with_capacity(spaced.len());
    for word in spaced.split_whitespace() {
        let kept: String = word
            .chars()
            .filter(|c| c.is_alphanumeric() || is_combining_mark(*c))
            .collect();
        if !kept.chars().any(char::is_alphanumeric) {
            continue;
        }
        if !key.is_empty() {
            key.push(' ');
        }
        key.push_str(&kept);
    }
    if key.is_empty() {
        folded.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        key
    }
}

/// The key of a name as normaliser `version` made it, for finding what an
/// earlier version keyed: `None` for a version this one does not know.
pub fn normalize_by(version: i64, text: &str) -> Option<String> {
    match version {
        1 => Some(normalize_v1(text)),
        NORMALIZER_VERSION => Some(normalize(text)),
        _ => None,
    }
}

/// The words of a name with punctuation inside a word joining the letters
/// on either side, as "Re-recorded" is `rerecorded`: what version markers
/// are read from, beside the words punctuation parts.
pub(crate) fn joined_words(text: &str) -> String {
    normalize_v1(text)
}

/// Version 1's key: punctuation inside a word joined the letters on either
/// side, and every mark was dropped.
fn normalize_v1(text: &str) -> String {
    let folded: String = text.nfkc().flat_map(char::to_lowercase).collect();
    let mut key = String::with_capacity(folded.len());
    for word in folded.split_whitespace() {
        let kept: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if kept.is_empty() {
            continue;
        }
        if !key.is_empty() {
            key.push(' ');
        }
        key.push_str(&kept);
    }
    if key.is_empty() {
        folded.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        key
    }
}

/// The key of a title without what is written in brackets after it, such as
/// `(Remastered 2011)` or `[Deluxe Edition]`. It finds candidates among
/// titles that differ only in such a note. A title that is all brackets
/// keeps its full key.
pub fn core(text: &str) -> String {
    let mut outside = String::with_capacity(text.len());
    let mut depth = 0_u32;
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => outside.push(c),
            _ => {}
        }
    }
    let key = normalize(&outside);
    if key.is_empty() || !key.chars().any(char::is_alphanumeric) {
        normalize(text)
    } else {
        key
    }
}

/// A key with its accents taken off: each Latin letter decomposed and its
/// marks dropped, and the few Latin letters that have no decomposition
/// written as the letters they are read as. Marks on letters of other
/// scripts stay, because there they make a different letter: `й` is not
/// `и`, and `が` is not `か`. It is a switch of the resolver's policy,
/// applied to candidate text only, and never to a stored name.
pub fn fold_accents(key: &str) -> String {
    let latin = |c: char| {
        c.is_ascii_alphabetic()
            || ('\u{00c0}'..='\u{024f}').contains(&c)
            || ('\u{1e00}'..='\u{1eff}').contains(&c)
    };
    let mut folded = String::with_capacity(key.len());
    let mut after_latin = false;
    for c in key.nfd() {
        if unicode_normalization::char::is_combining_mark(c) {
            if !after_latin {
                folded.push(c);
            }
            continue;
        }
        after_latin = latin(c);
        match c {
            'ø' => folded.push('o'),
            'ł' => folded.push('l'),
            'đ' | 'ð' => folded.push('d'),
            'ı' => folded.push('i'),
            'æ' => folded.push_str("ae"),
            'œ' => folded.push_str("oe"),
            'ß' => folded.push_str("ss"),
            'þ' => folded.push_str("th"),
            _ => folded.push(c),
        }
    }
    folded.nfc().collect()
}

/// How far apart two keys are, from 0 (the same) to 1 (nothing in common):
/// the edit distance over the length of the longer.
pub fn difference(a: &str, b: &str) -> f64 {
    if a == b {
        return 0.0;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let longer = a.len().max(b.len());
    if longer == 0 {
        return 0.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = i;
        let mut left = i + 1;
        for (cell, y) in row.iter_mut().skip(1).zip(&b) {
            let above = *cell;
            let cost = diagonal + usize::from(x != y);
            left = cost.min(above + 1).min(left + 1);
            diagonal = above;
            *cell = left;
        }
        if let Some(first) = row.first_mut() {
            *first = i + 1;
        }
    }
    let distance = row.last().copied().unwrap_or(longer);
    distance as f64 / longer as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_ignores_case_width_punctuation_and_spacing() {
        assert_eq!(normalize("  The  Quiet\tHarbour "), "the quiet harbour");
        assert_eq!(normalize("Ｆｕｌｌ Ｗｉｄｔｈ"), "full width");
        assert_eq!(normalize("Don't Stop!"), "dont stop");
        assert_eq!(normalize("Ne$ta"), "neta");
        assert_eq!(normalize("Song - Part 2"), "song part 2");
        assert_eq!(normalize("ÉTÉ"), "été");
        // A capital dotted I is a small i, as in Turkish.
        assert_eq!(normalize("İzmir"), normalize("izmir"));
        assert_eq!(normalize("H\u{0331}"), normalize("\u{1E96}"));
    }

    #[test]
    fn punctuation_between_words_is_the_same_with_or_without_spaces_round_it() {
        // A dash, a slash or a plus parts the words on either side, as the
        // same mark with spaces round it does.
        for (joined, spaced) in [
            ("Lanterns-42", "Lanterns - 42"),
            ("lanterns\u{2010}42", "Lanterns 42"),
            ("Here/There", "Here / There"),
            ("Tide+Shore", "Tide + Shore"),
            ("Glass:Water", "Glass: Water"),
        ] {
            assert_eq!(normalize(joined), normalize(spaced), "{joined}");
        }
        assert_eq!(normalize("Lanterns\u{2014}42"), "lanterns 42");
        // "&" reads as the word it stands for, wherever it is written.
        assert_eq!(normalize("Mira & Vale"), "mira and vale");
        assert_eq!(normalize("Mira&Vale"), normalize("Mira and Vale"));
        // What joins a word stays joined.
        assert_eq!(normalize("Don't"), normalize("Dont"));
        assert_eq!(normalize("M.I.R.A."), normalize("MIRA"));
        assert_eq!(normalize("Ne$ta"), "neta");
    }

    #[test]
    fn a_mark_that_makes_another_letter_keeps_two_names_apart() {
        // A virama, which joins two consonants, and a Thai tone mark: none
        // has a composed form for NFKC to fold it into.
        let virama = "\u{0915}\u{094D}\u{0937}";
        assert_ne!(normalize(virama), normalize("\u{0915}\u{0937}"));
        let tone = "\u{0E43}\u{0E01}\u{0E25}\u{0E49}";
        assert_ne!(normalize(tone), normalize("\u{0E43}\u{0E01}\u{0E25}"));
        // A Latin letter with a mark it has no composed form for.
        assert_ne!(normalize("Spin\u{0308}al"), normalize("Spinal"));
        // Folding accents to find candidates still takes the Latin one off.
        assert_eq!(fold_accents(&normalize("Spin\u{0308}al")), "spinal");
    }

    #[test]
    fn the_first_version_of_a_key_can_be_made_again() {
        assert_eq!(
            normalize_by(1, "Lanterns-42"),
            Some("lanterns42".to_owned())
        );
        assert_eq!(normalize_by(1, "Mira & Vale"), Some("mira vale".to_owned()));
        assert_eq!(
            normalize_by(NORMALIZER_VERSION, "Lanterns-42"),
            Some(normalize("Lanterns-42"))
        );
        assert_eq!(normalize_by(0, "Lanterns"), None);
    }

    #[test]
    fn a_name_of_punctuation_alone_keeps_it() {
        assert_eq!(normalize("!!!"), "!!!");
        assert_eq!(normalize("&"), "&");
        assert_ne!(normalize("&"), normalize("And"));
        assert_ne!(normalize("!!!"), normalize("???"));
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn the_core_of_a_title_drops_what_is_in_brackets() {
        assert_eq!(core("Lantern (Remastered 2011)"), "lantern");
        assert_eq!(core("Lantern [Live] (Radio Edit)"), "lantern");
        assert_eq!(core("(Untitled)"), "untitled");
        assert_eq!(core("Lantern"), "lantern");
    }

    #[test]
    fn folding_takes_accents_off_a_key_and_leaves_the_rest() {
        assert_eq!(fold_accents(&normalize("Été à Noël")), "ete a noel");
        assert_eq!(fold_accents("søren łódź straße"), "soren lodz strasse");
        assert_eq!(fold_accents("ünïcödé"), "unicode");
        assert_eq!(fold_accents("plain 42"), "plain 42");
        // Letters of other scripts keep what is not a mark.
        assert_eq!(fold_accents("東京"), "東京");
        assert_eq!(fold_accents("がй"), "がй");
    }

    #[test]
    fn the_difference_is_the_edit_distance_over_the_longer_length() {
        assert_eq!(difference("abc", "abc"), 0.0);
        assert_eq!(difference("", ""), 0.0);
        assert_eq!(difference("abc", ""), 1.0);
        assert!((difference("kitten", "sitting") - 3.0 / 7.0).abs() < 1e-9);
        assert_eq!(difference("abc", "xyz"), 1.0);
    }
}
