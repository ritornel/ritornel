//! Resolves a controller's view before a request leaves its hub.
//! See "Routing playback and queue calls" in docs/internals/contract.md.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ritornel_contract::methods::{Method, PlaybackPlay, QueueAdd};
use ritornel_contract::{ErrorCode, Frame, Id, PlaySource, Request, RequestId, RpcError};
use ritornel_engine::{Fingerprint, playback::Catalog};
use serde_json::{Value, json};

use super::playback::{MAX_VIEW_FROM_HUB, refused};
use super::{Core, MAX_MESSAGE_BYTES, fingerprint, lock, read_params};
use crate::session::Session;

const REWRITE_TTL: Duration = Duration::from_secs(3 * 60);
const MAX_REWRITES: usize = 32;
const REWRITE_BYTES: usize = 2 * MAX_MESSAGE_BYTES;

#[derive(Debug)]
struct Rewrite {
    player: Id,
    key: Id,
    original: Fingerprint,
    params: Box<str>,
    created: Instant,
}

/// A session owns its rewrites just as it owns its views. Only the written
/// params are kept, with bounds on both their total bytes and entry count.
#[derive(Debug, Default)]
pub(crate) struct Rewrites(VecDeque<Rewrite>);

impl Rewrites {
    fn get(
        &mut self,
        player: Id,
        key: Id,
        original: Fingerprint,
        now: Instant,
    ) -> Result<Option<Value>, RpcError> {
        self.0
            .retain(|r| now.duration_since(r.created) < REWRITE_TTL);
        let Some(saved) = self.0.iter().find(|r| r.player == player && r.key == key) else {
            return Ok(None);
        };
        if saved.original != original {
            return Err(RpcError::new(
                ErrorCode::IdempotencyMismatch,
                "this idempotency key was used before with other params",
            ));
        }
        serde_json::from_str(&saved.params)
            .map(Some)
            .map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))
    }

    fn insert(&mut self, rewrite: Rewrite) {
        let mut bytes = rewrite.params.len() + self.0.iter().map(|r| r.params.len()).sum::<usize>();
        while self.0.len() >= MAX_REWRITES || bytes > REWRITE_BYTES {
            let Some(oldest) = self.0.pop_front() else {
                break;
            };
            bytes -= oldest.params.len();
        }
        self.0.push_back(rewrite);
    }
}

/// Reserve room for the whole JSON-RPC envelope, including the longest
/// request ID the presence session can assign after the rewrite.
fn written_params(method: &str, params: &Value) -> Result<Box<str>, RpcError> {
    let envelope = Frame::Request(Request {
        id: RequestId::Number(i64::MAX),
        method: method.into(),
        params: Some(Value::Null),
    })
    .encode();
    let written = params.to_string().into_boxed_str();
    if written.len() + envelope.len() - "null".len() > MAX_MESSAGE_BYTES {
        return Err(RpcError::new(
            ErrorCode::Invalid,
            "the routed player call exceeds the message size limit",
        ));
    }
    Ok(written)
}

