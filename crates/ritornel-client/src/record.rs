//! What a client says of the listening record: for each warning, what is
//! wrong and what the person can do about it; for each note, what happened
//! and what Ritornel does about it. Every client says the same words; each
//! says since when in its own way, and names its own ways to dismiss, to
//! restart the server and to sign in to Last.fm again.

use ritornel_contract::{RecordProblem, RecordProblemKind};

/// How this client says what the person does: dismiss, and restart the
/// server, which for the desktop app is quitting it and opening it again.
#[derive(Debug, Clone, Copy)]
pub struct Words<'a> {
    /// "Dismiss" for a button, or a command to run.
    pub dismiss: &'a str,
    /// "restart the server", or "quit and reopen Ritornel".
    pub restart: &'a str,
    /// How to sign in to Last.fm again, as a whole sentence: where in
    /// Settings, or the command to run.
    pub sign_in: &'a str,
}

/// What is wrong, or what happened, in a sentence that starts with a
/// capital and has no full stop.
pub fn what(problem: &RecordProblem) -> String {
    let n = problem.count;
    let many = |one: &str, more: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{} {more}", thousands(n))
        }
    };
    match problem.kind {
        RecordProblemKind::ListenUnsaved => "The listen playing can’t be saved".to_owned(),
        RecordProblemKind::ListensUndelivered => {
            format!(
                "{} reached your history",
                many("listen hasn’t", "listens haven’t")
            )
        }
        RecordProblemKind::ScrobblesUnknown => format!(
            "{} not have reached Last.fm",
            many("scrobble may", "scrobbles may")
        ),
        RecordProblemKind::ScrobblesWithdrawn => {
            format!("{} sent to Last.fm", many("play wasn’t", "plays weren’t"))
        }
        RecordProblemKind::ScrobblesRefused => "Last.fm is refusing scrobbles".to_owned(),
        RecordProblemKind::SignInNeeded => "Last.fm needs you to sign in again".to_owned(),
        RecordProblemKind::ImportFailed if n == 1 => "Importing from Last.fm failed".to_owned(),
        RecordProblemKind::ImportFailed => {
            format!(
                "Importing from Last.fm failed {} times in a row",
                thousands(n)
            )
        }
        RecordProblemKind::Unreadable => {
            "Ritornel can’t tell whether every listen is being recorded".to_owned()
        }
    }
}

/// For a warning, what the person can do about it; for a note, what
/// Ritornel does about it, and why nothing is lost. Whole sentences.
pub fn then(problem: &RecordProblem, words: Words<'_>) -> String {
    let Words {
        dismiss,
        restart,
        sign_in,
    } = words;
    let one = problem.count == 1;
    match problem.kind {
        RecordProblemKind::ListenUnsaved => "Check that the disk Ritornel’s data is on has free \
             space. Ritornel keeps trying, and saves the listen as soon as it can."
            .to_owned(),
        RecordProblemKind::ListensUndelivered => "Ritornel keeps trying. If this lasts, check \
             that the disk Ritornel’s data is on has free space, and look in the server’s log."
            .to_owned(),
        RecordProblemKind::ScrobblesUnknown if one => format!(
            "The play is still in your history here. If the next import from Last.fm finds it \
             there, this note clears; Ritornel doesn’t send it again. {dismiss} sets every \
             scrobble note and warning aside."
        ),
        RecordProblemKind::ScrobblesUnknown => format!(
            "The plays are still in your history here. Those the next import from Last.fm finds \
             there clear; Ritornel doesn’t send the others again. {dismiss} sets every scrobble note and warning aside."
        ),
        RecordProblemKind::ScrobblesWithdrawn if one => format!(
            "It’s still in your history here. Ritornel chose not to send it, for the reason \
             given. {dismiss} sets every scrobble note and warning aside."
        ),
        RecordProblemKind::ScrobblesWithdrawn => format!(
            "They’re still in your history here. Ritornel chose not to send them, for the \
             reason given for the latest. {dismiss} sets every scrobble note and warning aside."
        ),
        RecordProblemKind::ScrobblesRefused => format!(
            "Check the API key and secret Ritornel uses, in lastfm.env in its data folder, then \
             {restart}. It clears by itself once Last.fm takes a scrobble again. Refused \
             scrobbles aren’t sent again; {dismiss} sets every scrobble note and warning aside."
        ),
        RecordProblemKind::SignInNeeded => format!(
            "Last.fm no longer accepts Ritornel’s sign-in, so nothing is scrobbled. {sign_in} \
             Signed in to the same account, Ritornel sends the plays waiting since; they stay \
             in your history here either way."
        ),
        RecordProblemKind::ImportFailed => format!(
            "Ritornel tries again at the next import. If it keeps failing, check the internet \
             connection, and the API key and secret in lastfm.env in Ritornel’s data folder, \
             then {restart}."
        ),
        RecordProblemKind::Unreadable => format!(
            "Check that the disk Ritornel’s data is on is connected and has free space, then \
             {restart}."
        ),
    }
}

/// A number with a comma between each three digits: 241,920.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(kind: RecordProblemKind, count: u64) -> RecordProblem {
        RecordProblem {
            kind,
            since: 0,
            count,
            detail: None,
            dismissable: false,
        }
    }

    const APP: Words<'static> = Words {
        dismiss: "Dismiss",
        restart: "quit and reopen Ritornel",
        sign_in: "Sign in again in Settings → Last.fm.",
    };

    #[test]
    fn each_kind_says_what_is_wrong_in_plain_words() {
        assert_eq!(
            what(&problem(RecordProblemKind::ListensUndelivered, 241_920)),
            "241,920 listens haven’t reached your history"
        );
        assert_eq!(
            what(&problem(RecordProblemKind::ScrobblesUnknown, 1)),
            "1 scrobble may not have reached Last.fm"
        );
        assert_eq!(
            what(&problem(RecordProblemKind::ImportFailed, 3)),
            "Importing from Last.fm failed 3 times in a row"
        );
    }

    #[test]
    fn each_client_names_its_own_dismiss_and_restart() {
        let refused = then(&problem(RecordProblemKind::ScrobblesRefused, 1), APP);
        assert!(
            refused.contains("then quit and reopen Ritornel."),
            "{refused}"
        );
        assert!(
            refused.ends_with("Dismiss sets every scrobble note and warning aside."),
            "{refused}"
        );
        let cli = Words {
            dismiss: "`ritornelctl status --dismiss`",
            restart: "restart the server",
            sign_in: "Sign in again with `ritornelctl lastfm signin`.",
        };
        assert!(
            then(&problem(RecordProblemKind::Unreadable, 1), cli)
                .ends_with("then restart the server.")
        );
        let sign_in = then(&problem(RecordProblemKind::SignInNeeded, 1), cli);
        assert!(
            sign_in.contains("`ritornelctl lastfm signin`. Signed in to the same account"),
            "{sign_in}"
        );
    }

    #[test]
    fn a_note_says_the_play_is_still_in_the_history_whatever_the_count() {
        for kind in [
            RecordProblemKind::ScrobblesUnknown,
            RecordProblemKind::ScrobblesWithdrawn,
        ] {
            for count in [1, 2] {
                let said = then(&problem(kind, count), APP);
                assert!(said.contains("still in your history here"), "{said}");
                assert!(
                    said.ends_with("Dismiss sets every scrobble note and warning aside."),
                    "{said}"
                );
            }
        }
    }
}
