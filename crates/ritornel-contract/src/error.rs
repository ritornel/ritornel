//! The error model: a JSON-RPC error whose `data` is `{code, retryable,
//! details}`. See "Errors and capabilities" in docs/internals/contract.md.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What went wrong, from a closed set. A client decides what to do by this
/// and never by the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The call is wrong: its frame, its params, or its place in the
    /// session. The client fixes the call.
    Invalid,
    /// No such ID. The client drops it.
    NotFound,
    /// `expectedRevision` did not match, and `details` has the current one.
    /// The client reads again and decides again.
    Conflict,
    /// The plan is no longer valid. The client plans again.
    PlanStale,
    /// The view is no longer valid. The client opens it again.
    ViewExpired,
    /// The thing exists but cannot be reached now. The client shows a calm
    /// state and tries later.
    Unavailable,
    /// The call needs a `full` session. The client hides or explains.
    Forbidden,
    /// This server lacks the method or the capability. The client hides the
    /// feature.
    Unsupported,
    /// A key was reused with different params: a bug in the client.
    IdempotencyMismatch,
    /// A bug in the server.
    Internal,
}

impl ErrorCode {
    /// Every code, in the order "Errors and capabilities" in
    /// docs/internals/contract.md lists them.
    pub const ALL: &[Self] = &[
        Self::Invalid,
        Self::NotFound,
        Self::Conflict,
        Self::PlanStale,
        Self::ViewExpired,
        Self::Unavailable,
        Self::Forbidden,
        Self::Unsupported,
        Self::IdempotencyMismatch,
        Self::Internal,
    ];

    /// The code as it is written on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::PlanStale => "plan_stale",
            Self::ViewExpired => "view_expired",
            Self::Unavailable => "unavailable",
            Self::Forbidden => "forbidden",
            Self::Unsupported => "unsupported",
            Self::IdempotencyMismatch => "idempotency_mismatch",
            Self::Internal => "internal",
        }
    }

    /// Whether the same call, sent again unchanged, may succeed later.
    /// Only `unavailable` is like that: every other code needs the client
    /// to change something or to give up.
    pub const fn retryable(self) -> bool {
        matches!(self, Self::Unavailable)
    }

    /// The JSON-RPC 2.0 number that goes beside this code. The contract's
    /// own failures share -32000, the first of the range JSON-RPC leaves to
    /// the application; the frame-level failures have their own numbers in
    /// [`RpcError`]'s constructors.
    const fn rpc_code(self) -> i32 {
        match self {
            Self::Invalid => INVALID_PARAMS,
            Self::Unsupported => METHOD_NOT_FOUND,
            Self::Internal => INTERNAL_ERROR,
            _ => APPLICATION_ERROR,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
const APPLICATION_ERROR: i32 = -32000;

/// The `data` of an error: what a client acts on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    /// What went wrong.
    pub code: ErrorCode,
    /// Whether sending the same call again later may succeed.
    pub retryable: bool,
    /// What the client needs to act on the error, by code. It is an empty
    /// object when there is nothing to add.
    pub details: Map<String, Value>,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct RpcError {
    /// The JSON-RPC number. It says less than `data.code` and nothing
    /// should be decided by it.
    pub code: i32,
    /// For logs and for a person reading them. Never for parsing.
    pub message: String,
    /// What a client acts on.
    pub data: ErrorData,
}

impl RpcError {
    /// An error with this code and message, and no details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::numbered(code.rpc_code(), code, message)
    }

    fn numbered(rpc_code: i32, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: rpc_code,
            message: message.into(),
            data: ErrorData {
                code,
                retryable: code.retryable(),
                details: Map::new(),
            },
        }
    }

    /// The text was not JSON.
    pub fn parse_error() -> Self {
        Self::numbered(PARSE_ERROR, ErrorCode::Invalid, "the frame is not JSON")
    }

    /// The JSON was not a frame of this contract.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::numbered(INVALID_REQUEST, ErrorCode::Invalid, message)
    }

    /// Adds one entry to `details`.
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.data.details.insert(key.to_owned(), value.into());
        self
    }

    /// The contract's code: what a client acts on.
    pub fn kind(&self) -> ErrorCode {
        self.data.code
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.data.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_codes_are_the_closed_set_in_snake_case() {
        let written: Vec<_> = ErrorCode::ALL
            .iter()
            .map(|code| serde_json::to_value(code).unwrap())
            .collect();
        assert_eq!(
            written,
            [
                "invalid",
                "not_found",
                "conflict",
                "plan_stale",
                "view_expired",
                "unavailable",
                "forbidden",
                "unsupported",
                "idempotency_mismatch",
                "internal"
            ]
        );
        for code in ErrorCode::ALL {
            assert_eq!(json!(code.as_str()), serde_json::to_value(code).unwrap());
            assert_eq!(
                serde_json::from_value::<ErrorCode>(json!(code.as_str())).unwrap(),
                *code
            );
        }
        assert!(serde_json::from_value::<ErrorCode>(json!("transport")).is_err());
    }

    #[test]
    fn an_error_is_code_retryable_and_details_under_data() {
        let error = RpcError::new(ErrorCode::Conflict, "the queue moved")
            .with_detail("currentRevision", 42);
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({
                "code": -32000,
                "message": "the queue moved",
                "data": {
                    "code": "conflict",
                    "retryable": false,
                    "details": { "currentRevision": 42 },
                },
            })
        );
        assert_eq!(error.kind(), ErrorCode::Conflict);
        assert_eq!(error.to_string(), "conflict: the queue moved");
    }

    #[test]
    fn only_unavailable_is_worth_sending_again() {
        for code in ErrorCode::ALL {
            assert_eq!(
                RpcError::new(*code, "").data.retryable,
                *code == ErrorCode::Unavailable,
                "{code}"
            );
        }
    }

    #[test]
    fn the_json_rpc_number_follows_the_code() {
        let number = |code| RpcError::new(code, "").code;
        assert_eq!(number(ErrorCode::Invalid), -32602);
        assert_eq!(number(ErrorCode::Unsupported), -32601);
        assert_eq!(number(ErrorCode::Internal), -32603);
        assert_eq!(number(ErrorCode::Forbidden), -32000);
        assert_eq!(number(ErrorCode::Unavailable), -32000);
        assert_eq!(RpcError::parse_error().code, -32700);
        assert_eq!(RpcError::parse_error().kind(), ErrorCode::Invalid);
        assert_eq!(RpcError::invalid_request("no method").code, -32600);
    }
}
