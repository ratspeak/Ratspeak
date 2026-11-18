//! Destination stamps for the announces Ratspeak builds itself. Generation runs
//! on the shared Reticulum stamper; callers poll instead of waiting on it.
//! Each destination's stamp is also made as soon as it exists, so its first
//! announce need not wait, and the window can say when one is still being made.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use rns_identity::announce::AnnounceStampExt;
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_runtime::destination_stamper::{Admission, DestinationStamps};
use serde_json::{Value, json};

use crate::state::AppState;

pub(crate) const STAMP_EVENT: &str = "announce_stamp";

/// The stamps being made for this identity's presence announces.
#[derive(Default)]
pub(crate) struct Preparation {
    round: u64,
    identity: Option<String>,
    started: bool,
    working: usize,
    since: Option<Instant>,
    in_flight: HashSet<[u8; 16]>,
    jobs: Vec<tokio::task::JoinHandle<()>>,
}

impl Preparation {
    fn status(&self) -> Value {
        let state = if self.working > 0 {
            "working"
        } else if self.started {
            "ready"
        } else {
            "idle"
        };
        let elapsed_ms = self
            .since
            .map_or(0, |since| since.elapsed().as_millis() as u64);
        json!({ "state": state, "elapsed_ms": elapsed_ms })
    }
}

impl AppState {
    /// `idle` before an identity's network starts, then `working` while a
    /// presence announce's stamp is still being made, else `ready`.
    pub fn announce_stamp_status(&self) -> Value {
        self.announce_stamp
            .lock()
            .map(|prep| prep.status())
            .unwrap_or_else(|_| json!({ "state": "ready", "elapsed_ms": 0 }))
    }
}

/// This identity's stamp jobs; another identity's are cancelled. The same
/// identity keeps its jobs, so a start after setup finds setup's stamp.
pub(crate) fn begin(state: &AppState, identity_hash: &str) -> u64 {
    let Ok(mut prep) = state.announce_stamp.lock() else {
        return 0;
    };
    if prep.identity.as_deref() == Some(identity_hash) {
        return prep.round;
    }
    for job in prep.jobs.drain(..) {
        job.abort();
    }
    prep.round += 1;
    prep.identity = Some(identity_hash.to_string());
    prep.started = true;
    prep.working = 0;
    prep.since = None;
    prep.in_flight.clear();
    state.emitter.emit(STAMP_EVENT, prep.status());
    prep.round
}

/// Set up the stamper as this identity's network start will, before that start.
pub(crate) fn configure_for_identity(state: &AppState, identity_hash: &str) -> bool {
    let config_dir = crate::rns_config_dir_for_start(state, Some(identity_hash));
    match rns_runtime::reticulum::configure_destination_stamper(
        config_dir.to_str(),
        &crate::rns::init_options(),
    ) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "could not set up destination stamps before the network");
            false
        }
    }
}

/// Make an identity's presence stamps now; `configure_for_identity` first.
pub(crate) fn start_for_identity(
    state: &Arc<AppState>,
    identity_hash: &str,
    identity: &Identity,
    destinations: &[&Destination],
) {
    let round = begin(state, identity_hash);
    for destination in destinations {
        prepare(state, round, destination, identity, true);
    }
}

pub(crate) fn current_round(state: &AppState) -> u64 {
    state.announce_stamp.lock().map_or(0, |prep| prep.round)
}

