//! Only admitted work reaches the threads. A queued pin release needs no
//! admission, so dropping a batch never waits on the filesystem or database.

use crate::Error;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use tokio::sync::{OwnedMutexGuard, Semaphore, oneshot};

use super::{CancelOnDrop, KeyWork};

type Task = Box<dyn FnOnce() + Send>;

pub(super) struct Workers {
    send: mpsc::Sender<Task>,
    receive: Arc<Mutex<mpsc::Receiver<Task>>>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    permits: Arc<Semaphore>,
    limit: usize,
}

/// The pool drops its sender first. Threads drain outstanding work and pin
/// releases before the server closes its store or unlocks its data directory.
pub(super) struct JoinWorkers(Arc<Mutex<Vec<JoinHandle<()>>>>);

impl Drop for JoinWorkers {
    fn drop(&mut self) {
        for thread in self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
        {
            let _ = thread.join();
        }
    }
}

impl Workers {
    pub(super) fn new(limit: usize) -> (Self, JoinWorkers) {
        let (send, receive) = mpsc::channel();
        let threads = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                send,
                receive: Arc::new(Mutex::new(receive)),
                threads: threads.clone(),
                permits: Arc::new(Semaphore::new(limit)),
                limit,
            },
            JoinWorkers(threads),
        )
    }

    pub(super) fn release(&self, work: impl FnOnce() + Send + 'static) {
        let _ = self.send.send(Box::new(work));
    }

    pub(super) async fn run<T: Send + 'static>(
        &self,
        key: Arc<KeyWork>,
        work: impl FnOnce(&std::sync::atomic::AtomicBool) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        // A replacement flight waits for an abandoned encoder to unwind
        // before it takes a worker. Distinct keys never share this lock.
        let gate: OwnedMutexGuard<()> = key.gate.clone().lock_owned().await;
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::WriterStopped)?;
        {
            let mut threads = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
            if threads.len() < self.limit - self.permits.available_permits() {
                let receive = self.receive.clone();
                threads.push(
                    thread::Builder::new()
                        .name("transcode".into())
                        .spawn(move || {
                            loop {
                                let task = receive
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .recv();
                                let Ok(task) = task else { break };
                                // Dropping the reply reports a failed worker to every waiter.
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
                            }
                        })
                        .map_err(Error::Io)?,
                );
            }
        }
        let cancel = CancelOnDrop::default();
        let stopped = cancel.0.clone();
        let (reply, result) = oneshot::channel();
        self.send
            .send(Box::new(move || {
                let _held = (key, gate, permit);
                let outcome = super::super::check_cancel(&stopped)
                    .map_err(Error::Io)
                    .and_then(|()| work(&stopped));
                let _ = reply.send(outcome);
            }))
            .map_err(|_| Error::WriterStopped)?;
        result
            .await
            .map_err(|_| Error::Io(std::io::Error::other("transcode worker stopped")))?
    }
}