impl Core {
    pub(super) fn prepare_player_call(
        &self,
        session: &Session,
        target: Id,
        method: &str,
        params: Option<Value>,
    ) -> Result<Option<Value>, RpcError> {
        if !matches!(method, PlaybackPlay::NAME | QueueAdd::NAME) {
            return Ok(params);
        }
        self.require_online_player(target)?;
        let (key, source, start, original) = if method == PlaybackPlay::NAME {
            let p = read_params::<PlaybackPlay>(params.clone())?;
            let original = fingerprint::<PlaybackPlay>(&p)?;
            (p.idempotency_key, p.source, p.start, original)
        } else {
            let p = read_params::<QueueAdd>(params.clone())?;
            let original = fingerprint::<QueueAdd>(&p)?;
            (p.idempotency_key, p.source, None, original)
        };
        // The typed source above requires an object. Never pass a controller's
        // claim that its own IDs were checked through to the player.
        let mut params = params.unwrap_or_default();
        let fields = params
            .as_object_mut()
            .ok_or_else(|| RpcError::new(ErrorCode::Invalid, "a player call needs an object"))?;
        fields.remove("tracksKnown");
        let mut rewrites = lock(&session.rewrites);
        if let Some(saved) = rewrites.get(target, key, original, Instant::now())? {
            return Ok(Some(saved));
        }
        match source {
            PlaySource::View { view } => {
                let (tracks, _, moved) = self
                    .with_stores(|stores| {
                        Self::songs_from_view(
                            stores,
                            session,
                            &view,
                            start.map(|n| n as usize),
                            Some(MAX_VIEW_FROM_HUB),
                        )
                    })
                    .ok_or_else(|| {
                        RpcError::new(ErrorCode::Unavailable, "the server is stopping")
                    })??;
                fields.insert("source".into(), json!(PlaySource::Tracks { ids: tracks }));
                if let Some(moved) = moved {
                    fields.insert("start".into(), json!(moved));
                }
                if fields.get("context").is_none_or(Value::is_null) {
                    fields.insert("context".into(), json!({"kind":"query"}));
                }
                fields.insert("tracksKnown".into(), Value::Bool(true));
                rewrites.insert(Rewrite {
                    player: target,
                    key,
                    original,
                    params: written_params(method, &params)?,
                    created: Instant::now(),
                });
            }
            PlaySource::Tracks { ids } => {
                // If a row has gone since a first attempt, the player can
                // still replay its receipt before validating the IDs afresh.
                if self
                    .with_library(|_, library| library.unknown_appearance(&ids))
                    .transpose()
                    .map_err(refused)?
                    == Some(None)
                {
                    fields.insert("tracksKnown".into(), Value::Bool(true));
                    written_params(method, &params)?;
                }
            }
            PlaySource::Release { .. } => {}
        }
        Ok(Some(params))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> Id {
        Id::from_bytes([byte; 16])
    }

    #[test]
    fn rewrites_expire_and_evict_the_oldest_at_either_bound() {
        let now = Instant::now();
        let original = Fingerprint::of("queue.add", b"view");
        let rewrite = |key, size| Rewrite {
            player: id(1),
            key: id(key),
            original,
            params: json!("a".repeat(size)).to_string().into(),
            created: now,
        };
        let mut saved = Rewrites::default();
        for key in 0..=MAX_REWRITES {
            saved.insert(rewrite(key as u8, 1));
        }
        assert_eq!(saved.get(id(1), id(0), original, now).unwrap(), None);
        assert_eq!(
            saved.get(id(1), id(1), original, now).unwrap(),
            Some(json!("a"))
        );
        assert_eq!(saved.get(id(2), id(1), original, now).unwrap(), None);

        for key in [40, 41, 42] {
            saved.insert(rewrite(key, MAX_MESSAGE_BYTES - 100));
        }
        assert_eq!(saved.0.len(), 2);
        assert_eq!(saved.get(id(1), id(40), original, now).unwrap(), None);
        assert_eq!(
            saved
                .get(id(1), id(41), original, now + REWRITE_TTL)
                .unwrap(),
            None
        );
        assert!(saved.0.is_empty());
    }

    #[test]
    fn twenty_thousand_ids_fit_and_the_envelope_counts_toward_the_limit() {
        let params = json!({"idempotencyKey":id(1),"source":{"type":"tracks","ids":vec![id(2); MAX_VIEW_FROM_HUB]},"context":{"kind":"query"},"tracksKnown":true});
        assert!(written_params(PlaybackPlay::NAME, &params).is_ok());
        let mut params = json!({"extra":""});
        let envelope = |params: Value| {
            Frame::Request(Request {
                id: RequestId::Number(i64::MAX),
                method: QueueAdd::NAME.into(),
                params: Some(params),
            })
            .encode()
            .len()
        };
        let padding = MAX_MESSAGE_BYTES - envelope(params.clone());
        params["extra"] = json!("x".repeat(padding));
        assert!(written_params(QueueAdd::NAME, &params).is_ok());
        params["extra"] = json!("x".repeat(padding + 1));
        assert_eq!(
            written_params(QueueAdd::NAME, &params).unwrap_err().kind(),
            ErrorCode::Invalid
        );
    }
}
