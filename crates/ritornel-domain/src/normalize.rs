//! Text normalisation: what makes two spellings of a name the same key.
//! It is mechanical (NFKC, case fold, punctuation strip, whitespace
//! collapse), it only finds candidates, and it never decides. See
//! "Extraction and normalization" in docs/internals/identity.md.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// The version of stored name keys made by [`normalize`]. A title core
/// is computed when comparing candidates and is never stored as a name key.
///
/// 2: a mark is kept wherever NFKC leaves it, so a virama or a tone mark
/// keeps two names apart; punctuation that parts words (a dash, a slash, a
/// plus) parts them as a space does; and "&" reads as "and".
///
/// 3: a symbol at either end of a name, such as the plus of "Night Drives
/// +", is a word of its key, so a record named apart by one is not the
/// same record.
pub const NORMALIZER_VERSION: i64 = 3;

/// Whether a mark between two words parts them as a space does: dashes,
/// slashes and the like. Everything else that is not a letter, a digit or a
/// mark joins what is on either side of it, as an apostrophe does.
fn parts_words(c: char) -> bool {
    matches!(c, '\u{2010}'..='\u{2015}')
        || "-\u{2212}\u{2E3A}\u{2E3B}/\\|+,;:_~\u{00B7}\u{2022}\u{2044}\u{2215}\u{30FB}\u{3001}\u{301C}\u{00D7}()[]{}"
            .contains(c)
}

/// Whether a mark is a symbol, as Unicode's maths, currency and other
/// symbols are, rather than punctuation: `+`, `=`, `÷`, `$`, `★`.
fn is_symbol(c: char) -> bool {
    !c.is_alphanumeric()
        && matches!(c,
            '+' | '<' | '=' | '>' | '$'
            | '\u{a2}'..='\u{a6}' | '\u{a9}' | '\u{ac}' | '\u{ae}' | '\u{b0}' | '\u{b1}'
            | '\u{d7}' | '\u{f7}'
            | '\u{20a0}'..='\u{20cf}' | '\u{2100}'..='\u{2bff}' | '\u{1f000}'..='\u{1faff}')
        // A mark that parts words, as a dash, a slash or a tilde does, is
        // punctuation however it is written, but for a plus and a times.
        && !(parts_words(c) && !matches!(c, '+' | '\u{d7}'))
        // The brackets among them, ornamental and mathematical, are
        // punctuation.
        && !matches!(c,
            '\u{2308}'..='\u{230b}' | '\u{2329}' | '\u{232a}' | '\u{2768}'..='\u{2775}'
            | '\u{27c5}' | '\u{27c6}' | '\u{27e6}'..='\u{27ef}' | '\u{2983}'..='\u{2998}'
            | '\u{29d8}'..='\u{29db}' | '\u{29fc}' | '\u{29fd}')
}

/// Whether a mark only says whether the character before it is drawn as
/// text or as an emoji: no part of a key from version 3. Other variation
/// selectors, which can make another ideograph, stay.
fn is_presentation(c: char) -> bool {
    matches!(c, '\u{fe0e}' | '\u{fe0f}')
}

/// The symbols in the marks before a name's first letter or digit, or
/// after its last.
fn edge_symbols(marks: impl Iterator<Item = char>) -> String {
    marks
        // "&" is the word "and" in a name with letters.
        .take_while(|c| !c.is_alphanumeric() && *c != '&')
        .filter(|c| is_symbol(*c))
        .collect()
}

/// A name without the bracketed notes at its end: "Night Drives +" of
/// `Night Drives + (Deluxe) [2024]`.
fn without_trailing_notes(text: &str) -> &str {
    let mut end = text.trim_end();
    while end.ends_with([')', ']', '}']) {
        let mut depth = 0_u32;
        let mut open = None;
        for (at, c) in end.char_indices().rev() {
            match c {
                ')' | ']' | '}' => depth += 1,
                '(' | '[' | '{' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        open = Some(at);
                        break;
                    }
                }
                _ => {}
            }
        }
        match open {
            // A bracket of symbols alone is no note: "Night Drives (+)".
            Some(at)
                if end[at..].chars().any(|c| c.is_alphanumeric() || c == '&')
                    || !end[at..].chars().any(is_symbol) =>
            {
                end = end[..at].trim_end();
            }
            _ => break,
        }
    }
    end
}

