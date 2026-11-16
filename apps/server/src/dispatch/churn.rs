//! Developer-only: writing to an invented library while it is served, as a
//! scan would. `ritornel-server serve --invent-churn <per second>` starts
//! it, and it is not in the help.

use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use ritornel_engine::invent::{Churn, ChurnKind, Churned};
use tracing::{debug, info, warn};

use super::Core;
use crate::Error;

impl Core {
    /// One write to the invented library, of this kind, now. Each call
    /// starts from what the library has.
    pub(crate) fn invent_step(&self, kind: ChurnKind) -> Result<Churned, Error> {
        let failed = |why: String| Error::Io {
            doing: "invent",
            path: self.dir.root().to_owned(),
            source: std::io::Error::other(why),
        };
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let stores = stores.as_ref().ok_or(Error::Stopped)?;
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let library = library
            .as_ref()
            .ok_or_else(|| failed("no library".to_owned()))?;
        let mut churn = Churn::start(
            library,
            &stores.server,
            self.clock.now_ms() as u64,
            self.clock.now_ms(),
        )
        .map_err(|error| failed(error.to_string()))?;
        churn
            .take(library, kind)
            .map_err(|error| failed(error.to_string()))
    }

    /// One step, on the library as it is now. `false` once there is no
    /// library to write to.
    fn churn_step(
        &self,
        churn: &mut Option<Churn>,
        steps: &mut u64,
        now_ms: i64,
        started: Instant,
    ) -> bool {
        let stores = self.stores.read().unwrap_or_else(PoisonError::into_inner);
        let Some(stores) = stores.as_ref() else {
            return false;
        };
        let library = stores
            .library
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(library) = library.as_ref() else {
            return false;
        };
        if churn.is_none() {
            match Churn::start(library, &stores.server, 0x00c4_u64, now_ms) {
                Ok(ready) => {
                    info!("writing to the invented library while serving");
                    *churn = Some(ready);
                }
                Err(error) => {
                    warn!(%error, "the invented library cannot be written to");
                    return false;
                }
            }
        }
        if let Some(churn) = churn.as_mut() {
            match churn.step(library) {
                Ok(step) => {
                    *steps += 1;
                    debug!(
                        step = ?step,
                        steps = *steps,
                        ms = started.elapsed().as_secs_f64() * 1000.0,
                        "churned"
                    );
                }
                Err(error) => warn!(%error, "a churn step failed"),
            }
        }
        true
    }

    /// Starts a thread that takes one step of [`Churn`] `per_second` times
    /// a second, until the server's databases close.
    pub(crate) fn invent_churn(self: &Arc<Self>, per_second: u32) -> Result<(), Error> {
        let core = Arc::downgrade(self);
        let interval = Duration::from_secs_f64(1.0 / f64::from(per_second.max(1)));
        let now_ms = self.clock.now_ms();
        std::thread::Builder::new()
            .name("invent churn".to_owned())
            .spawn(move || {
                let mut churn: Option<Churn> = None;
                let mut steps: u64 = 0;
                loop {
                    let started = Instant::now();
                    let Some(core) = core.upgrade() else { return };
                    if !core.churn_step(&mut churn, &mut steps, now_ms, started) {
                        return;
                    }
                    drop(core);
                    std::thread::sleep(interval.saturating_sub(started.elapsed()));
                }
            })
            .map(|_| ())
            .map_err(Error::Runtime)
    }
}
