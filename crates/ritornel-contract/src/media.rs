//! Media: covers over plain HTTP, and the token that a request which
//! cannot carry a header presents. See "Media" in
//! docs/internals/contract.md.
//!
//! A cover is `GET /v1/art/{artId}/{size}`, where `artId` is a row's art ID
//! and `size` is one of [`ART_SIZES`], the buckets the descriptor lists. The
//! answer is a JPEG at most that many pixels on its longer side, sent as
//! immutable: a changed image is a new art ID. A native client presents the
//! credential in an `Authorization` header. A page cannot set one on an
//! `<img>`, so it asks for a [`MediaToken`] over the socket and puts it in
//! the query string:
//!
//! ```text
//! GET /v1/art/<artId>/256?token=<token>
//! ```
//!
//! The media token opens media routes and no other. It is never the session
//! token and never a ticket, and it stays the same for as long as its
//! session lasts, so that the URLs a page built stay the same too.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The name of the query parameter that carries a media token on a media
/// route.
pub const MEDIA_TOKEN_PARAM: &str = "token";

/// The sizes a cover is served at, in pixels on its longer side: what
/// `{size}` in the route may be. A client asks for the smallest that is at
/// least what it draws, so that caches hit.
pub const ART_SIZES: &[u32] = &[64, 96, 128, 192, 256, 384, 512, 768, 1024];

/// The result of `media.token`: what a page puts in the query string of a
/// media URL.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MediaToken {
    /// The token. It reads media and nothing else, and works until the
    /// session that asked for it ends.
    pub token: String,
}

/// The token reads media, so it is not printed.
impl fmt::Debug for MediaToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaToken")
            .field("token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_media_token_is_its_text_and_debug_does_not_print_it() {
        let token = MediaToken {
            token: "SENTINEL_MEDIA_TOKEN".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&token).unwrap(),
            json!({ "token": "SENTINEL_MEDIA_TOKEN" })
        );
        assert!(!format!("{token:?}").contains("SENTINEL"));
    }

    #[test]
    fn the_sizes_run_from_64_to_1024_in_order() {
        assert_eq!(ART_SIZES.first(), Some(&64));
        assert_eq!(ART_SIZES.last(), Some(&1024));
        assert!(ART_SIZES.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
