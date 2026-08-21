//! Shared readiness and exact teardown helpers for runtime-spawned RNodes.
//!
//! Product operations retain ownership of their lifecycle lease. Waiting and
//! teardown are deliberately separate so a stale operation cannot use this
//! adapter to stop a newer interface registration.

use std::fmt;
use std::time::Duration;

use ratspeak_runtime::{PendingRNodeActivityMonitor, RNodeActivityOrigin};
use rns_interface::rnode::{
    RNodeCapabilityAdmissionFailureClass, RNodeRuntimeReason, RNodeTransportClass,
};
use rns_runtime::reticulum::{RNodeReadinessError, ReticulumHandle, SpawnedRNodeRuntime};

use crate::state::AppState;

/// Maximum time a product operation waits for complete RNode protocol
/// readiness. The observer uses one absolute deadline across reconnects.
pub(crate) const RNODE_READINESS_TIMEOUT: Duration = Duration::from_secs(120);

/// A Ready observer is already registered with transport, so this is only a
/// short publication barrier. It prevents the operation's terminal event from
/// racing ahead of the first authoritative stats row without extending the
/// device/pairing timeout materially.
pub(crate) const RNODE_READY_STATS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RnodeReadyStatsPublicationFailure {
    Timeout,
    ObservationLost,
    SessionReplaced,
}

/// Privacy-safe terminal classification for a bounded readiness wait.
///
/// The upstream error carries a last-known snapshot. Command callers should
/// use this classification instead of surfacing that snapshot: no numeric or
/// device-derived snapshot fields may cross this boundary, but the upstream
/// closed typed classifications (runtime reason, capability admission failure
/// class) may be consulted and carried.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RnodeReadinessFailure {
    Timeout,
    ShuttingDown,
    Stopped,
    ObservationClosed,
    ReadyStatsPublication(RnodeReadyStatsPublicationFailure),
    /// The driver terminally rejected the device's capability admission; the
    /// class identifies why, when the driver published one.
    CapabilityAdmissionRejected(Option<RNodeCapabilityAdmissionFailureClass>),
    /// Forward-compatible fallback for new non-exhaustive upstream variants.
    Unclassified,
}

impl fmt::Display for RnodeReadinessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Timeout => "RNode readiness timed out",
            Self::ShuttingDown => "RNode began shutting down before becoming ready",
            Self::Stopped => "RNode stopped before becoming ready",
            Self::ObservationClosed => "RNode readiness observation closed",
            Self::ReadyStatsPublication(RnodeReadyStatsPublicationFailure::Timeout) => {
                "RNode ready stats publication timed out"
            }
            Self::ReadyStatsPublication(RnodeReadyStatsPublicationFailure::ObservationLost) => {
                "RNode left Ready before stats publication"
            }
            Self::ReadyStatsPublication(RnodeReadyStatsPublicationFailure::SessionReplaced) => {
                "RNode session was replaced before stats publication"
            }
            Self::CapabilityAdmissionRejected(_) => "RNode capability admission was rejected",
            Self::Unclassified => "RNode readiness failed",
        })
    }
}

impl std::error::Error for RnodeReadinessFailure {}

/// Reduce the non-exhaustive upstream error to the command layer's stable,
/// snapshot-free outcome vocabulary.
pub(crate) fn classify_rnode_readiness_error(error: &RNodeReadinessError) -> RnodeReadinessFailure {
    match error {
        RNodeReadinessError::Timeout { .. } => RnodeReadinessFailure::Timeout,
        RNodeReadinessError::ShuttingDown { .. } => RnodeReadinessFailure::ShuttingDown,
        RNodeReadinessError::Stopped { .. } => {
            let class = error.capability_admission_failure();
            if class.is_some()
                || error.last_snapshot().reason
                    == Some(RNodeRuntimeReason::CapabilityAdmissionRejected)
            {
                RnodeReadinessFailure::CapabilityAdmissionRejected(class)
            } else {
                RnodeReadinessFailure::Stopped
            }
        }
        RNodeReadinessError::ObservationClosed { .. } => RnodeReadinessFailure::ObservationClosed,
        _ => RnodeReadinessFailure::Unclassified,
    }
}

