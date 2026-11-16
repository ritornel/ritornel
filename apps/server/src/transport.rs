//! What a session's frames travel on, and the one loop that moves them.
//! A transport is a [`Wire`]: the socket, or a pair of channels in memory.
//! Both carry a frame as text and both are run by [`pump`], so whatever is
//! decided here is decided for every transport. See decision 4 in
//! docs/design/contract.md.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;

use crate::dispatch::{Core, Received};
use crate::session::Session;

/// Something that carries text frames to and from one client.
pub(crate) trait Wire: Send {
    /// The next frame from the client. `None` when the client has gone, or
    /// has sent something the transport itself refuses.
    fn recv(&mut self) -> impl Future<Output = Option<String>> + Send;

    /// Sends one frame to the client. `false` when the client has gone.
    fn send(&mut self, text: String) -> impl Future<Output = bool> + Send;

    /// Tells the client the server is closing, if the transport has a way
    /// to.
    fn close(&mut self) -> impl Future<Output = ()> + Send;
}

/// Moves frames between a session and its transport until either side
/// closes.
///
/// A frame from the client is decoded and answered on the runtime's
/// blocking threads: a call may read the disk or the database, and an async
/// worker that waited on either would hold up every other session. Frames
/// of one session are still answered one at a time, in the order they came.
///
/// Whatever this is waiting on, it stops waiting when the session closes.
/// A write to a client that has stopped reading never finishes by itself,
/// and neither that nor anything else here may outlast the server.
pub(crate) async fn pump(core: &Arc<Core>, session: &Arc<Session>, wire: &mut impl Wire) {
    let closed = session.closed();
    tokio::pin!(closed);
    // Whether the client can still be told that the server is closing.
    let goodbye = loop {
        tokio::select! {
            () = &mut closed => break true,
            text = wire.recv() => {
                let Some(text) = text else { break false };
                let (core, session) = (Arc::clone(core), Arc::clone(session));
                let answering = tokio::task::spawn_blocking(move || core.receive(&session, &text));
                tokio::select! {
                    // The call goes on to its end where it is. Closing the
                    // database waits for it.
                    () = &mut closed => break true,
                    // A call that panicked ends its session, and no other.
                    received = answering => {
                        if !matches!(received, Ok(Received::Taken)) {
                            break false;
                        }
                    }
                }
            }
            frame = session.next() => {
                let Some(frame) = frame else { break true };
                tokio::select! {
                    // The client is not reading, so there is no telling it
                    // anything.
                    () = &mut closed => break false,
                    sent = wire.send(frame.encode()) => {
                        if !sent {
                            break false;
                        }
                    }
                }
            }
        }
    };
    if goodbye {
        // Said if it can be said at once. A goodbye is not worth waiting
        // for either.
        let _ = tokio::time::timeout(Duration::ZERO, wire.close()).await;
    }
}

/// Waits for every task in `tasks` to end, for at most `grace`, and then
/// ends those that are left and waits for them to be gone. When this
/// returns none of them is running. `true` if they all ended by themselves.
pub(crate) async fn end_within(tasks: &mut JoinSet<()>, grace: Duration) -> bool {
    let ended = async { while tasks.join_next().await.is_some() {} };
    let by_themselves = tokio::time::timeout(grace, ended).await.is_ok();
    if !by_themselves {
        tasks.shutdown().await;
    }
    by_themselves
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// Says when it has been dropped.
    struct Gone(Arc<AtomicBool>);

    impl Drop for Gone {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn tasks_that_end_in_time_are_waited_for_and_the_rest_are_ended() {
        let mut tasks = JoinSet::new();
        tasks.spawn(async {});
        assert!(end_within(&mut tasks, Duration::from_secs(60)).await);

        // One that would never end by itself.
        let gone = Arc::new(AtomicBool::new(false));
        let held = Gone(Arc::clone(&gone));
        tasks.spawn(async {});
        tasks.spawn(async move {
            let _held = held;
            std::future::pending::<()>().await;
        });
        assert!(!end_within(&mut tasks, Duration::from_millis(20)).await);
        // It is not left running: it is gone, with what it held.
        assert!(gone.load(Ordering::SeqCst));
        assert!(tasks.is_empty());
    }
}
