//! Text normalisation: what makes two spellings of a name the same key.
//! It is mechanical (NFKC, case fold, punctuation strip, whitespace
//! collapse), it only finds candidates, and it never decides. See decision
//! 12 and "Displayed metadata" in docs/design/identity.md.

use unicode_normalization::UnicodeNormalization;

/// The version of [`normalize`] and [`core`]. A stored key is only
/// comparable with one made by the same version.
pub const NORMALIZER_VERSION: i64 = 1;

/// The key of a name: NFKC, lower case, punctuation and symbols removed,
/// runs of white space made one space. A name that is nothing but
/// punctuation keeps it, so that `!!!` and `???` stay two names.
pub fn normalize(text: &str) -> String {
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
    }

    #[test]
    fn a_name_of_punctuation_alone_keeps_it() {
        assert_eq!(normalize("!!!"), "!!!");
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