/// Wait for complete protocol readiness on the observer returned atomically
/// with this exact runtime registration.
///
/// This intentionally never substitutes `SpawnedRNodeRuntime::online` for the
/// exact observer: serial/TCP currently project Ready through that shared flag,
/// but not every RNode transport guarantees identical semantics.
pub(crate) async fn await_spawned_rnode_ready(
    state: &AppState,
    spawned: &SpawnedRNodeRuntime,
    origin: RNodeActivityOrigin,
) -> Result<Option<PendingRNodeActivityMonitor>, RnodeReadinessFailure> {
    debug_assert_eq!(spawned.interface_id, spawned.observer.interface_id());
    // Cover the exact ID before waiting. A failed cover means the originating
    // RNS session was replaced; readiness remains a product concern, but no
    // Activity monitor may be rebased onto the replacement session.
    let covered = state.cover_rnode_activity_interface(spawned.interface_id, origin);
    let ready_snapshot = spawned
        .observer
        .await_ready(RNODE_READINESS_TIMEOUT)
        .await
        .map_err(|error| classify_rnode_readiness_error(&error))?;
    if !covered {
        return Err(RnodeReadinessFailure::ReadyStatsPublication(
            RnodeReadyStatsPublicationFailure::SessionReplaced,
        ));
    }
    if !state.set_rnode_product_readiness(spawned.interface_id, origin, true) {
        return Err(RnodeReadinessFailure::ReadyStatsPublication(
            RnodeReadyStatsPublicationFailure::SessionReplaced,
        ));
    }
    // The stats publication is the first frame that exposes exact Ready
    // to the product. Revision it synchronously so a manual announce from
    // that frame includes the interface transition's semantic change.
    state.bump_announce_interface_revision();
    // Retain the exact observer before attempting the presentation barrier.
    // Stats publication is deliberately not a device-lifecycle authority: a
    // slow query or a reconnect immediately after first Ready must not tear
    // down the healthy runtime or discard its monitor.
    let pending_monitor =
        PendingRNodeActivityMonitor::new(spawned.observer.clone(), ready_snapshot, origin);
    match publish_exact_ready_stats(state, spawned, origin).await {
        Ok(()) => {}
        Err(RnodeReadinessFailure::ReadyStatsPublication(
            RnodeReadyStatsPublicationFailure::Timeout,
        )) => {
            tracing::warn!(
                interface_id = spawned.interface_id,
                "RNode reached Ready before its eager stats publication completed"
            );
        }
        Err(RnodeReadinessFailure::ReadyStatsPublication(
            RnodeReadyStatsPublicationFailure::ObservationLost,
        )) => {
            tracing::info!(
                interface_id = spawned.interface_id,
                "RNode entered reconnect after first Ready; regular stats publication will resume"
            );
        }
        Err(failure) => return Err(failure),
    }
    Ok(Some(pending_monitor))
}

