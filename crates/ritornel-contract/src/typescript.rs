//! The contract as TypeScript: `contract/contract.ts` at the repository
//! root, which a client written in TypeScript is typed by. A test here fails
//! when that file and the Rust types disagree, so CI catches a contract that
//! changed without its TypeScript. See "Schema definition and code
//! generation" in docs/internals/contract.md.
//!
//! To write the file again after changing a wire type or the method table:
//! `mise run contract`.
//!
//! Nothing here lists the methods or the wire types. The methods come from
//! [`visit_methods`], and a wire type is written because a method, an event,
//! an error or a route's body uses it. ts-rs writes each type from its serde
//! attributes, and is built only for this crate's tests. Three things it
//! cannot take from them are settled here and in the types:
//!
//! - A 64-bit integer would be a `bigint`. Every integer of the contract
//!   fits in 53 bits, so [`config`] makes each a `number`.
//! - A field that is left out when it is nothing says so twice: with
//!   `skip_serializing_if` for serde and with `ts(optional)` for
//!   TypeScript. Without the second, ts-rs lets the field be `null`, and
//!   unless serde's `default` is there too it calls the field always
//!   present. Neither is what is on the wire.
//! - A frame is written by hand in `frame.rs`, so its types are written by
//!   hand in [`FRAMES`].

use std::any::TypeId;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use ts_rs::{Config, TS, TypeVisitor};

use crate::methods::{Method, MethodKind, MethodVisitor, visit_methods};
use crate::{
    ART_SIZES, CONTRACT_VERSION, EVENT_METHOD, Event, FILTER_VERSION, MAX_CLIENT_TEXT,
    MEDIA_TOKEN_PARAM, PublicDescriptor, Receipt, RpcError, SocketTicket, TICKET_PARAM, Topic,
    capability, route,
};

/// The file, from the repository root.
const FILE: &str = "contract/contract.ts";

/// The environment variable that makes the test write the file instead of
/// comparing it. It is the one the schema snapshots use.
const BLESS: &str = "RITORNEL_BLESS";

/// The routes, by the name each has in `ROUTES`. A route added to
/// [`route`] is added here.
const ROUTES: &[(&str, &str)] = &[
    ("ws", route::WS),
    ("ticket", route::TICKET),
    ("descriptor", route::DESCRIPTOR),
    ("health", route::HEALTH),
    ("art", route::ART),
    ("link", route::LINK),
    ("audio", route::AUDIO),
    ("cacheDerivative", route::CACHE_DERIVATIVE),
    ("cachePackaged", route::CACHE_PACKAGED),
];

/// The capability flags, by the name each has in `CAPABILITIES`. A flag
/// added to [`capability`] is added here.
const CAPABILITIES: &[(&str, &str)] = &[
    ("eventsServers", capability::EVENTS_SERVERS),
    ("eventsLibrary", capability::EVENTS_LIBRARY),
    ("lists", capability::LISTS),
    ("mediaArt", capability::MEDIA_ART),
    ("playback", capability::PLAYBACK),
    ("players", capability::PLAYERS),
];

const HEADER: &str = "\
// The Ritornel contract as TypeScript: every call, event, error and route a
// client uses to talk to a server.
//
// Generated from the Rust types and the method table in
// crates/ritornel-contract. Do not edit it: change those, then run
// `mise run contract`.
//
// Fields are camelCase and enum values are snake_case. An ID is an opaque
// string. Instants are Unix milliseconds and durations are milliseconds.
// Every integer fits in 53 bits, so each is a `number`.
";

/// The frames. `frame.rs` reads and writes them by hand, so there is no
/// Rust type to write these from. A test below holds them to what
/// [`Frame::encode`](crate::Frame::encode) writes.
const FRAMES: &str = r#"
/**
 * What a client names a request by, and a response names its request by: a
 * whole number or a string.
 */
export type RequestId = number | string;

/**
 * A call: a JSON-RPC 2.0 request. It gets exactly one response, which
 * carries its `id`. A method that takes no params is sent `{}`.
 */
export type RequestFrame<M extends MethodName = MethodName> = {
  [K in M]: { jsonrpc: "2.0"; id: RequestId; method: K; params: MethodMap[K]["params"] };
}[M];

/**
 * The answer to a call: its result, or why there is none. The `id` is the
 * request's, and is `null` only when the request was too broken to read one
 * from.
 */