/// Make `destination`'s stamp now. `presence` stamps hold the user's
/// delivery announce, so the window reports them; the stamp goes into the LXMF
/// manager's matching destination once made.
pub(crate) fn prepare(
    state: &Arc<AppState>,
    round: u64,
    destination: &Destination,
    identity: &Identity,
    presence: bool,
) {
    let hash = destination.hash;
    let Ok(mut prep) = state.announce_stamp.lock() else {
        return;
    };
    // One search per destination: a later start waits on the one in flight.
    if prep.round != round || prep.in_flight.contains(&hash) {
        return;
    }
    let Some(prepared) = rns_runtime::destination_stamper::prepare(destination, identity) else {
        return;
    };
    prep.in_flight.insert(hash);
    if presence {
        prep.working += 1;
        prep.since.get_or_insert_with(Instant::now);
        state.emitter.emit(STAMP_EVENT, prep.status());
    }
    let task_state = Arc::clone(state);
    prep.jobs.push(tokio::spawn(async move {
        let stamp = prepared.ready().await;
        if let (Some(stamp), Ok(mut lxmf)) = (stamp, task_state.lxmf.lock())
            && let Some(mgr) = lxmf.as_mut()
        {
            mgr.install_destination_stamp(&hash, stamp);
        }
        let Ok(mut prep) = task_state.announce_stamp.lock() else {
            return;
        };
        if prep.round != round {
            return;
        }
        prep.in_flight.remove(&hash);
        if presence {
            prep.working = prep.working.saturating_sub(1);
            if prep.working == 0 {
                prep.since = None;
            }
            task_state.emitter.emit(STAMP_EVENT, prep.status());
        }
    }));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampWait {
    /// The stamp is being generated; ask again later.
    Pending,
    /// Generation failed or the stamper queue was full: this announce is cancelled.
    Failed,
}

#[derive(Default)]
pub(crate) struct AnnounceStamps {
    stamps: DestinationStamps<()>,
    pending: bool,
    promoted: bool,
}

impl AnnounceStamps {
    /// `Destination.announce` dispatch for one destination, without blocking:
    /// the extension to append (`None` when the policy does not stamp).
    pub(crate) fn gate(
        &mut self,
        destination: &mut Destination,
        identity: &Identity,
        path_response: bool,
    ) -> Result<Option<AnnounceStampExt>, StampWait> {
        if self.pending {
            let Some(outcome) = self.stamps.try_recv() else {
                if path_response && !self.promoted {
                    // Path responses take the stamper's priority queue.
                    self.promoted = true;
                    let _ = self.stamps.admit(destination, identity, true, ());
                }
                return Err(StampWait::Pending);
            };
            self.pending = false;
            self.promoted = false;
            let failed = outcome.stamp.is_none();
            self.stamps.complete(destination, identity, outcome);
            if failed {
                return Err(StampWait::Failed);
            }
        }
        match self.stamps.admit(destination, identity, path_response, ()) {
            Admission::Ready((), ext) => Ok(ext),
            Admission::Queued => {
                self.pending = true;
                Err(StampWait::Pending)
            }
            Admission::Dropped(()) => Err(StampWait::Failed),
        }
    }
}

/// Tests share the process stamp policy, and a value-25 destination stamp is
/// too slow to wait for in a debug build. A test that builds announces or
/// starts a network holds this guard for its run: one at a time, destination
/// stamping off. A start turns stamping back on; `unstamp` after it.
#[cfg(test)]
pub(crate) mod test_policy {
    use rns_transport::stamps::StampPolicy;
    use tokio::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::const_new(());

    pub(crate) type Guard = MutexGuard<'static, ()>;

    pub(crate) fn unstamp() {
        rns_runtime::destination_stamper::configure(
            StampPolicy {
                stamp_destinations: false,
                ..StampPolicy::default()
            },
            None,
        );
    }

    pub(crate) fn blocking_unstamped() -> Guard {
        let guard = LOCK.blocking_lock();
        unstamp();
        guard
    }

    pub(crate) async fn unstamped() -> Guard {
        let guard = LOCK.lock().await;
        unstamp();
        guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn status_is_idle_until_a_session_starts_then_working_or_ready() {
        let mut prep = Preparation::default();
        assert_eq!(prep.status()["state"], "idle");
        prep.started = true;
        assert_eq!(prep.status(), json!({ "state": "ready", "elapsed_ms": 0 }));
        prep.working = 1;
        prep.since = Instant::now().checked_sub(Duration::from_millis(1500));
        let status = prep.status();
        assert_eq!(status["state"], "working");
        assert!(status["elapsed_ms"].as_u64().unwrap() >= 1500);
    }
}