/// Force and await the first normal stats publication for the exact Ready
/// registration. The authoritative row comes from Reticulum transport; the
/// runtime state applies its exact product-readiness overlay and emits the same
/// `stats_update` payload used by the regular poll loop.
async fn publish_exact_ready_stats(
    state: &AppState,
    spawned: &SpawnedRNodeRuntime,
    origin: RNodeActivityOrigin,
) -> Result<(), RnodeReadinessFailure> {
    let Some(handle) = state.rnode_activity_handle_for_origin(spawned.interface_id, origin) else {
        return ready_stats_publication_failure(
            state,
            spawned.interface_id,
            origin,
            RnodeReadyStatsPublicationFailure::SessionReplaced,
        );
    };
    let deadline = tokio::time::Instant::now() + RNODE_READY_STATS_TIMEOUT;

    loop {
        if spawned.observer.snapshot().phase != rns_interface::rnode::RNodeRuntimePhase::Ready {
            return ready_stats_publication_failure(
                state,
                spawned.interface_id,
                origin,
                RnodeReadyStatsPublicationFailure::ObservationLost,
            );
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return ready_stats_publication_failure(
                state,
                spawned.interface_id,
                origin,
                RnodeReadyStatsPublicationFailure::Timeout,
            );
        }
        let response = tokio::time::timeout(
            remaining,
            handle.query_control(rns_transport::messages::TransportQuery::GetInterfaceStats),
        )
        .await
        .map_err(|_| {
            RnodeReadinessFailure::ReadyStatsPublication(RnodeReadyStatsPublicationFailure::Timeout)
        })?;
        // `query_control` may outlive this exact protocol generation. Never
        // overlay the product-ready bit onto a row after the observer has
        // left Ready, even when the transport query itself completed.
        if spawned.observer.snapshot().phase != rns_interface::rnode::RNodeRuntimePhase::Ready {
            return ready_stats_publication_failure(
                state,
                spawned.interface_id,
                origin,
                RnodeReadyStatsPublicationFailure::ObservationLost,
            );
        }
        if let Some(rns_transport::messages::TransportQueryResponse::InterfaceStats(stats)) =
            response
        {
            if state.publish_ready_rnode_interface_stats(
                spawned.interface_id,
                origin,
                handle.instance_mode,
                &stats,
            ) {
                return Ok(());
            }
        }

        if state
            .rnode_activity_handle_for_origin(spawned.interface_id, origin)
            .is_none()
        {
            return ready_stats_publication_failure(
                state,
                spawned.interface_id,
                origin,
                RnodeReadyStatsPublicationFailure::SessionReplaced,
            );
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn ready_stats_publication_failure(
    state: &AppState,
    interface_id: rns_interface::traits::InterfaceId,
    origin: RNodeActivityOrigin,
    failure: RnodeReadyStatsPublicationFailure,
) -> Result<(), RnodeReadinessFailure> {
    let _ = state.set_rnode_product_readiness(interface_id, origin, false);
    Err(RnodeReadinessFailure::ReadyStatsPublication(failure))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RnodeTeardownRoute {
    Ble,
    AndroidUsb,
    SerialOrTcp,
    Generic,
}

fn teardown_route(transport: RNodeTransportClass) -> RnodeTeardownRoute {
    match transport {
        RNodeTransportClass::Ble => RnodeTeardownRoute::Ble,
        RNodeTransportClass::Usb => RnodeTeardownRoute::AndroidUsb,
        RNodeTransportClass::Serial | RNodeTransportClass::Tcp => RnodeTeardownRoute::SerialOrTcp,
        _ => RnodeTeardownRoute::Generic,
    }
}

/// Tear down the exact runtime registration represented by `spawned`.
///
/// Callers must validate their product-operation lease before invoking this
/// function. Unsupported transport-specific helpers fall back to generic
/// exact-ID teardown, keeping this module buildable under every feature set.
pub(crate) async fn teardown_spawned_rnode_exact(
    handle: &ReticulumHandle,
    spawned: &SpawnedRNodeRuntime,
) {
    debug_assert_eq!(spawned.interface_id, spawned.observer.interface_id());
    let interface_id = spawned.interface_id;
    let route = teardown_route(spawned.observer.snapshot().transport);

    match route {
        RnodeTeardownRoute::Ble => {
            #[cfg(feature = "ble")]
            {
                rns_runtime::reticulum::teardown_ble_rnode_interface(handle, interface_id).await;
                return;
            }
        }
        RnodeTeardownRoute::AndroidUsb => {
            #[cfg(target_os = "android")]
            {
                rns_runtime::reticulum::teardown_android_usb_rnode_interface(handle, interface_id)
                    .await;
                return;
            }
        }
        RnodeTeardownRoute::SerialOrTcp => {
            #[cfg(any(feature = "serial", feature = "rnode-tcp"))]
            {
                rns_runtime::reticulum::teardown_rnode_interface(handle, interface_id).await;
                return;
            }
        }
        RnodeTeardownRoute::Generic => {}
    }

    rns_runtime::reticulum::teardown_interface(handle, interface_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_timeout_is_named_and_bounded() {
        assert_eq!(RNODE_READINESS_TIMEOUT, Duration::from_secs(120));
    }

    #[test]
    fn readiness_failure_labels_are_stable_and_snapshot_free() {
        assert_eq!(
            RnodeReadinessFailure::Timeout.to_string(),
            "RNode readiness timed out"
        );
        assert_eq!(
            RnodeReadinessFailure::ShuttingDown.to_string(),
            "RNode began shutting down before becoming ready"
        );
        assert_eq!(
            RnodeReadinessFailure::Stopped.to_string(),
            "RNode stopped before becoming ready"
        );
        assert_eq!(
            RnodeReadinessFailure::ObservationClosed.to_string(),
            "RNode readiness observation closed"
        );
        assert_eq!(
            RnodeReadinessFailure::ReadyStatsPublication(
                RnodeReadyStatsPublicationFailure::Timeout
            )
            .to_string(),
            "RNode ready stats publication timed out"
        );
        assert_eq!(
            RnodeReadinessFailure::ReadyStatsPublication(
                RnodeReadyStatsPublicationFailure::ObservationLost
            )
            .to_string(),
            "RNode left Ready before stats publication"
        );
        assert_eq!(
            RnodeReadinessFailure::ReadyStatsPublication(
                RnodeReadyStatsPublicationFailure::SessionReplaced
            )
            .to_string(),
            "RNode session was replaced before stats publication"
        );
        assert_eq!(
            RnodeReadinessFailure::CapabilityAdmissionRejected(None).to_string(),
            "RNode capability admission was rejected"
        );
        assert_eq!(
            RnodeReadinessFailure::Unclassified.to_string(),
            "RNode readiness failed"
        );
    }

    #[test]
    fn ready_stats_publication_timeout_is_short_and_bounded() {
        assert_eq!(RNODE_READY_STATS_TIMEOUT, Duration::from_secs(3));
        assert!(RNODE_READY_STATS_TIMEOUT < RNODE_READINESS_TIMEOUT);
    }

    #[test]
    fn teardown_routes_cover_all_current_rnode_transports() {
        assert_eq!(
            teardown_route(RNodeTransportClass::Ble),
            RnodeTeardownRoute::Ble
        );
        assert_eq!(
            teardown_route(RNodeTransportClass::Usb),
            RnodeTeardownRoute::AndroidUsb
        );
        assert_eq!(
            teardown_route(RNodeTransportClass::Serial),
            RnodeTeardownRoute::SerialOrTcp
        );
        assert_eq!(
            teardown_route(RNodeTransportClass::Tcp),
            RnodeTeardownRoute::SerialOrTcp
        );
    }
}
