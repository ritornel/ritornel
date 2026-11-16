//! The method table: every call of the contract, defined once. See decision
//! 1 in docs/design/contract.md.
//!
//! A method is a type in this module that implements [`Method`], named for
//! its call: [`ServerHello`] is `server.hello`. The server's dispatcher and
//! the Rust client both take the name, the params type and the result type
//! from that one implementation, so they cannot disagree.
//!
//! A generator walks the table two ways. [`METHODS`] lists each method with
//! its types by name, for anything that needs only text. [`visit_methods`]
//! hands a [`MethodVisitor`] each method as a type, and [`Described`] is
//! what a generator may ask of the params and the result. The TypeScript in
//! `contract/contract.ts` is written that way, by this crate's tests.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// What sort of call a method is.
///
/// The commands of M1 join as a third kind: each carries an idempotency key
/// and returns a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    /// It changes only the session it is called on: who the client is, and
    /// what it is subscribed to.
    Session,
    /// It reads and changes nothing.
    Query,
}

/// One call of the contract.
pub trait Method {
    /// The name on the wire, such as `server.hello`.
    const NAME: &'static str;
    /// What sort of call it is.
    const KIND: MethodKind;
    /// Whether it needs a `full` session. A method that does not is open to
    /// any session.
    const NEEDS_FULL: bool;
    /// The params. A method that takes none has [`NoParams`](crate::NoParams).
    type Params: Serialize + DeserializeOwned + Described;
    /// The result. It is always an object, so that it can grow fields.
    type Result: Serialize + DeserializeOwned + Described;
}

/// What a generator asks of a method's params and result.
///
/// In this crate's tests it is ts-rs's `TS`: the type can be written as
/// TypeScript. A method whose params or result cannot be does not compile
/// there, so it cannot be left out of `contract/contract.ts` unnoticed. In
/// every other build it asks nothing and every type has it, so no program
/// is built with ts-rs.
#[cfg(test)]
pub trait Described: ts_rs::TS + 'static {}

#[cfg(test)]
impl<T: ts_rs::TS + 'static> Described for T {}

/// What a generator asks of a method's params and result. In this build it
/// asks nothing: see the same trait as this crate's tests have it.
#[cfg(not(test))]
pub trait Described {}

#[cfg(not(test))]
impl<T> Described for T {}

/// One row of the method table, as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    /// The name on the wire.
    pub name: &'static str,
    /// What sort of call it is.
    pub kind: MethodKind,
    /// Whether it needs a `full` session.
    pub needs_full: bool,
    /// The name of the params type in this crate.
    pub params: &'static str,
    /// The name of the result type in this crate.
    pub result: &'static str,
}

/// Something that is shown every method in turn, as a type.
pub trait MethodVisitor {
    /// Called once for each method, in the table's order.
    fn visit<M: Method>(&mut self);
}

/// Defines the table: a type for each method, [`METHODS`] and
/// [`visit_methods`].
macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $marker:ident: $name:literal, $kind:ident, needs_full: $needs_full:literal,
        $params:ident => $result:ident;
    )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $marker;

            impl Method for $marker {
                const NAME: &'static str = $name;
                const KIND: MethodKind = MethodKind::$kind;
                const NEEDS_FULL: bool = $needs_full;
                type Params = crate::$params;
                type Result = crate::$result;
            }
        )*

        /// Every method of the contract, in the order of
        /// docs/design/contract.md's call list.
        pub const METHODS: &[MethodInfo] = &[$(
            MethodInfo {
                name: $name,
                kind: MethodKind::$kind,
                needs_full: $needs_full,
                params: stringify!($params),
                result: stringify!($result),
            },
        )*];

        /// Shows `visitor` every method, in [`METHODS`]' order.
        pub fn visit_methods<V: MethodVisitor>(visitor: &mut V) {
            $(visitor.visit::<$marker>();)*
        }
    };
}

methods! {
    /// `server.hello`: the handshake. The client says who it is and the
    /// server answers with its descriptor. It is the first call on a
    /// socket, and every other call is refused until it has succeeded.
    ServerHello: "server.hello", Session, needs_full: false,
    HelloParams => Descriptor;

    /// `server.status`: the health of the server, for a person or an agent
    /// explaining a failure. It names a path on the server's disk, so it
    /// needs a `full` session.
    ServerStatus: "server.status", Query, needs_full: true,
    NoParams => ServerStatus;

    /// `servers.list`: every server this one knows, itself included. A
    /// client joins the copies of a track against it.
    ServersList: "servers.list", Query, needs_full: false,
    NoParams => ServersList;

    /// `events.subscribe`: adds topics to what this session is sent.
    EventsSubscribe: "events.subscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;

    /// `events.unsubscribe`: removes topics from what this session is sent.
    /// No event on those topics follows the response.
    EventsUnsubscribe: "events.unsubscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;
}

/// The row of the table for the method named `name`, if there is one.
pub fn method(name: &str) -> Option<&'static MethodInfo> {
    METHODS.iter().find(|method| method.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_m0_call_list() {
        let names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
        assert_eq!(
            names,
            [
                "server.hello",
                "server.status",
                "servers.list",
                "events.subscribe",
                "events.unsubscribe"
            ]
        );
        assert_eq!(method("server.status").unwrap().result, "ServerStatus");
        assert_eq!(method("server.statu"), None);
    }

    #[test]
    fn only_the_status_needs_a_full_session() {
        let full: Vec<_> = METHODS
            .iter()
            .filter(|method| method.needs_full)
            .map(|method| method.name)
            .collect();
        assert_eq!(full, ["server.status"]);
    }

    #[test]
    fn a_visitor_sees_every_row_as_a_type() {
        #[derive(Default)]
        struct Rows(Vec<MethodInfo>);

        impl MethodVisitor for Rows {
            fn visit<M: Method>(&mut self) {
                let short = |name: &'static str| name.rsplit("::").next().unwrap();
                self.0.push(MethodInfo {
                    name: M::NAME,
                    kind: M::KIND,
                    needs_full: M::NEEDS_FULL,
                    params: short(std::any::type_name::<M::Params>()),
                    result: short(std::any::type_name::<M::Result>()),
                });
            }
        }

        let mut rows = Rows::default();
        visit_methods(&mut rows);
        assert_eq!(rows.0, METHODS);
    }
}
