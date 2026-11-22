//! What a client says of each thing that stops the listening record: what
//! is wrong, and what the person can do about it. Every client says the
//! same words; each says since when in its own way, and names its own way
//! to dismiss.

use ritornel_contract::{RecordProblem, RecordProblemKind};

/// What is wrong, in a sentence that starts with a capital and has no
/// full stop.
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
        RecordProblemKind::ScrobblesWithdrawn => format!(
            "{} sent to Last.fm",
            many("scrobble wasn’t", "scrobbles weren’t")
        ),
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

/// What the person can do about it, in whole sentences. `dismiss` is how
/// this client dismisses, such as "Dismiss" for a button or a command to
/// run.
pub fn what_to_do(kind: RecordProblemKind, dismiss: &str) -> String {
    match kind {
        RecordProblemKind::ListenUnsaved => "Check that the disk Ritornel’s data is on has free \
             space. Ritornel keeps trying, and saves the listen as soon as it can."
            .to_owned(),
        RecordProblemKind::ListensUndelivered => "Ritornel keeps trying. If this lasts, check \
             that the disk Ritornel’s data is on has free space, and look in the server’s log."
            .to_owned(),
        RecordProblemKind::ScrobblesUnknown => format!(
            "Nothing to do yet: the next import from Last.fm clears those that arrived. Ritornel \
             doesn’t send the rest again; they stay in your history here. {dismiss} sets them \
             aside."
        ),
        RecordProblemKind::ScrobblesWithdrawn => format!(
            "Nothing to do: their plays stopped counting, or grew too old for Last.fm, before \
             they could be sent. They stay in your history here. {dismiss} sets them aside."
        ),
        RecordProblemKind::ScrobblesRefused => format!(
            "Check the API key and secret Ritornel uses, in lastfm.env in its data folder, then \
             restart the server. It clears by itself once Last.fm takes a scrobble again. \
             Refused scrobbles aren’t sent again; {dismiss} sets them aside."
        ),
        RecordProblemKind::SignInNeeded => "Last.fm no longer accepts Ritornel’s sign-in, so \
             nothing is scrobbled. Sign in again, in Settings → Last.fm or with `ritornelctl \
             lastfm signin`, then turn scrobbling back on. Plays from before then stay in your \
             history here."
            .to_owned(),
        RecordProblemKind::ImportFailed => "Ritornel tries again at the next import. If it \
             keeps failing, check the internet connection, and the API key and secret in \
             lastfm.env in Ritornel’s data folder, then restart the server."
            .to_owned(),
        RecordProblemKind::Unreadable => "Check that the disk Ritornel’s data is on is \
             connected and has free space, then restart the server."
            .to_owned(),
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
    fn what_to_do_names_this_clients_way_to_dismiss() {
        assert!(
            what_to_do(RecordProblemKind::ScrobblesRefused, "Dismiss")
                .ends_with("aren’t sent again; Dismiss sets them aside.")
        );
        assert!(
            what_to_do(
                RecordProblemKind::ScrobblesUnknown,
                "`ritornelctl status --dismiss`"
            )
            .ends_with("`ritornelctl status --dismiss` sets them aside.")
        );
    }
}
