//! Shared readiness and exact teardown helpers for runtime-spawned RNodes.
//!
//! Product operations retain ownership of their lifecycle lease. Waiting and
//! teardown are deliberately separate so a stale operation cannot use this
//! adapter to stop a newer interface registration.

use std::fmt;
use std::time::Duration;

use rns_interface::rnode::RNodeTransportClass;
use rns_runtime::reticulum::{RNodeReadinessError, ReticulumHandle, SpawnedRNodeRuntime};

/// Maximum time a product operation waits for complete RNode protocol
/// readiness. The observer uses one absolute deadline across reconnects.
pub(crate) const RNODE_READINESS_TIMEOUT: Duration = Duration::from_secs(120);

/// Privacy-safe terminal classification for a bounded readiness wait.
///
/// The upstream error carries a last-known snapshot. Command callers should
/// use this classification instead of surfacing that snapshot or depending on
/// its current fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RnodeReadinessFailure {
    Timeout,
    ShuttingDown,
    Stopped,
    ObservationClosed,
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
        RNodeReadinessError::Stopped { .. } => RnodeReadinessFailure::Stopped,
        RNodeReadinessError::ObservationClosed { .. } => RnodeReadinessFailure::ObservationClosed,
        _ => RnodeReadinessFailure::Unclassified,
    }
}

/// Wait for complete protocol readiness on the observer returned atomically
/// with this exact runtime registration.
///
/// This intentionally never reads `SpawnedRNodeRuntime::online`, which is a
/// legacy enabled/connect flag rather than authoritative readiness.
pub(crate) async fn await_spawned_rnode_ready(
    spawned: &SpawnedRNodeRuntime,
) -> Result<(), RnodeReadinessFailure> {
    debug_assert_eq!(spawned.interface_id, spawned.observer.interface_id());
    spawned
        .observer
        .await_ready(RNODE_READINESS_TIMEOUT)
        .await
        .map(|_| ())
        .map_err(|error| classify_rnode_readiness_error(&error))
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
            RnodeReadinessFailure::Unclassified.to_string(),
            "RNode readiness failed"
        );
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
