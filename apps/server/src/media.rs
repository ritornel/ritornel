//! Media: the token a page reads covers with, and the threads that make
//! covers. See "Media" in docs/internals/contract.md.
//!
//! A media token is 32 random bytes, written as 64 hex digits, made for one
//! session the first time it asks and the same for as long as that session
//! lasts. It opens `GET /v1/art/…` and no other route, it is never the
//! session token or a ticket, and like those it is in no file, no database
//! and no log line. It ends with its session.
//!
//! A cover the cache does not have is made on a [`CoverPool`] thread: never
//! on an async worker, since finding its source reads the database and
//! making it reads a file, decodes and scales. The pool takes the newest
//! request first, because the cover a person scrolled to last is the one
//! they are looking at, and drops a request whose client has gone before
//! it starts.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use ritornel_engine::Entropy;
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::Error;

/// The media tokens the sessions of a running server hold.
#[derive(Default)]
pub(crate) struct MediaTokens {
    by_session: Mutex<HashMap<u64, [u8; 32]>>,
}

impl std::fmt::Debug for MediaTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MediaTokens")
    }
}

impl MediaTokens {
    fn lock(&self) -> MutexGuard<'_, HashMap<u64, [u8; 32]>> {
        // Nothing done under this lock can leave the map half changed.
        self.by_session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The token of `session`, made the first time it is asked for.
    pub(crate) fn of(&self, session: u64, entropy: &dyn Entropy) -> Result<String, Error> {
        let mut tokens = self.lock();
        let secret = match tokens.get(&session) {
            Some(secret) => *secret,
            None => {
                let mut secret = [0; 32];
                entropy.fill(&mut secret)?;
                tokens.insert(session, secret);
                secret
            }
        };
        Ok(secret.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    /// Whether `presented` is a token some session holds. Every token held
    /// is compared, each in the same time wherever the two first differ.
    pub(crate) fn accepts(&self, presented: &str) -> bool {
        let Some(presented) = secret_of(presented) else {
            return false;
        };
        let mut found = false;
        for secret in self.lock().values() {
            found |= bool::from(secret.ct_eq(&presented));
        }
        found
    }

    /// Ends the token of a session that has closed.
    pub(crate) fn revoke(&self, session: u64) {
        self.lock().remove(&session);
    }
}

/// The 32 bytes written as `text`, if it is 64 hex digits.
fn secret_of(text: &str) -> Option<[u8; 32]> {
    let text = text.as_bytes();
    if text.len() != 64 {
        return None;
    }
    let mut secret = [0; 32];
    for (byte, pair) in secret.iter_mut().zip(text.chunks(2)) {
        let digit = |character: u8| match character {
            b'0'..=b'9' => Some(character - b'0'),
            b'a'..=b'f' => Some(character - b'a' + 10),
            _ => None,
        };
        let [high, low] = pair else { return None };
        *byte = (digit(*high)? << 4) | digit(*low)?;
    }
    Some(secret)
}

/// One request for a cover, waiting for a thread.
struct Job {
    /// Set when the client stopped waiting.
    cancelled: Arc<AtomicBool>,
    work: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct Queue {
    /// Newest last: a thread takes from the end.
    waiting: Vec<Job>,
    stopping: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Threads that make covers, newest request first.
pub(crate) struct CoverPool {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for CoverPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CoverPool")
    }
}

/// Sets its flag when it is dropped: a request whose client has gone.
pub(crate) struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl CoverPool {
    /// Starts `threads` threads.
    pub(crate) fn start(threads: usize) -> Self {
        let shared = Arc::new(Shared::default());
        let handles = (0..threads.max(1))
            .filter_map(|index| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(format!("covers {index}"))
                    .spawn(move || work(&shared))
                    .ok()
            })
            .collect();
        Self {
            shared,
            threads: Mutex::new(handles),
        }
    }

    /// Queues `work`, newest first. The guard it returns drops the work if
    /// it is dropped before a thread has started on it.
    pub(crate) fn submit(&self, work: impl FnOnce() + Send + 'static) -> CancelOnDrop {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut queue = self.shared.lock();
        if !queue.stopping {
            queue.waiting.push(Job {
                cancelled: Arc::clone(&cancelled),
                work: Box::new(work),
            });
        }
        drop(queue);
        self.shared.ready.notify_one();
        CancelOnDrop(cancelled)
    }

    /// Stops taking work, drops what is waiting, and waits for what is in
    /// hand to finish.
    pub(crate) fn stop(&self) {
        let mut queue = self.shared.lock();
        queue.stopping = true;
        queue.waiting.clear();
        drop(queue);
        self.shared.ready.notify_all();
        let threads =
            std::mem::take(&mut *self.threads.lock().unwrap_or_else(PoisonError::into_inner));
        for thread in threads {
            let _ = thread.join();
        }
    }
}

impl Drop for CoverPool {
    fn drop(&mut self) {
        let mut queue = self.shared.lock();
        queue.stopping = true;
        queue.waiting.clear();
        drop(queue);
        self.shared.ready.notify_all();
    }
}

/// One thread's loop: the newest request that is still wanted, until the
/// pool stops.
fn work(shared: &Shared) {
    loop {
        let job = {
            let mut queue = shared.lock();
            loop {
                if queue.stopping {
                    return;
                }
                // The newest that is still wanted. Those cancelled are
                // dropped on the way.
                let mut found = None;
                while let Some(job) = queue.waiting.pop() {
                    if !job.cancelled.load(Ordering::SeqCst) {
                        found = Some(job);
                        break;
                    }
                }
                if let Some(job) = found {
                    break job;
                }
                queue = shared
                    .ready
                    .wait(queue)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        // A cover a decoder panics on is not made, and its request is
        // answered as failed when its work is dropped. The thread goes on
        // to the next, so that the pool does not shrink one panic at a time
        // until no cover is ever made.
        if std::panic::catch_unwind(AssertUnwindSafe(job.work)).is_err() {
            warn!("making a cover panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use ritornel_engine::OsEntropy;

    use super::*;

    #[test]
    fn a_token_is_the_same_for_its_session_accepted_while_it_lasts_and_no_other_is() {
        let tokens = MediaTokens::default();
        let entropy = OsEntropy;
        let first = tokens.of(1, &entropy).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(tokens.of(1, &entropy).unwrap(), first);
        let second = tokens.of(2, &entropy).unwrap();
        assert_ne!(first, second);
        assert!(tokens.accepts(&first) && tokens.accepts(&second));
        assert!(!tokens.accepts(&"0".repeat(64)));
        assert!(!tokens.accepts(&first.to_uppercase()));
        assert!(!tokens.accepts(""));
        tokens.revoke(1);
        assert!(!tokens.accepts(&first));
        assert!(tokens.accepts(&second));
    }

    #[test]
    fn the_newest_request_is_made_first_and_one_whose_client_left_is_not_made() {
        let pool = CoverPool::start(1);
        let (done, order) = mpsc::channel();
        // Holds the one thread until every request is queued.
        let (release, held) = mpsc::channel::<()>();
        let (took, taken) = mpsc::channel();
        let _busy = pool.submit(move || {
            took.send(()).unwrap();
            let _ = held.recv();
        });
        taken.recv_timeout(Duration::from_secs(60)).unwrap();
        let mut guards = Vec::new();
        for name in ["oldest", "middle", "left", "newest"] {
            let done = done.clone();
            guards.push((name, pool.submit(move || done.send(name).unwrap())));
        }
        // The client of one went away.
        guards.retain(|(name, _)| *name != "left");
        release.send(()).unwrap();
        let made: Vec<_> = (0..3)
            .map(|_| order.recv_timeout(Duration::from_secs(10)).unwrap())
            .collect();
        assert_eq!(made, ["newest", "middle", "oldest"]);
        // The one whose client left was not made: by now it would have been.
        assert!(order.recv_timeout(Duration::from_millis(50)).is_err());
        pool.stop();
    }

    #[test]
    fn a_request_that_panics_leaves_its_thread_to_make_the_next() {
        let pool = CoverPool::start(1);
        // Holds the one thread until both are queued. The newest is made
        // first: the one that panics.
        let (release, held) = mpsc::channel::<()>();
        let (took, taken) = mpsc::channel();
        let _busy = pool.submit(move || {
            took.send(()).unwrap();
            let _ = held.recv();
        });
        taken.recv_timeout(Duration::from_secs(60)).unwrap();
        let (done, made) = mpsc::channel();
        let _second = pool.submit(move || done.send(()).unwrap());
        let _first = pool.submit(|| panic!("a cover that cannot be made"));
        release.send(()).unwrap();
        assert_eq!(made.recv_timeout(Duration::from_secs(10)), Ok(()));
        pool.stop();
    }
}
