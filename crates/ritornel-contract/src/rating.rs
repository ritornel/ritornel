//! What the listener says of a song: stars, on the recording, so that every
//! appearance of it shows the same. See flags 5 and 6 in
//! docs/design/schema.md.

use serde::{Deserialize, Serialize};

use crate::{Id, Receipt};

/// The highest rating: five stars.
pub const MAX_RATING: u32 = 100;

/// The params of `ratings.set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RatingSetParams {
    /// The command's ID, made by the client.
    pub idempotency_key: Id,
    /// The recording.
    pub recording_id: Id,
    /// Its stars, 0 to 100, ten to the half star; `null` takes them away.
    pub rating: Option<u32>,
}

/// The result of `ratings.set`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RatingSet {
    /// The receipt, with the library revision of the write.
    #[serde(flatten)]
    pub receipt: Receipt,
    /// The recording, as it is now: one merged since is named by the one
    /// it was merged into.
    pub recording_id: Id,
    /// Its stars now.
    pub rating: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn taking_the_stars_away_is_a_rating_of_null() {
        let wire = json!({
            "idempotencyKey": "c0c0c0c0-c0c0-c0c0-c0c0-c0c0c0c0c0c0",
            "recordingId": "7e7e7e7e-7e7e-7e7e-7e7e-7e7e7e7e7e7e",
            "rating": null,
        });
        let params: RatingSetParams = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(params.rating, None);
        assert_eq!(serde_json::to_value(params).unwrap(), wire);
    }
}
