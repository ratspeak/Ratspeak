//! Interfaces Reticulum detaches since 1.6.1: a server whose tunnel synthesis
//! fails verification, or one whose transport identity is blackholed.
//!
//! Ratspeak takes the transport's detach requests, carries each out as
//! rns-runtime does, and keeps why, for the connection's status line. A saved
//! connection whose server failed verification is attached again from its
//! config after 30 s, 2 min, then every 10 min; a blocked server stays off.

use std::collections::HashMap;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::ReticulumHandle;
use rns_transport::messages::{
    DetachReason, InterfaceControl, InterfaceRole, InterfaceStatRpcEntry, TransportMessage,
    TransportQuery, TransportQueryResponse,
};
use tokio::sync::mpsc;

use crate::state::AppState;

/// The waits before each re-attach; the last repeats.
pub const REATTACH_DELAYS: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// Up this long after a re-attach, the next detach starts the waits over.
const SETTLED: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachCause {
    /// The server's reply failed verification; tried again later.
    Unverified,
    /// The server's transport identity is blackholed; stays off.
    Blocked,
}

impl DetachCause {
    fn from_reason(reason: DetachReason) -> Option<Self> {
        match reason {
            DetachReason::InvalidSynthesis => Some(Self::Unverified),
            DetachReason::BlackholedTransport => Some(Self::Blocked),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Blocked => "blocked",
        }
    }
}

/// A re-attach now due, for the shell, which spawns interfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReattachRequest {
    pub name: String,
    session: u64,
    attempt: u32,
}

#[derive(Debug, Clone)]
struct Detached {
    cause: DetachCause,
    /// Detaches in a row; the first is 1.
    attempt: u32,
    /// Unix seconds of the next re-attach.
    retry_at: Option<u64>,
    /// No interface runs for it now.
    down: bool,
    reattached: Option<Instant>,
}

/// Detached interfaces of the running network, by configured name.
#[derive(Debug, Default)]
pub struct InterfaceDetaches {
    session: u64,
    entries: HashMap<String, Detached>,
}

impl InterfaceDetaches {
    fn begin_session(&mut self) -> u64 {
        self.session += 1;
        self.entries.clear();
        self.session
    }

    /// Records a detach; the attempt and wait of its re-attach, if any.
    fn detached(
        &mut self,
        session: u64,
        name: &str,
        cause: DetachCause,
        now: Instant,
        wall: u64,
    ) -> Option<(u32, Duration)> {
        if session != self.session {
            return None;
        }
        let attempt = match self.entries.get(name) {
            Some(previous)
                if previous
                    .reattached
                    .is_none_or(|at| now.saturating_duration_since(at) < SETTLED) =>
            {
                previous.attempt.saturating_add(1)
            }
            _ => 1,
        };
        let wait = (cause == DetachCause::Unverified).then(|| {
            let index = usize::try_from(attempt - 1)
                .unwrap_or(usize::MAX)
                .min(REATTACH_DELAYS.len() - 1);
            REATTACH_DELAYS[index]
        });
        self.entries.insert(
            name.to_string(),
            Detached {
                cause,
                attempt,
                retry_at: wait.map(|wait| wall.saturating_add(wait.as_secs())),
                down: true,
                reattached: None,
            },
        );
        wait.map(|wait| (attempt, wait))
    }

    fn due(&self, request: &ReattachRequest) -> bool {
        request.session == self.session
            && self.entries.get(&request.name).is_some_and(|entry| {
                entry.down
                    && entry.attempt == request.attempt
                    && entry.cause == DetachCause::Unverified
            })
    }

    fn reattached(&mut self, request: &ReattachRequest, now: Instant) {
        if self.due(request)
            && let Some(entry) = self.entries.get_mut(&request.name)
        {
            entry.down = false;
            entry.retry_at = None;
            entry.reattached = Some(now);
        }
    }

    fn forget(&mut self, name: &str) {
        self.entries.remove(name);
    }

