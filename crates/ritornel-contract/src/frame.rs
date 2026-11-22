//! Frames: JSON-RPC 2.0 requests, responses and notifications, one to a
//! message. Batches are not supported. See "Session and frames" in
//! docs/internals/contract.md.
//!
//! A frame is text on every transport. The socket carries it as a text
//! message and the in-memory transport as a string, so both go through
//! [`Frame::encode`] and [`Frame::decode`].

use serde_json::{Map, Value};

use crate::RpcError;

/// The `jsonrpc` member of every frame.
const VERSION: &str = "2.0";

/// What a client names a request by, and a response names its request by.
///
/// It is a string or an integer, and nothing else. JSON-RPC 2.0 also allows
/// `null` and a number with a fraction, and discourages both: a `null` ID
/// reads as a response to a request nobody could name, and a fraction does
/// not survive every reader. The Model Context Protocol, which is why this
/// contract uses JSON-RPC framing at all ("Session and frames" in
/// docs/internals/contract.md), requires a string or an integer and forbids
/// `null`. So a request
/// whose ID is `null`, `1.0` or `1.5` is refused as an invalid request, on
/// purpose, and a client must not send one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RequestId {
    /// A whole number. `1` is one; `1.0` is not.
    Number(i64),
    /// A string.
    Text(String),
}

impl RequestId {
    fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Number(number) => number.as_i64().map(Self::Number),
            Value::String(text) => Some(Self::Text(text)),
            _ => None,
        }
    }

    fn to_value(&self) -> Value {
        match self {
            Self::Number(number) => Value::from(*number),
            Self::Text(text) => Value::from(text.as_str()),
        }
    }
}

/// A call: it gets exactly one response, which carries its `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// The caller's name for this call.
    pub id: RequestId,
    /// The method, such as `server.hello`.
    pub method: String,
    /// The params, if the frame had any. The method says what type they
    /// are.
    pub params: Option<Value>,
}

/// A message that gets no response. The server sends events this way.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// The method. For an event it is [`EVENT_METHOD`](crate::EVENT_METHOD).
    pub method: String,
    /// The params, if the frame had any.
    pub params: Option<Value>,
}

/// The answer to a request.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The `id` of the request this answers. It is `None` only when the
    /// request was too broken to read an `id` from.
    pub id: Option<RequestId>,
    /// The result, or why there is none.
    pub outcome: Result<Value, RpcError>,
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// A call.
    Request(Request),
    /// A message that gets no response.
    Notification(Notification),
    /// The answer to a call.
    Response(Response),
}

/// Text that is not a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Malformed {
    /// The `id` the text carried, when that much could be read.
    pub id: Option<RequestId>,
    /// Why it is not a frame.
    pub error: RpcError,
}

impl Malformed {
    fn invalid(id: Option<RequestId>, why: &'static str) -> Self {
        Self {
            id,
            error: RpcError::invalid_request(why),
        }
    }

    /// What a server answers to it.
    pub fn into_response(self) -> Response {
        Response {
            id: self.id,
            outcome: Err(self.error),
        }
    }
}

/// The `id` member of a frame: missing, `null`, or an ID.
enum WireId {
    Absent,
    Null,
    Present(RequestId),
}

impl WireId {
    fn known(self) -> Option<RequestId> {
        match self {
            Self::Present(id) => Some(id),
            _ => None,
        }
    }
}

impl Frame {
    /// Reads one frame.
    pub fn decode(text: &str) -> Result<Self, Malformed> {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return Err(Malformed {
                id: None,
                error: RpcError::parse_error(),
            });
        };
        let mut object = match value {
            Value::Object(object) => object,
            Value::Array(_) => return Err(Malformed::invalid(None, "batches are not supported")),
            _ => return Err(Malformed::invalid(None, "a frame is a JSON object")),
        };

        let id = match object.remove("id") {
            None => WireId::Absent,
            Some(Value::Null) => WireId::Null,
            Some(value) => match RequestId::from_value(value) {
                Some(id) => WireId::Present(id),
                None => {
                    return Err(Malformed::invalid(
                        None,
                        "an id is a string or a whole number",
                    ));
                }
            },
        };
        if object.remove("jsonrpc") != Some(Value::from(VERSION)) {
            return Err(Malformed::invalid(id.known(), "jsonrpc must be \"2.0\""));
        }

