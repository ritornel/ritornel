//! Where a typed name is in a name found, for the lozenge to show the part
//! typed in the ink and the rest quieter: the real client's `walls/find.ts`,
//! folding as the server folds (case, accents and punctuation do not count).

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// A name cut where what was typed is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cut {
    pub before: String,
    pub typed: String,
    pub after: String,
}

/// The Latin letters with no decomposition, as the server reads them.
fn letter(character: char) -> Option<&'static str> {
    Some(match character {
        'ø' => "o",
        'ł' => "l",
        'đ' | 'ð' => "d",
        'ı' => "i",
        'æ' => "ae",
        'œ' => "oe",
        'ß' => "ss",
        'þ' => "th",
        _ => return None,
    })
}

fn latin(character: char) -> bool {
    matches!(character, 'a'..='z' | 'À'..='ɏ' | 'Ḁ'..='ỿ')
}

/// One character with case and accents taken away; a mark on a letter of
/// another script stays.
fn fold(character: char) -> String {
    let mut folded = String::new();
    let mut is_latin = false;
    let lower: String = character
        .to_string()
        .nfkc()
        .collect::<String>()
        .to_lowercase();
    for each in lower.nfd() {
        if is_combining_mark(each) {
            if !is_latin {
                folded.push(each);
            }
            continue;
        }
        is_latin = latin(each);
        match letter(each) {
            Some(plain) => folded.push_str(plain),
            None => folded.push(each),
        }
    }
    folded.nfc().collect()
}

struct Word {
    /// Where it starts in the name, in bytes.
    start: usize,
    /// The word folded.
    text: String,
    /// For each byte of `text`, where the character it came from ends in the name.
    ends: Vec<usize>,
}

/// The words of `text`, folded: anything that is not a letter or a digit is
/// a space between them.
fn words(text: &str) -> Vec<Word> {
    let mut found = Vec::new();
    let mut word: Option<Word> = None;
    for (at, character) in text.char_indices() {
        let folded = fold(character);
        let end = at + character.len_utf8();
        if folded.chars().any(char::is_alphanumeric) {
            let current = word.get_or_insert_with(|| Word {
                start: at,
                text: String::new(),
                ends: Vec::new(),
            });
            current.text.push_str(&folded);
            current.ends.extend(std::iter::repeat_n(end, folded.len()));
        } else if let Some(done) = word.take() {
            found.push(done);
        }
    }
    found.extend(word);
    found
}

/// Where `typed` is in `name`: from the start of a word, its words in turn,
/// the last of them begun. "led z" is in "Led Zeppelin", "beat" in "The
/// Beatles" and "ac dc" in "AC/DC". `None` when it is not there.
pub(crate) fn cut(name: &str, typed: &str) -> Option<Cut> {
    let wanted: Vec<String> = words(typed).into_iter().map(|word| word.text).collect();
    let last_wanted = wanted.last()?;
    let text: String = name.nfc().collect();
    let have = words(&text);
    for first in 0..=have.len().saturating_sub(wanted.len()) {
        let Some(run) = have.get(first..first + wanted.len()) else {
            break;
        };
        let fits = run
            .iter()
            .zip(&wanted)
            .enumerate()
            .all(|(at, (word, want))| {
                if at == wanted.len() - 1 {
                    word.text.starts_with(want.as_str())
                } else {
                    &word.text == want
                }
            });
        if !fits {
            continue;
        }
        let (Some(first_word), Some(last_word)) = (run.first(), run.last()) else {
            continue;
        };
        let end = *last_word.ends.get(last_wanted.len().checked_sub(1)?)?;
        let start = first_word.start;
        return Some(Cut {
            before: text.get(..start)?.to_owned(),
            typed: text.get(start..end)?.to_owned(),
            after: text.get(end..)?.to_owned(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real client's tests (`walls/find.test.ts`), as `before[typed]after`.
    fn read(name: &str, typed: &str) -> Option<String> {
        cut(name, typed).map(|cut| format!("{}[{}]{}", cut.before, cut.typed, cut.after))
    }

    #[test]
    fn is_at_the_start_of_a_word_case_not_counting() {
        assert_eq!(
            read("Led Zeppelin", "led z").as_deref(),
            Some("[Led Z]eppelin")
        );
        assert_eq!(read("Radiohead", "r").as_deref(), Some("[R]adiohead"));
        assert_eq!(read("Pink Floyd", "pink ").as_deref(), Some("[Pink] Floyd"));
    }

    #[test]
    fn may_be_past_an_article_the_name_is_filed_without() {
        assert_eq!(
            read("The Beatles", "beat").as_deref(),
            Some("The [Beat]les")
        );
        assert_eq!(
            read("The Beatles", "the b").as_deref(),
            Some("[The B]eatles")
        );
    }

    #[test]
    fn does_not_count_accents_or_punctuation() {
        assert_eq!(
            read("Sigur Rós", "sigur ro").as_deref(),
            Some("[Sigur Ró]s")
        );
        assert_eq!(read("Røyksopp", "royk").as_deref(), Some("[Røyk]sopp"));
        assert_eq!(read("AC/DC", "ac dc").as_deref(), Some("[AC/DC]"));
        assert_eq!(
            read("Guns N’ Roses", "guns n' r").as_deref(),
            Some("[Guns N’ R]oses")
        );
    }

    #[test]
    fn is_in_a_name_filed_another_way_round() {
        assert_eq!(
            read("David Bowie", "bowie").as_deref(),
            Some("David [Bowie]")
        );
    }

    #[test]
    fn is_nowhere_when_the_name_does_not_have_it() {
        assert_eq!(read("Led Zeppelin", "led zx"), None);
        assert_eq!(read("Radiohead", "head"), None);
        assert_eq!(read("Led Zeppelin", " "), None);
    }

    #[test]
    fn finds_a_year() {
        assert_eq!(read("1994", "199").as_deref(), Some("[199]4"));
    }
}
