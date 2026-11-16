//! What a client makes for a command before it sends it.

use std::time::{SystemTime, UNIX_EPOCH};

use ritornel_contract::Id;

use crate::Error;

/// A new idempotency key: the ID a client gives a command.
///
/// Make one for each thing to be done, and keep it for as long as the
/// outcome is unknown: sending a command again with its key returns the
/// first result, and with a new key asks for the work a second time
/// (docs/design/contract.md, decision 13).
pub fn idempotency_key() -> Result<Id, Error> {
    let mut random = [0; 10];
    getrandom::fill(&mut random).map_err(|error| Error::NoRandom(error.to_string()))?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        });
    Ok(Id::new_v7(now_ms, random))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_key_is_a_new_uuid_version_7() {
        let (first, second) = (idempotency_key().unwrap(), idempotency_key().unwrap());
        assert_ne!(first, second);
        for key in [first, second] {
            let text = key.to_string();
            assert_eq!(text.as_bytes()[14], b'7', "{text}");
            assert!(
                matches!(text.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
                "{text}"
            );
        }
    }
}