        match object.remove("method") {
            Some(Value::String(method)) => {
                let params = object.remove("params");
                match id {
                    WireId::Absent => Ok(Self::Notification(Notification { method, params })),
                    WireId::Present(id) => Ok(Self::Request(Request { id, method, params })),
                    WireId::Null => Err(Malformed::invalid(None, "a request's id is not null")),
                }
            }
            Some(_) => Err(Malformed::invalid(id.known(), "method must be a string")),
            None => {
                let outcome = match (object.remove("result"), object.remove("error")) {
                    (Some(result), None) => Ok(result),
                    (None, Some(error)) => match serde_json::from_value(error) {
                        Ok(error) => Err(error),
                        Err(_) => {
                            return Err(Malformed::invalid(
                                id.known(),
                                "an error is {code, message, data}",
                            ));
                        }
                    },
                    _ => {
                        return Err(Malformed::invalid(
                            id.known(),
                            "a frame has a method, or a result, or an error",
                        ));
                    }
                };
                match id {
                    WireId::Absent => Err(Malformed::invalid(
                        None,
                        "a response names the request it answers",
                    )),
                    WireId::Null => Ok(Self::Response(Response { id: None, outcome })),
                    WireId::Present(id) => Ok(Self::Response(Response {
                        id: Some(id),
                        outcome,
                    })),
                }
            }
        }
    }

    /// Writes the frame as one line of JSON.
    pub fn encode(&self) -> String {
        let mut object = Map::new();
        object.insert("jsonrpc".to_owned(), Value::from(VERSION));
        match self {
            Self::Request(Request { id, method, params }) => {
                object.insert("id".to_owned(), id.to_value());
                call(&mut object, method, params.as_ref());
            }
            Self::Notification(Notification { method, params }) => {
                call(&mut object, method, params.as_ref());
            }
            Self::Response(Response { id, outcome }) => {
                let id = id.as_ref().map_or(Value::Null, RequestId::to_value);
                object.insert("id".to_owned(), id);
                match outcome {
                    Ok(result) => object.insert("result".to_owned(), result.clone()),
                    Err(error) => object.insert("error".to_owned(), error_value(error)),
                };
            }
        }
        Value::Object(object).to_string()
    }
}

fn call(object: &mut Map<String, Value>, method: &str, params: Option<&Value>) {
    object.insert("method".to_owned(), Value::from(method));
    if let Some(params) = params {
        object.insert("params".to_owned(), params.clone());
    }
}

