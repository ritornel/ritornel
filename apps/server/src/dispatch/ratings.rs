//! `ratings.set`: the listener's stars for a recording. See flags 5 and 6
//! in docs/design/schema.md.

use ritornel_contract::methods::{Method, RatingsSet};
use ritornel_contract::{ErrorCode, RatingSet, RatingSetParams, Receipt, ReceiptStatus, RpcError};
use ritornel_engine::{Command, WriteRequest};

use super::playback::refused;
use super::{Core, Stores, fingerprint, revision};

impl Core {
    pub(super) fn ratings_set(
        &self,
        stores: &Stores,
        params: &RatingSetParams,
    ) -> Result<RatingSet, RpcError> {
        let key = params.idempotency_key;
        let request = WriteRequest {
            method: RatingsSet::NAME,
            command: Some(Command {
                id: key,
                fingerprint: fingerprint::<RatingsSet>(params)?,
            }),
            client: self.client(),
        };
        let rating = params.rating;
        let written = Self::library(stores, |library| {
            library
                .set_rating(
                    request,
                    params.recording_id,
                    rating,
                    move |rev, recording| {
                        let set = RatingSet {
                            receipt: Receipt {
                                command_id: key,
                                status: ReceiptStatus::Done,
                                revision: Some(revision(rev)),
                            },
                            recording_id: recording,
                            rating,
                        };
                        // These few fields always write.
                        serde_json::to_string(&set).unwrap_or_default()
                    },
                )
                .map_err(refused)
        })?;
        // The player bar shows the playing song's stars.
        if !written.replayed {
            self.said_again(stores);
        }
        serde_json::from_str(&written.result_json).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    RatingsSet::NAME
                ),
            )
        })
    }
}