export type ResponseFrame<M extends MethodName = MethodName> =
  | { jsonrpc: "2.0"; id: RequestId; result: MethodMap[M]["result"]; error?: never }
  | { jsonrpc: "2.0"; id: RequestId | null; error: RpcError; result?: never };

/**
 * An event: a notification, which gets no response. A client ignores one
 * whose topic it does not know.
 */
export type EventFrame = { jsonrpc: "2.0"; method: typeof EVENT_METHOD; params: Event };

/** A frame a server sends a client. */
export type ServerFrame = ResponseFrame | EventFrame;
"#;

/// How ts-rs writes a type. Every integer of the contract fits in 53 bits,
/// so the 64-bit ones are a `number` and never a `bigint`.
fn config() -> Config {
    Config::new().with_large_int("number")
}

/// Text as a TypeScript string literal. A JSON string is one.
fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

/// The declaration of every type that is visited, and of every type those
/// are made of, by its name in TypeScript.
struct Declarations {
    config: Config,
    seen: HashSet<TypeId>,
    by_name: BTreeMap<String, String>,
}

impl TypeVisitor for Declarations {
    fn visit<T: TS + 'static + ?Sized>(&mut self) {
        // What has nowhere to be exported to has no declaration: a string,
        // a number or an array is written where it is used.
        if T::output_path().is_none() || !self.seen.insert(TypeId::of::<T>()) {
            return;
        }
        let name = T::ident(&self.config);
        let declaration = declaration::<T>(&self.config);
        // Two Rust types may share a declaration: ts-rs writes a JSON value
        // through a type of its own. Sharing a name and not the
        // declaration is two types where one file has room for one.
        if let Some(declared) = self.by_name.get(&name) {
            assert!(
                *declared == declaration,
                "two Rust types are both named {name} in TypeScript, and they differ: rename one"
            );
        }
        self.by_name.insert(name, declaration);
        T::visit_dependencies(self);
    }
}

/// One type's declaration with its documentation, as ts-rs writes it and
/// laid out one field to a line.
///
/// ts-rs puts a struct's fields on one line, with each field's
/// documentation flush left above it. Only white space is changed here:
/// the fields are indented, the closing brace gets a line of its own, and
/// no line ends in a space.
fn declaration<T: TS + ?Sized>(config: &Config) -> String {
    let written = T::decl(config);
    let (body, closing) = match written.strip_suffix(", };") {
        Some(body) => (body, ",\n};\n"),
        None => (written.as_str(), "\n"),
    };
    let mut text = T::docs().unwrap_or_default();
    text.push_str("export ");
    for (index, line) in body.lines().enumerate() {
        if index > 0 {
            text.push_str("\n  ");
        }
        text.push_str(line.trim_end());
    }
    text.push_str(closing);
    text
}

/// One row of the method table, with its types by their TypeScript names.
struct Row {
    name: &'static str,
    kind: MethodKind,
    needs_full: bool,
    params: String,
    result: String,
}

/// Reads the method table: a row for each method, and a declaration for
/// each type a method uses.
struct Table {
    declarations: Declarations,
    rows: Vec<Row>,
}

impl MethodVisitor for Table {
    fn visit<M: Method>(&mut self) {
        self.declarations.visit::<M::Params>();
        self.declarations.visit::<M::Result>();
        self.rows.push(Row {
            name: M::NAME,
            kind: M::KIND,
            needs_full: M::NEEDS_FULL,
            params: M::Params::name(&self.declarations.config),
            result: M::Result::name(&self.declarations.config),
        });
    }
}