/// An error as JSON. It is built by hand so that writing a frame cannot
/// fail.
fn error_value(error: &RpcError) -> Value {
    let mut data = Map::new();
    data.insert("code".to_owned(), Value::from(error.data.code.as_str()));
    data.insert("retryable".to_owned(), Value::from(error.data.retryable));
    data.insert(
        "details".to_owned(),
        Value::Object(error.data.details.clone()),
    );
    let mut object = Map::new();
    object.insert("code".to_owned(), Value::from(error.code));
    object.insert("message".to_owned(), Value::from(error.message.as_str()));
    object.insert("data".to_owned(), Value::Object(data));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::ErrorCode;

    fn value(frame: &Frame) -> Value {
        serde_json::from_str(&frame.encode()).unwrap()
    }

    fn round_trip(frame: &Frame, wire: &Value) {
        assert_eq!(&value(frame), wire);
        assert_eq!(&Frame::decode(&wire.to_string()).unwrap(), frame);
        assert_eq!(frame.encode().lines().count(), 1);
    }

    #[test]
    fn a_request_has_an_id_a_method_and_maybe_params() {
        round_trip(
            &Frame::Request(Request {
                id: RequestId::Number(7),
                method: "server.hello".to_owned(),
                params: Some(json!({ "name": "ritornelctl" })),
            }),
            &json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "server.hello",
                "params": { "name": "ritornelctl" },
            }),
        );
        round_trip(
            &Frame::Request(Request {
                id: RequestId::Text("a".to_owned()),
                method: "servers.list".to_owned(),
                params: None,
            }),
            &json!({ "jsonrpc": "2.0", "id": "a", "method": "servers.list" }),
        );
    }

    #[test]
    fn a_notification_is_a_request_with_no_id() {
        round_trip(
            &Frame::Notification(Notification {
                method: "event".to_owned(),
                params: Some(json!({ "topic": "servers" })),
            }),
            &json!({ "jsonrpc": "2.0", "method": "event", "params": { "topic": "servers" } }),
        );
    }

    #[test]
    fn a_response_carries_a_result_or_an_error() {
        round_trip(
            &Frame::Response(Response {
                id: Some(RequestId::Number(7)),
                outcome: Ok(json!({ "servers": [] })),
            }),
            &json!({ "jsonrpc": "2.0", "id": 7, "result": { "servers": [] } }),
        );
        round_trip(
            &Frame::Response(Response {
                id: None,
                outcome: Err(RpcError::new(ErrorCode::Forbidden, "needs a full session")
                    .with_detail("method", "server.status")),
            }),
            &json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32000,
                    "message": "needs a full session",
                    "data": {
                        "code": "forbidden",
                        "retryable": false,
                        "details": { "method": "server.status" },
                    },
                },
            }),
        );
    }

    #[test]
    fn writing_an_error_by_hand_matches_what_serde_writes() {
        let error =
            RpcError::new(ErrorCode::Unavailable, "the hub is offline").with_detail("after", 3);
        assert_eq!(error_value(&error), serde_json::to_value(&error).unwrap());
    }

    #[test]
    fn members_this_contract_does_not_know_are_ignored() {
        let frame = Frame::decode(
            r#"{"jsonrpc":"2.0","id":1,"method":"servers.list","trace":"abc","params":{}}"#,
        )
        .unwrap();
        assert_eq!(
            frame,
            Frame::Request(Request {
                id: RequestId::Number(1),
                method: "servers.list".to_owned(),
                params: Some(json!({})),
            })
        );
    }

    /// What a server answers to text that is not a frame.
    fn refusal(text: &str) -> (Option<RequestId>, i32, String) {
        let malformed = Frame::decode(text).unwrap_err();
        assert_eq!(malformed.error.kind(), ErrorCode::Invalid, "{text}");
        let Response { id, outcome } = malformed.into_response();
        let error = outcome.unwrap_err();
        (id, error.code, error.message)
    }

    #[test]
    fn text_that_is_not_json_is_a_parse_error() {
        for text in ["", "{", "hello", "{\"jsonrpc\":\"2.0\",}"] {
            assert_eq!(
                refusal(text),
                (None, -32700, "the frame is not JSON".to_owned()),
                "{text}"
            );
        }
    }

    #[test]
    fn a_batch_is_refused() {
        for text in [
            "[]",
            r#"[{"jsonrpc":"2.0","id":1,"method":"servers.list"}]"#,
        ] {
            assert_eq!(
                refusal(text),
                (None, -32600, "batches are not supported".to_owned()),
                "{text}"
            );
        }
    }

    /// JSON-RPC 2.0 allows these IDs. This contract does not, as the Model
    /// Context Protocol does not: see [`RequestId`]. If this test fails,
    /// the decoder has been loosened, and that is a change to the contract.
    #[test]
    fn a_null_id_and_an_id_with_a_fraction_are_refused_on_purpose() {
        for (id, why) in [
            ("null", "a request's id is not null"),
            ("1.0", "an id is a string or a whole number"),
            ("1.5", "an id is a string or a whole number"),
            ("-0.0", "an id is a string or a whole number"),
            ("1e2", "an id is a string or a whole number"),
        ] {
            let text = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"servers.list"}}"#);
            assert_eq!(refusal(&text), (None, -32600, why.to_owned()), "{id}");
        }
        // What is accepted: an integer, negative or not, and any string.
        for (id, read) in [
            ("1", RequestId::Number(1)),
            ("-7", RequestId::Number(-7)),
            ("0", RequestId::Number(0)),
            (r#""1.5""#, RequestId::Text("1.5".to_owned())),
            (r#""""#, RequestId::Text(String::new())),
        ] {
            let text = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"servers.list"}}"#);
            let Frame::Request(request) = Frame::decode(&text).unwrap() else {
                panic!("{id}");
            };
            assert_eq!(request.id, read, "{id}");
        }
    }

    #[test]
    fn json_that_is_not_a_frame_is_refused_and_the_answer_names_it_when_it_can() {
        let seven = Some(RequestId::Number(7));
        for (text, id, why) in [
            ("17", None, "a frame is a JSON object"),
            ("null", None, "a frame is a JSON object"),
            (
                r#"{"id":7,"method":"servers.list"}"#,
                seven.clone(),
                "jsonrpc must be \"2.0\"",
            ),
            (
                r#"{"jsonrpc":"1.0","id":7,"method":"servers.list"}"#,
                seven.clone(),
                "jsonrpc must be \"2.0\"",
            ),
            (
                r#"{"jsonrpc":"2.0","id":7,"method":17}"#,
                seven.clone(),
                "method must be a string",
            ),
            (
                r#"{"jsonrpc":"2.0","id":7}"#,
                seven.clone(),
                "a frame has a method, or a result, or an error",
            ),
            (
                r#"{"jsonrpc":"2.0","id":7,"result":1,"error":{}}"#,
                seven.clone(),
                "a frame has a method, or a result, or an error",
            ),
            (
                r#"{"jsonrpc":"2.0","id":7,"error":{"code":1,"message":"no data"}}"#,
                seven,
                "an error is {code, message, data}",
            ),
            (
                r#"{"jsonrpc":"2.0","id":null,"method":"servers.list"}"#,
                None,
                "a request's id is not null",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1.5,"method":"servers.list"}"#,
                None,
                "an id is a string or a whole number",
            ),
            (
                r#"{"jsonrpc":"2.0","id":{},"method":"servers.list"}"#,
                None,
                "an id is a string or a whole number",
            ),
            (
                r#"{"jsonrpc":"2.0","result":{}}"#,
                None,
                "a response names the request it answers",
            ),
        ] {
            assert_eq!(refusal(text), (id, -32600, why.to_owned()), "{text}");
        }
    }
}