/// The key of a name: NFKC, lower case, "&" as "and", punctuation that
/// parts words made a space, other punctuation and symbols removed, runs of
/// white space made one space. A symbol at either end stays, as a word of
/// its own, so that "Night Drives +" is not "Night Drives". A name that is
/// nothing but punctuation keeps it, so that `!!!` and `???` stay two
/// names.
pub fn normalize(text: &str) -> String {
    normalize_with(text, true)
}

/// The key of a name without the symbols at either end, as version 2 made
/// it: for telling whether two texts that came back from one place name
/// the same thing.
pub fn words_key(text: &str) -> String {
    normalize_with(text, false)
}

/// [`normalize`], keeping the symbols at either end or not, as version 2
/// did not.
fn normalize_with(text: &str, edges: bool) -> String {
    // Lower case can leave a letter and a mark that compose: composed
    // again, and the dot that a capital dotted I leaves on its small i
    // taken off, as Turkish reads it.
    let folded: String = text
        .replace('\u{0130}', "I")
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .replace("i\u{0307}", "i")
        .nfc()
        .collect();
    // "&" alone is a name of punctuation, not the word "and".
    if !folded.chars().any(char::is_alphanumeric) {
        let drawn: String = folded
            .chars()
            .filter(|c| !(edges && is_presentation(*c)))
            .collect();
        let kept = if drawn.trim().is_empty() {
            folded.as_str()
        } else {
            drawn.as_str()
        };
        return kept.split_whitespace().collect::<Vec<_>>().join(" ");
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
            .filter(|c| {
                (c.is_alphanumeric() || is_combining_mark(*c)) && !(edges && is_presentation(*c))
            })
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
        return folded.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    if edges {
        let (first, last) = (
            edge_symbols(folded.chars()),
            edge_symbols(folded.chars().rev()),
        );
        if !first.is_empty() {
            key.insert(0, ' ');
            key.insert_str(0, &first);
        }
        if !last.is_empty() {
            key.push(' ');
            key.extend(last.chars().rev());
        } else {
            // A symbol just before the notes at the end keeps its place.
            let head = without_trailing_notes(&folded);
            let before = edge_symbols(head.chars().rev());
            if !before.is_empty()
                && head.len() < folded.trim_end().len()
                && head.chars().any(char::is_alphanumeric)
            {
                let at =
                    normalize_with(head, false).split(' ').count() + usize::from(!first.is_empty());
                let mut words: Vec<&str> = key.split(' ').collect();
                let before: String = before.chars().rev().collect();
                words.insert(at.min(words.len()), &before);
                key = words.join(" ");
            }
        }
    }
    // Taking punctuation out can bring a letter and its mark together.
    key.nfc().collect()
}

/// The key of a name as normaliser `version` made it, for finding what an
/// earlier version keyed: `None` for a version this one does not know.
pub fn normalize_by(version: i64, text: &str) -> Option<String> {
    match version {
        1 => Some(normalize_v1(text)),
        2 => Some(normalize_with(text, false)),
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
    core_with_fullwidth(text, true)
}

/// Earlier file policies recognise only ASCII brackets around notes.
pub(crate) fn core_with_fullwidth(text: &str, fullwidth: bool) -> String {
    let mut outside = String::with_capacity(text.len());
    // A bracket of symbols alone, as "(+)", is kept: it is no note.
    let mut inside = String::new();
    let mut depth = 0_u32;
    // Read the original brackets before NFKC turns an enclosed numeral
    // such as ⑴ into "(1)", which would look like a note.
    for c in text.chars() {
        match c {
            '(' | '[' | '{' | '（' | '［' | '｛' if fullwidth || c.is_ascii() => depth += 1,
            ')' | ']' | '}' | '）' | '］' | '｝' if fullwidth || c.is_ascii() => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let folded: String = inside.nfkc().collect();
                    let words = folded.chars().any(|c| c.is_alphanumeric() || c == '&');
                    if !words && folded.chars().any(is_symbol) {
                        outside.push(' ');
                        outside.push_str(&inside);
                        outside.push(' ');
                    }
                    inside.clear();
                }
            }
            _ if depth == 0 => outside.push(c),
            _ => inside.push(c),
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
        assert_eq!(normalize("İ\u{0323}"), normalize("i\u{0323}"));
        assert_eq!(normalize("e'\u{0301}"), normalize("é"));
        for spelling in [
            "Here⁄There",
            "Here∕There",
            "Here・There",
            "Here×There",
            "Here〜There",
            "Here(There)",
        ] {
            assert_eq!(normalize(spelling), "here there", "{spelling}");
        }
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
    fn a_symbol_at_either_end_of_a_name_stays_in_its_key() {
        // "Night Drives +" is another record than "Night Drives", however
        // the symbol is spaced or bracketed.
        assert_ne!(normalize("Night Drives +"), normalize("Night Drives"));
        assert_eq!(normalize("Night Drives +"), "night drives +");
        for spelling in ["Night Drives+", "Night Drives (+)", " Night Drives  + "] {
            assert_eq!(normalize(spelling), "night drives +", "{spelling}");
        }
        // Records named by a symbol keep it beside a note.
        let keys: Vec<String> = ["+ (Deluxe)", "= (Deluxe)", "\u{f7} (Deluxe)", "Deluxe"]
            .into_iter()
            .map(normalize)
            .collect();
        for (at, key) in keys.iter().enumerate() {
            assert!(!keys[at + 1..].contains(key), "{keys:?}");
        }
        assert_eq!(normalize("\u{f7} (Deluxe)"), "\u{f7} deluxe");
        // A symbol between words still parts them, and punctuation at
        // either end is still no part of a name.
        assert_eq!(normalize("Mira + Vale"), "mira vale");
        assert_eq!(normalize("Mira+Vale"), "mira vale");
        for spelling in [
            "Night Drives!",
            "(Night Drives)",
            "Night Drives...",
            "*Night Drives",
            "Night Drives -",
        ] {
            assert_eq!(normalize(spelling), "night drives", "{spelling}");
        }
        // A symbol in brackets of its own stays in the key without notes
        // too, and an emoji's presentation is no part of the key.
        assert_eq!(core("Night Drives (+)"), "night drives +");
        assert_eq!(core("Night Drives +"), "night drives +");
        assert_eq!(core("Night Drives (Live)"), "night drives");
        assert_eq!(normalize("Mira\u{2764}\u{fe0f}"), "mira \u{2764}");
        assert_eq!(normalize("Mira \u{2764}\u{fe0f}"), "mira \u{2764}");
        // A symbol before the notes at the end is kept where it is.
        assert_eq!(
            normalize("Night Drives + (Deluxe)"),
            "night drives + deluxe"
        );
        assert_ne!(
            normalize("Night Drives + (Deluxe)"),
            normalize("Night Drives (Deluxe)")
        );
        assert_eq!(core("Night Drives + (Deluxe)"), "night drives +");
        // A bracket of symbols before a note is the symbol, not a note.
        assert_eq!(
            normalize("Night Drives (+) (Deluxe)"),
            "night drives + deluxe"
        );
        // A plus however wide, in brackets too.
        assert_eq!(core("Night Drives (\u{ff0b})"), "night drives +");
        // A selector that makes another ideograph keeps two names apart.
        assert_ne!(
            normalize("\u{845b}\u{e0100}"),
            normalize("\u{845b}\u{e0101}")
        );
        assert_eq!(normalize("\u{2764}\u{fe0f}"), normalize("\u{2764}"));
        // A bracket with "&" in it is a note, as one with "and" is.
        assert_eq!(core("Night Drives (& +)"), core("Night Drives (and +)"));
        // With a leading symbol too, and several notes.
        assert_eq!(
            normalize("\u{2605} Night Drives + (Deluxe) [2024]"),
            "\u{2605} night drives + deluxe 2024"
        );
        // "&" is "and" wherever it is.
        assert_eq!(normalize("Night Drives + &"), "night drives and");
        // Marks that part words are not symbols, however they are written.
        assert_eq!(normalize("\u{ff5e}Night Drives\u{ff5e}"), "night drives");
        assert_eq!(normalize("\u{301c}Night Drives\u{301c}"), "night drives");
        assert_eq!(normalize("Night Drives \u{2212}"), "night drives");
        assert_eq!(normalize("Night Drives |"), "night drives");
        // Ornamental brackets are brackets, not symbols.
        assert_eq!(normalize("\u{2768}Night Drives\u{2769}"), "night drives");
        // Version 2 dropped it, and its keys can be made again.
        assert_eq!(
            normalize_by(2, "Night Drives +"),
            Some("night drives".to_owned())
        );
        assert_eq!(normalize_by(2, "Mira - Vale"), Some("mira vale".to_owned()));
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
        assert_eq!(core("Lantern（Remastered）"), "lantern");
        assert_eq!(core("Lantern［＋］"), core("Lantern [+]"));
        assert_eq!(core("（Untitled）"), "untitled");
        assert_eq!(core("Lantern ⑴"), "lantern 1");
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