/// `contract/contract.ts`, whole.
///
/// The same contract gives the same bytes on every machine: the methods
/// are in the table's order, the types are in the order of their names,
/// and nothing depends on the time or the place.
fn generate() -> String {
    let mut table = Table {
        declarations: Declarations {
            config: config(),
            seen: HashSet::new(),
            by_name: BTreeMap::new(),
        },
        rows: Vec::new(),
    };
    visit_methods(&mut table);
    let Table {
        mut declarations,
        rows,
    } = table;
    // What no method names: an event, an error, the body of a route, and
    // the receipt, which each command's result holds as fields of its own.
    declarations.visit::<Receipt>();
    declarations.visit::<Event>();
    declarations.visit::<Topic>();
    declarations.visit::<RpcError>();
    declarations.visit::<PublicDescriptor>();
    declarations.visit::<route::Health>();
    declarations.visit::<SocketTicket>();
    declarations.visit::<crate::LinkRequest>();
    declarations.visit::<crate::LinkGranted>();

    // `TOPICS` and the event map are written from `Topic::ALL`, and the
    // types `Topic` and `Event` from the enums. They must agree: the map
    // takes each topic's events out of `Event` by its tag, and a topic with
    // no events would come out as `never` and say nothing.
    let topics: Vec<String> = Topic::ALL
        .iter()
        .map(|topic| quoted(topic.as_str()))
        .collect();
    let topic = format!("export type Topic = {};\n", topics.join(" | "));
    assert!(
        declarations.by_name["Topic"].ends_with(&topic),
        "Topic::ALL is not every topic, or ts-rs no longer writes them as `{topic}`"
    );
    for topic in &topics {
        let tag = format!("\"topic\": {topic}");
        assert!(
            declarations.by_name["Event"].contains(&tag),
            "the topic {topic} has no variant in Event, or ts-rs no longer writes one as {{ {tag} }}"
        );
    }

    let constants = |constants: &[(&str, &str)]| -> String {
        constants
            .iter()
            .map(|(name, value)| format!("  {name}: {},\n", quoted(value)))
            .collect()
    };
    let kind = |kind| match kind {
        MethodKind::Session => "session",
        MethodKind::Query => "query",
        MethodKind::Command => "command",
    };
    let methods: String = rows
        .iter()
        .map(|row| {
            let (name, kind) = (quoted(row.name), quoted(kind(row.kind)));
            let needs_full = row.needs_full;
            format!("  {name}: {{ kind: {kind}, needsFull: {needs_full} }},\n")
        })
        .collect();
    let method_map: String = rows
        .iter()
        .map(|row| {
            let (name, params, result) = (quoted(row.name), &row.params, &row.result);
            format!("  {name}: {{ params: {params}; result: {result} }};\n")
        })
        .collect();
    let event_map: String = topics
        .iter()
        .map(|topic| format!("  {topic}: Extract<Event, {{ topic: {topic} }}>;\n"))
        .collect();

    let mut text = format!(
        "{HEADER}
/** The version of the contract these types are. */
export const CONTRACT_VERSION = {CONTRACT_VERSION};

/** The HTTP routes. Each is a `GET` but `ticket` and `link`, which are `POST`s. */
export const ROUTES = {{
{routes}}} as const;

/**
 * The name of the query parameter that carries a ticket on the socket's
 * route.
 */
export const TICKET_PARAM = {ticket_param};

/** The capability flags a server may list in its descriptor. */
export const CAPABILITIES = {{
{capabilities}}} as const;

/** The method of every event notification. */
export const EVENT_METHOD = {event_method};

/** The longest a client's name or version may be, in characters. */
export const MAX_CLIENT_TEXT = {MAX_CLIENT_TEXT};

/**
 * The name of the query parameter that carries a media token on a media
 * route.
 */
export const MEDIA_TOKEN_PARAM = {media_token_param};

/**
 * The sizes a cover is served at, in pixels on its longer side. The
 * descriptor lists the ones a server serves.
 */
export const ART_SIZES = [{art_sizes}] as const;

/** The version of the filter language a filter is written in. */
export const FILTER_VERSION = {FILTER_VERSION};

/**
 * Every method, in the order of the method table: what kind of call it is,
 * and whether it needs a `full` session.
 */
export const METHODS = {{
{methods}}} as const;

/** Every topic a client can subscribe to. */
export const TOPICS = [{topics}] as const;

/** The params and the result of each method. */
export type MethodMap = {{
{method_map}}};

/** The name of a method. */
export type MethodName = keyof MethodMap;

/**
 * What an event on each topic carries: the params of its notification. Its
 * keys are the topics, as `Topic` is.
 */
export type EventMap = {{
{event_map}}};
{FRAMES}",
        routes = constants(ROUTES),
        capabilities = constants(CAPABILITIES),
        event_method = quoted(EVENT_METHOD),
        ticket_param = quoted(TICKET_PARAM),
        media_token_param = quoted(MEDIA_TOKEN_PARAM),
        art_sizes = ART_SIZES
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        topics = topics.join(", "),
    );
    for declaration in declarations.by_name.values() {
        text.push('\n');
        text.push_str(declaration);
    }
    text
}

