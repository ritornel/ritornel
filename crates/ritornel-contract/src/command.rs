//! Commands: the calls that change something lasting. See "Commands,
//! playback, and jobs" in docs/internals/contract.md.
//!
//! A command carries an `idempotencyKey`, a UUID its client makes, and
//! returns a [`Receipt`]. Sending the same key again with the same params
//! returns the same receipt and does the work no second time, so a client
//! that does not know what became of a command sends it again. The same key
//! with other params fails with `idempotency_mismatch`.

use serde::{Deserialize, Serialize};

use crate::Id;

/// What a command returns: that its intent was recorded, and what is known
/// of it so far. A command's own result has these fields and whatever else
/// the command returns.
///
/// The job a command started and the outcome of each item it covered join
/// with the first commands that have them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    /// The command's ID: the idempotency key it was sent with.
    pub command_id: Id,
    /// How far the command has got.
    pub status: ReceiptStatus,
    /// The library revision the command was committed at. It is left out
    /// for a command that changes no library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub revision: Option<u64>,
}

/// How far a command has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    /// The intent is recorded and the work is still to do or under way.
    Accepted,
    /// The work is done.
    Done,
}

/// The params of `library.create`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryCreateParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
}

/// The result of `library.create`: the receipt, and the library that now
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LibraryCreated {
    /// The receipt. Its revision is the library's first.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The new library's ID.
    pub library_id: Id,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_commands_result_is_its_receipt_and_what_else_it_returns_in_one_object() {
        let created = LibraryCreated {
            receipt: Receipt {
                command_id: Id::from_bytes([0xc0; 16]),
                status: ReceiptStatus::Done,
                revision: Some(1),
            },
            library_id: Id::from_bytes([0x1b; 16]),
        };
        let wire = json!({
            "commandId": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0",
            "status": "done",
            "revision": 1,
            "libraryId": "1b1b1b1b-1b1b-1b1b-1b1b-1b1b1b1b1b1b",
        });
        assert_eq!(serde_json::to_value(created).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<LibraryCreated>(wire.clone()).unwrap(),
            created
        );
        // The receipt alone is the same object without the rest.
        assert_eq!(
            serde_json::from_value::<Receipt>(wire).unwrap(),
            created.receipt
        );
    }

    #[test]
    fn a_receipt_leaves_out_a_revision_it_has_none_of() {
        let receipt = Receipt {
            command_id: Id::from_bytes([0xc0; 16]),
            status: ReceiptStatus::Accepted,
            revision: None,
        };
        let wire = json!({
            "commandId": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0",
            "status": "accepted",
        });
        assert_eq!(serde_json::to_value(receipt).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Receipt>(wire).unwrap(), receipt);
    }

    #[test]
    fn the_key_of_a_command_is_an_id_and_nothing_else_will_do() {
        let params = json!({ "idempotencyKey": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0" });
        assert_eq!(
            serde_json::from_value::<LibraryCreateParams>(params).unwrap(),
            LibraryCreateParams {
                idempotency_key: Id::from_bytes([0xc0; 16])
            }
        );
        for wrong in [
            json!({}),
            json!({ "idempotencyKey": "again" }),
            json!({ "idempotencyKey": 7 }),
        ] {
            assert!(serde_json::from_value::<LibraryCreateParams>(wrong).is_err());
        }
    }
}