    /// `{name: {cause, retry_at}}` for the connections that are down.
    fn payload(&self) -> serde_json::Value {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.down)
            .map(|(name, entry)| {
                (
                    name.clone(),
                    serde_json::json!({ "cause": entry.cause.as_str(), "retry_at": entry.retry_at }),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

fn detaches(state: &AppState) -> MutexGuard<'_, InterfaceDetaches> {
    state
        .interface_detaches
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// For the interface stats payload.
pub fn payload(state: &AppState) -> serde_json::Value {
    detaches(state).payload()
}

/// The request is still the one to carry out.
pub fn due(state: &AppState, request: &ReattachRequest) -> bool {
    detaches(state).due(request)
}

/// The connection runs again.
pub fn reattached(state: &AppState, request: &ReattachRequest) {
    detaches(state).reattached(request, Instant::now());
    state.request_poll_now();
}

/// It could not start; the next wait follows.
pub fn reattach_failed(state: &Arc<AppState>, request: &ReattachRequest) {
    let next = {
        let mut detaches = detaches(state);
        if !detaches.due(request) {
            return;
        }
        detaches.detached(
            request.session,
            &request.name,
            DetachCause::Unverified,
            Instant::now(),
            unix_now(),
        )
    };
    if let Some((attempt, wait)) = next {
        schedule(state, request.session, &request.name, attempt, wait);
    }
}

/// The user resumed, paused, or removed it: nothing is retried.
pub fn forget(state: &AppState, name: &str) {
    detaches(state).forget(name);
}

fn schedule(state: &Arc<AppState>, session: u64, name: &str, attempt: u32, wait: Duration) {
    let state = Arc::clone(state);
    let request = ReattachRequest {
        name: name.to_string(),
        session,
        attempt,
    };
    tokio::spawn(async move {
        tokio::time::sleep(wait).await;
        if due(&state, &request) {
            let _ = state.interface_reattach_tx.send(request);
        }
    });
}

fn short_name(entry: &InterfaceStatRpcEntry) -> &str {
    entry
        .identity
        .as_ref()
        .map_or(entry.name.as_str(), |identity| identity.short_name.as_str())
}

/// As rns-runtime: I2P listeners and local interfaces stay.
fn kept(entry: &InterfaceStatRpcEntry) -> bool {
    let i2p = entry
        .identity
        .as_ref()
        .is_some_and(|identity| identity.class_name == "I2PInterface");
    let local = [
        InterfaceRole::SharedServer,
        InterfaceRole::LocalClient,
        InterfaceRole::SharedInstancePeer,
    ]
    .iter()
    .any(|role| role.as_str() == entry.role);
    i2p || local
}

/// Takes the transport's detach requests for this network session, in place
/// of rns-runtime's own handler, which then stops.
pub async fn install(state: &Arc<AppState>, handle: &ReticulumHandle, shutdown: ShutdownSignal) {
    let (tx, mut requests) = mpsc::unbounded_channel();
    if handle
        .transport_tx
        .send(TransportMessage::SetInterfaceControl { tx })
        .await
        .is_err()
    {
        return;
    }
    let session = detaches(state).begin_session();
    let state = Arc::clone(state);
    let handle = handle.clone();
    tokio::spawn(async move {
        loop {
            let request = tokio::select! {
                request = requests.recv() => request,
                () = shutdown.wait() => None,
            };
            match request {
                Some(InterfaceControl::Detach {
                    interface_id,
                    reason,
                }) => detach(&state, &handle, session, interface_id, reason).await,
                Some(_) => {}
                None => return,
            }
        }
    });
}

async fn detach(
    state: &Arc<AppState>,
    handle: &ReticulumHandle,
    session: u64,
    interface_id: rns_transport::messages::InterfaceId,
    reason: DetachReason,
) {
    let Some(TransportQueryResponse::InterfaceStats(entries)) = handle
        .query_transport(TransportQuery::GetInterfaceStats)
        .await
    else {
        return;
    };
    let Some(entry) = entries.iter().find(|entry| entry.id == interface_id) else {
        return;
    };
    if kept(entry) {
        return;
    }
    let name = short_name(entry).to_string();
    tracing::info!(?reason, "Reticulum detached an interface");
    // By name, the runtime's own path; an ambiguous name stops just this one.
    let unique = entries.iter().filter(|e| short_name(e) == name).count() == 1;
    let detached = entry.parent_id.is_none()
        && unique
        && matches!(handle.detach_interface(&name).await, Ok(Some(true)));
    if !detached {
        for child in entries.iter().filter(|e| e.parent_id == Some(interface_id)) {
            rns_runtime::reticulum::teardown_interface(handle, child.id).await;
        }
        rns_runtime::reticulum::teardown_interface(handle, interface_id).await;
    }
    // A server's accepted client reconnects on its own.
    if entry.parent_id.is_some() {
        return;
    }
    let Some(cause) = DetachCause::from_reason(reason) else {
        return;
    };
    let next = detaches(state).detached(session, &name, cause, Instant::now(), unix_now());
    state.request_poll_now();
    if let Some((attempt, wait)) = next {
        schedule(state, session, &name, attempt, wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, session: u64, attempt: u32) -> ReattachRequest {
        ReattachRequest {
            name: name.to_string(),
            session,
            attempt,
        }
    }

    #[test]
    fn a_failed_verification_waits_30_s_2_min_then_10_min() {
        let mut detaches = InterfaceDetaches::default();
        let session = detaches.begin_session();
        let start = Instant::now();
        let mut waits = Vec::new();
        for attempt in 1..=5 {
            let (n, wait) = detaches
                .detached(session, "Hub", DetachCause::Unverified, start, 1_000)
                .unwrap();
            assert_eq!(n, attempt);
            assert!(detaches.due(&request("Hub", session, n)));
            assert!(!detaches.due(&request("Hub", session, n + 1)));
            assert_eq!(
                detaches.payload()["Hub"],
                serde_json::json!({ "cause": "unverified", "retry_at": 1_000 + wait.as_secs() })
            );
            detaches.reattached(&request("Hub", session, n), start);
            assert!(detaches.payload().get("Hub").is_none(), "up again");
            waits.push(wait.as_secs());
        }
        assert_eq!(waits, [30, 120, 600, 600, 600]);

        // Up for 10 minutes, a new failure starts the waits over.
        let later = start + SETTLED;
        let (attempt, wait) = detaches
            .detached(session, "Hub", DetachCause::Unverified, later, 2_000)
            .unwrap();
        assert_eq!((attempt, wait), (1, REATTACH_DELAYS[0]));
    }

    #[test]
    fn a_blocked_server_stays_off() {
        let mut detaches = InterfaceDetaches::default();
        let session = detaches.begin_session();
        assert_eq!(
            detaches.detached(session, "Hub", DetachCause::Blocked, Instant::now(), 5),
            None
        );
        assert!(!detaches.due(&request("Hub", session, 1)));
        assert_eq!(
            detaches.payload(),
            serde_json::json!({ "Hub": { "cause": "blocked", "retry_at": null } })
        );
        detaches.forget("Hub");
        assert_eq!(detaches.payload(), serde_json::json!({}));
    }

    #[test]
    fn a_new_network_session_drops_every_detach_and_its_retries() {
        let mut detaches = InterfaceDetaches::default();
        let first = detaches.begin_session();
        detaches
            .detached(first, "Hub", DetachCause::Unverified, Instant::now(), 0)
            .unwrap();
        let second = detaches.begin_session();
        assert!(!detaches.due(&request("Hub", first, 1)));
        assert_eq!(detaches.payload(), serde_json::json!({}));
        // A detach the old session reports late is not recorded.
        assert_eq!(
            detaches.detached(first, "Hub", DetachCause::Unverified, Instant::now(), 0),
            None
        );
        assert!(detaches.payload().get("Hub").is_none());
        assert!(
            detaches
                .detached(second, "Hub", DetachCause::Unverified, Instant::now(), 0)
                .is_some()
        );
    }

    #[test]
    fn every_detach_reason_is_named() {
        assert_eq!(
            DetachCause::from_reason(DetachReason::InvalidSynthesis),
            Some(DetachCause::Unverified)
        );
        assert_eq!(
            DetachCause::from_reason(DetachReason::BlackholedTransport),
            Some(DetachCause::Blocked)
        );
    }
}