#[test]
fn the_committed_typescript_is_what_the_contract_generates() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(FILE);
    let produced = generate();
    if std::env::var_os(BLESS).is_some() {
        std::fs::write(&path, &produced).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != produced {
        let line = committed
            .lines()
            .zip(produced.lines())
            .position(|(committed, produced)| committed != produced)
            .unwrap_or_else(|| committed.lines().count().min(produced.lines().count()));
        panic!(
            "{FILE} is not what the contract generates; they first differ at line {}.\n\
             If the Rust types are right, run `mise run contract` and commit the result.",
            line + 1
        );
    }
}

/// The contract's version and the digest of the wire it was set for,
/// committed beside `contract.ts`. `scripts/check-contract.sh` compares it
/// with main's: a wire that differs from main's is one version above it.
const VERSION_FILE: &str = "contract/contract.version";

/// A digest of the wire as `contract.ts` writes it: FNV-1a, 64 bits, of
/// its words, leaving out the version's line and the comments. It only has
/// to change when the wire does, and neither a comment nor where a comment
/// moves the line breaks is the wire.
fn wire_digest(text: &str) -> String {
    let comment = |line: &str| {
        let line = line.trim_start();
        line.starts_with("//") || line.starts_with("/*") || line.starts_with('*')
    };
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    let words = text
        .lines()
        .filter(|line| !line.starts_with("export const CONTRACT_VERSION = ") && !comment(line))
        .flat_map(str::split_whitespace);
    for word in words {
        for byte in word.bytes().chain(std::iter::once(b' ')) {
            digest ^= u64::from(byte);
            digest = digest.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{digest:016x}")
}

/// `contract.version`'s text for this version and wire.
fn version_file(digest: &str) -> String {
    format!(
        "# The contract version, and a digest of the wire it was set for. Written by\n\
         # `mise run contract`. scripts/check-contract.sh refuses a wire that differs\n\
         # from main's under main's version: raise CONTRACT_VERSION, once, for it.\n\
         version {CONTRACT_VERSION}\ndigest {digest}\n"
    )
}

/// Whether the version changed with the wire is checked against main's, by
/// `scripts/check-contract.sh`; here, that the file is this build's.
#[test]
fn the_version_file_is_this_builds_version_and_wire() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(VERSION_FILE);
    let digest = wire_digest(&generate());
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    let field = |name: &str| {
        committed
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
            .map(str::to_owned)
    };
    let (version, recorded) = (field("version"), field("digest"));
    let same_version = version.as_deref() == Some(CONTRACT_VERSION.to_string().as_str());
    let same_wire = recorded.as_deref() == Some(digest.as_str());
    if std::env::var_os(BLESS).is_some() {
        std::fs::write(&path, version_file(&digest)).unwrap();
        return;
    }
    assert!(
        same_version && same_wire,
        "{VERSION_FILE} is not this version and wire: run `mise run contract` and commit the result"
    );
}

#[test]
fn the_wire_digest_follows_the_text_and_not_the_version() {
    let text = generate();
    assert_eq!(wire_digest(&text), wire_digest(&text));
    assert_ne!(
        wire_digest(&text),
        wire_digest(&format!("{text}export const MORE = 1;\n"))
    );
    assert_eq!(
        wire_digest(&text),
        wire_digest(&format!("{text}/** A comment. */\n"))
    );
    let other = text.replace(
        &format!("export const CONTRACT_VERSION = {CONTRACT_VERSION};"),
        "export const CONTRACT_VERSION = 999999;",
    );
    assert_eq!(wire_digest(&text), wire_digest(&other));
    // A field given a comment is put on a line of its own.
    let commented = text.replace(
        "export type Dismissed = { record: RecordStatus,",
        "export type Dismissed = {\n/**\n * The record.\n */\nrecord: RecordStatus,",
    );
    assert_ne!(text, commented);
    assert_eq!(wire_digest(&text), wire_digest(&commented));
}

#[test]
fn every_integer_is_a_number() {
    let text = generate();
    assert!(!text.contains("bigint"));
    // The 64-bit fields there are today.
    for field in [
        "maxMessageBytes: number",
        "dataBytes?: number",
        "serverSchema: number",
        "librarySchema?: number",
        "revision: number",
        "revision?: number",
    ] {
        assert!(text.contains(field), "{field}");
    }
}

#[test]
fn an_id_is_a_string_and_a_field_left_out_is_optional_and_never_null() {
    let text = generate();
    assert!(text.contains("  serverId: string,\n"));
    assert!(text.contains("  dataBytes?: number,\n"));
}

/// Without `ts(optional)`, ts-rs lets a field that serde leaves out be
/// `null`, which it never is on the wire. Nothing but the source says which
/// fields serde leaves out, so this reads the source.
#[test]
fn a_field_serde_leaves_out_is_marked_optional_for_typescript() {
    let mut folders = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut files = Vec::new();
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                files.push(path);
            }
        }
    }
    let mut skipped = 0;
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines = text.lines().map(str::trim);
        while let Some(line) = lines.next() {
            if line.starts_with("#[serde(") && line.contains("skip_serializing_if") {
                skipped += 1;
                assert_eq!(
                    lines.next(),
                    Some("#[cfg_attr(test, ts(optional))]"),
                    "in {}, the line after `{line}`",
                    path.display()
                );
            }
        }
    }
    // `FreeDisk::data_bytes`, at least: the test is reading the source.
    assert!(skipped >= 1);
}

