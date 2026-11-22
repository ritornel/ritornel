//! Text as a person compares it: for ordering a list by name and for
//! matching what was typed. See "Large lists" in docs/internals/contract.md
//! and "Library walls and cover loading" in docs/internals/desktop.md.

use ritornel_domain::normalize::fold_accents;
use unicode_normalization::UnicodeNormalization;

/// `text` with case and accents taken away: lower-cased, and its accents
/// folded as the resolver folds them, so "Éclair" and "eclair", or "Røyksopp"
/// and "Royksopp", fold the same while a mark that makes another letter in
/// another script stays.
pub(crate) fn fold(text: &str) -> String {
    // FTS5 treats NUL as the end of a MATCH expression, even inside quotes.
    if text.contains('\0') {
        return fold(&text.replace('\0', ""));
    }
    // Most names are plain ASCII, which has nothing to fold but case.
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let lower: String = text.nfkc().flat_map(char::to_lowercase).collect();
    fold_accents(&lower)
}

/// The words a person files a name under, from the start: folded, with
/// anything that is not a letter or a digit read as a space between words.
fn words(text: &str) -> Vec<String> {
    fold(text)
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `text` as its folded words with one space between each: "AC/DC" is
/// "ac dc". Empty for text with no letter or digit.
pub(crate) fn spaced(text: &str) -> String {
    // Search folds every title in the library, so this makes one string
    // and not one a word.
    let folded = fold(text);
    let mut spaced = String::with_capacity(folded.len());
    for word in folded
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if !spaced.is_empty() {
            spaced.push(' ');
        }
        spaced.push_str(word);
    }
    spaced
}

/// What a name or a title is filed under: its folded words, with a leading
/// "the", "a" or "an" set aside when it is a word of its own and more
/// follows. A sort name that is given is taken as it is written, since
/// whoever wrote it chose the order; one that only repeats the name says
/// nothing, and a scan writes the name there when the tags have no sort
/// name.
pub(crate) fn sort_text(display: &str, sort_name: Option<&str>) -> String {
    if let Some(sort_name) =
        sort_name.filter(|name| !name.trim().is_empty() && name.trim() != display.trim())
    {
        let words = words(sort_name);
        if !words.is_empty() {
            return words.join(" ");
        }
    }
    let mut words = words(display);
    if words.len() > 1 && starts_with_article(display) {
        words.remove(0);
    }
    if words.is_empty() {
        // A name with no letter or digit in it is filed as it is written.
        return fold(display);
    }
    words.join(" ")
}

/// Whether `text` begins with "the", "a" or "an" and a space: "A.C." and
/// "A-ha" do not.
fn starts_with_article(text: &str) -> bool {
    let text = text.trim_start();
    ["the", "a", "an"].iter().any(|article| {
        text.get(..article.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(article))
            && text
                .get(article.len()..)
                .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })
}

/// The letter a filed name goes under: A to Z, or `#` for one that begins
/// with anything else, which comes after Z.
pub(crate) fn letter(sort_text: &str) -> char {
    match sort_text.chars().next() {
        Some(first @ 'a'..='z') => first.to_ascii_uppercase(),
        _ => '#',
    }
}

/// The order filed names go in: by letter, `#` last, then by the text.
pub(crate) fn rank(sort_text: &str) -> u8 {
    u8::from(letter(sort_text) == '#')
}

/// What was typed, read for matching: folded, and trimmed.
pub(crate) fn needle(typed: &str) -> String {
    // Trim ignored NULs along with spaces so neither can hide the other.
    fold(typed.trim_matches(|c: char| c.is_whitespace() || c == '\0'))
}

/// Whether `haystack`, folded, matches `needle`: contains it, or for a
/// needle of one or two characters, has a word that starts with it.
pub(crate) fn matches(haystack: &str, needle: &str) -> bool {
    let folded = fold(haystack);
    if needle.chars().count() >= 3 {
        return folded.contains(needle);
    }
    folded
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.starts_with(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_takes_away_case_and_accents() {
        assert_eq!(needle("\0   \0"), "");
        assert_eq!(needle("\0  t\0in  \0"), "tin");
        assert_eq!(fold("Éclair Brûlée"), "eclair brulee");
        assert_eq!(fold("MÖTLEY"), "motley");
        assert_eq!(fold("ﬁne"), "fine");
        // Letters with no decomposition, as the resolver folds them.
        assert_eq!(fold("Røyksopp Æther"), "royksopp aether");
        // A mark that makes another letter in another script stays.
        assert_eq!(fold("Й"), "й");
    }

    #[test]
    fn a_name_is_filed_without_its_article_and_its_punctuation() {
        assert_eq!(sort_text("The Beatles", None), "beatles");
        assert_eq!(
            sort_text("A Tribe Called Quest", None),
            "tribe called quest"
        );
        assert_eq!(sort_text("The The", None), "the");
        assert_eq!(sort_text("The", None), "the");
        assert_eq!(sort_text("AC/DC", None), "ac dc");
        assert_eq!(sort_text("“Heroes”", None), "heroes");
        assert_eq!(sort_text("Beatles", Some("Beatles, The")), "beatles the");
        // A sort name that repeats the name, as a scan writes it when the
        // tags have none, is no sort name.
        assert_eq!(
            sort_text("A Perfect Circle", Some("A Perfect Circle")),
            "perfect circle"
        );
        // An article is a word of its own.
        assert_eq!(sort_text("A.C. Newman", None), "a c newman");
        assert_eq!(sort_text("A-ha", None), "a ha");
        assert_eq!(sort_text("Anberlin", None), "anberlin");
        assert_eq!(sort_text("!!!", None), "!!!");
        assert_eq!(sort_text("Édith Piaf", None), "edith piaf");
    }

    #[test]
    fn letters_run_a_to_z_and_the_rest_go_under_hash_after_z() {
        assert_eq!(letter("edith piaf"), 'E');
        assert_eq!(letter("10 000 maniacs"), '#');
        assert_eq!(letter("!!!"), '#');
        assert_eq!(letter("ナンバーガール"), '#');
        assert!(rank("zz top") < rank("10cc"));
    }

    #[test]
    fn three_characters_or_more_match_anywhere_and_fewer_match_a_word_start() {
        assert!(matches("Midnight City", &needle("NIGHT")));
        assert!(matches("Sigur Rós", &needle("ros")));
        assert!(matches("Für Elise", &needle("fur")));
        assert!(matches("U2", &needle("u2")));
        assert!(matches("Live at U2's", &needle("U2")));
        assert!(!matches("Bu2b", &needle("u2")));
        assert!(matches("É", &needle("e")));
        assert!(!matches("Sea", &needle("e")));
    }
}
