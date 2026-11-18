//! Interfaces Reticulum detaches since 1.6.1: a server whose tunnel synthesis
//! fails verification, or one whose transport identity is blackholed.
//!
//! rns-runtime carries out each detach; Ratspeak keeps why, for the
//! connection's status line. A saved connection whose server failed
//! verification is attached again from its config after 30 s, 2 min, then
//! every 10 min; a blocked server stays off.

use std::collections::HashMap;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rns_runtime::lifecycle::ShutdownSignal;
use rns_runtime::reticulum::{InterfaceDetach, ReticulumHandle};
use rns_transport::messages::DetachReason;
use tokio::sync::broadcast::error::RecvError;

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

/// Follows this network session's detaches from its start, so one made
/// before this runs is not missed.
pub async fn install(state: &Arc<AppState>, handle: &ReticulumHandle, shutdown: ShutdownSignal) {
    let mut events = handle.subscribe_interface_detaches();
    let session = detaches(state).begin_session();
    let state = Arc::clone(state);
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                event = events.recv() => event,
                () = shutdown.wait() => return,
            };
            match event {
                Ok(detach) => detached(&state, session, &detach),
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

fn detached(state: &Arc<AppState>, session: u64, detach: &InterfaceDetach) {
    tracing::info!(reason = ?detach.reason, "Reticulum detached an interface");
    // A server's accepted client reconnects on its own.
    if detach.parent_id.is_some() {
        return;
    }
    let Some(cause) = DetachCause::from_reason(detach.reason) else {
        return;
    };
    let next = detaches(state).detached(session, &detach.name, cause, Instant::now(), unix_now());
    state.request_poll_now();
    if let Some((attempt, wait)) = next {
        schedule(state, session, &detach.name, attempt, wait);
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

    /// The hub's reply fails verification while the network starts, before
    /// Ratspeak follows its detaches: the row still says why, and it retries.
    #[tokio::test]
    async fn a_detach_while_the_network_starts_reaches_the_connection_row() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let _stamps = crate::announce_stamps::test_policy::unstamped().await;
        let hub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = hub.local_addr().unwrap().port();
        let torn_down = tokio::spawn(async move {
            let (mut stream, _) = hub.accept().await.unwrap();
            let mut reply = rns_transport::tunnel::build_tunnel_synthesis_packet_with_ext(
                &rns_identity::identity::Identity::new(),
                rns_transport::tunnel::interface_hash("TCPInterface[Hub]"),
                &rns_transport::tunnel::TunnelExt::local(true, 0.0, 0.0),
            )
            .unwrap();
            reply[rns_wire::constants::HEADER_MINSIZE + 120] ^= 0x01;
            stream
                .write_all(&rns_interface::hdlc::frame(&reply))
                .await
                .unwrap();
            let _ = stream.read_to_end(&mut Vec::new()).await;
        });
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("rns");
        assert!(crate::rns_config::write_config(
            &config_dir,
            &format!(
                "[reticulum]\nshare_instance = No\n[interfaces]\n[[Hub]]\ntype = TCPClientInterface\nenabled = yes\ntarget_host = 127.0.0.1\ntarget_port = {port}\n"
            ),
        ));
        let runtime = crate::rns::RnsManager::init_with_policy(
            config_dir.to_str().unwrap(),
            None,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
            rns_runtime::shared_instance::InstancePolicy::Standalone,
        )
        .await
        .unwrap();
        crate::announce_stamps::test_policy::unstamp();
        tokio::time::timeout(Duration::from_secs(10), torn_down)
            .await
            .expect("Reticulum detaches the hub")
            .unwrap();

        let pool = crate::db::init_pool(dir.path()).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            crate::config::DashboardConfig::from_env_and_defaults(dir.path().into()),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        install(&state, &runtime.handle, runtime.shutdown.clone()).await;
        let deadline = Instant::now() + Duration::from_secs(10);
        while payload(&state).get("Hub").is_none() {
            assert!(Instant::now() < deadline, "the detach reaches the row");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(payload(&state)["Hub"]["cause"], "unverified");
        assert!(payload(&state)["Hub"]["retry_at"].is_u64());

        runtime.handle.shutdown_and_wait().await;
    }
}