#[test]
fn the_maps_follow_the_method_table_and_the_topics() {
    let text = generate();
    for method in crate::METHODS {
        let row = format!(
            "  \"{}\": {{ params: {}; result: {} }};\n",
            method.name, method.params, method.result
        );
        assert!(text.contains(&row), "{row}");
    }
    assert!(text.contains("  \"server.status\": { kind: \"query\", needsFull: true },\n"));
    assert!(text.contains("  \"library.create\": { kind: \"command\", needsFull: true },\n"));
    assert!(text.contains("  \"servers\": Extract<Event, { topic: \"servers\" }>;\n"));
    assert!(text.contains("  \"jobs\": Extract<Event, { topic: \"jobs\" }>;\n"));
    assert!(text.contains("  \"list\": Extract<Event, { topic: \"list\" }>;\n"));
    assert!(text.contains(
        "export const TOPICS = [\"servers\", \"players\", \"jobs\", \"library\", \"list\", \"playback\", \"progress\", \"queue\"] as const;\n"
    ));
}

#[test]
fn the_ticket_has_its_route_its_parameter_and_its_type() {
    let text = generate();
    assert!(text.contains("  ticket: \"/v1/ticket\",\n"));
    assert!(text.contains("export const TICKET_PARAM = \"ticket\";\n"));
    assert!(text.contains("export type SocketTicket = {\n"));
    assert!(text.contains("  expiresInMs: number,\n"));
}

#[test]
fn the_frame_types_have_the_members_a_frame_is_written_with() {
    use serde_json::{Value, json};

    use crate::{ErrorCode, Frame, Notification, Request, RequestId, Response};

    /// The members of the frame type `name` that ends at `end`, one
    /// alternative at a time.
    fn alternatives(name: &str, end: &str) -> Vec<&'static str> {
        let start = FRAMES.find(&format!("export type {name}")).unwrap();
        let (declaration, _) = FRAMES[start..].split_once(end).unwrap();
        declaration
            .lines()
            .filter(|line| line.contains("jsonrpc: \"2.0\";"))
            .collect()
    }

    /// Whether the frame type has an alternative with exactly the members
    /// the frame was written with, leaving out those it declares `never`.
    fn written_as(frame: &Frame, name: &str, end: &str) {
        let Value::Object(wire) = serde_json::from_str(&frame.encode()).unwrap() else {
            panic!("a frame is an object");
        };
        let wire: Vec<_> = wire.keys().map(String::as_str).collect();
        let fits = alternatives(name, end).iter().any(|alternative| {
            let members: Vec<_> = alternative
                .split([' ', '{'])
                .filter_map(|word| word.strip_suffix(':'))
                .filter(|member| member.chars().all(char::is_alphanumeric))
                .collect();
            members.len() == wire.len() && wire.iter().all(|member| members.contains(member))
        });
        assert!(fits, "{name} has no alternative with the members {wire:?}");
    }

    let id = RequestId::Number(1);
    written_as(
        &Frame::Request(Request {
            id: id.clone(),
            method: "servers.list".to_owned(),
            params: Some(json!({})),
        }),
        "RequestFrame",
        "}[M];",
    );
    written_as(
        &Frame::Response(Response {
            id: Some(id),
            outcome: Ok(json!({})),
        }),
        "ResponseFrame",
        "\n\n",
    );
    written_as(
        &Frame::Response(Response {
            id: None,
            outcome: Err(RpcError::new(ErrorCode::Internal, "")),
        }),
        "ResponseFrame",
        "\n\n",
    );
    written_as(
        &Frame::Notification(Notification {
            method: EVENT_METHOD.to_owned(),
            params: Some(json!({})),
        }),
        "EventFrame",
        "\n\n",
    );
}
