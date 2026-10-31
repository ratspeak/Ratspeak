//! Closed product status of one RNode driver for its interface row.

use rns_interface::rnode::{
    RNodeCapabilityAdmissionFailureClass, RNodePortFailureClass, RNodeRuntimePhase,
    RNodeRuntimeReason, RNodeRuntimeSnapshot,
};
use rns_interface::rnode_protocol::RNodeReadinessEvidence;

/// Closed product reason for a radio that rejected its settings.
pub fn configuration_rejection_reason(rejected: Option<RNodeReadinessEvidence>) -> &'static str {
    match rejected {
        Some(RNodeReadinessEvidence::TransmitPower) => "rejected_tx_power",
        Some(RNodeReadinessEvidence::Frequency) => "rejected_frequency",
        Some(RNodeReadinessEvidence::Bandwidth) => "rejected_bandwidth",
        Some(RNodeReadinessEvidence::RadioState) => "rejected_radio_state",
        Some(RNodeReadinessEvidence::Firmware) => "unsupported_firmware",
        _ => "rejected_settings",
    }
}

/// Row state and reason while an RNode is not Ready, in the vocabulary the
/// dashboard already uses for Android radios. `None` once Ready or stopping.
pub(crate) fn rnode_row_state(
    snapshot: &RNodeRuntimeSnapshot,
) -> Option<(&'static str, Option<&'static str>)> {
    let state = row_state(
        snapshot.phase,
        snapshot.reconnect_total,
        snapshot.reason,
        snapshot.configuration_rejection,
        snapshot.capability_admission_failure,
        snapshot.port_failure,
    )?;
    Some(match state {
        ("waiting_for_radio", reason) => (
            "waiting_for_radio",
            attempt_failure_reason(snapshot.last_attempt_failure.map(|failure| failure.reason))
                .or(reason),
        ),
        ("initializing", None)
            if snapshot.reason == Some(RNodeRuntimeReason::RadioInitialisationFault) =>
        {
            ("initializing", Some("radio_start_failed"))
        }
        other => other,
    })
}

/// Closed reason for a radio whose last attempt failed in a way it named.
fn attempt_failure_reason(reason: Option<RNodeRuntimeReason>) -> Option<&'static str> {
    match reason? {
        RNodeRuntimeReason::RadioInitialisationFault => Some("radio_start_failed"),
        RNodeRuntimeReason::DeviceUnresponsive => Some("radio_unresponsive"),
        _ => None,
    }
}

/// Closed reason for a port that could not be opened.
pub fn port_failure_reason(failure: Option<RNodePortFailureClass>) -> &'static str {
    match failure {
        Some(RNodePortFailureClass::NotFound) => "port_not_found",
        Some(RNodePortFailureClass::AccessDenied) => "port_access_denied",
        Some(RNodePortFailureClass::InUse) => "port_in_use",
        _ => "not_reachable",
    }
}

fn row_state(
    phase: RNodeRuntimePhase,
    reconnect_total: u64,
    reason: Option<RNodeRuntimeReason>,
    configuration_rejection: Option<RNodeReadinessEvidence>,
    admission_failure: Option<RNodeCapabilityAdmissionFailureClass>,
    port_failure: Option<RNodePortFailureClass>,
) -> Option<(&'static str, Option<&'static str>)> {
    match phase {
        RNodeRuntimePhase::Connecting if reconnect_total == 0 => Some(("connecting", None)),
        RNodeRuntimePhase::Connecting | RNodeRuntimePhase::ReconnectBackoff => Some((
            "waiting_for_radio",
            Some(match reason {
                Some(RNodeRuntimeReason::ConnectionLost) => "link_lost",
                _ => port_failure_reason(port_failure),
            }),
        )),
        RNodeRuntimePhase::AwaitingReadiness => Some(("initializing", None)),
        RNodeRuntimePhase::Stopped => match reason {
            None | Some(RNodeRuntimeReason::StopRequested) => None,
            Some(RNodeRuntimeReason::ConfigurationRejected) => Some((
                "failed",
                Some(configuration_rejection_reason(configuration_rejection)),
            )),
            Some(RNodeRuntimeReason::CapabilityAdmissionRejected) => Some((
                "failed",
                Some(match admission_failure {
                    Some(RNodeCapabilityAdmissionFailureClass::UnsupportedFirmware) => {
                        "unsupported_firmware"
                    }
                    Some(RNodeCapabilityAdmissionFailureClass::RadioSettingsRejected) => {
                        "rejected_settings"
                    }
                    _ => "unsupported_radio",
                }),
            )),
            Some(_) => Some(("failed", Some("connect_failed"))),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_attempt_failures_explain_waiting_rows() {
        use RNodeRuntimeReason::{
            ConnectionAttemptFailed, DeviceUnresponsive, RadioInitialisationFault,
        };
        assert_eq!(
            attempt_failure_reason(Some(DeviceUnresponsive)),
            Some("radio_unresponsive")
        );
        assert_eq!(
            attempt_failure_reason(Some(RadioInitialisationFault)),
            Some("radio_start_failed")
        );
        assert_eq!(attempt_failure_reason(Some(ConnectionAttemptFailed)), None);
        assert_eq!(attempt_failure_reason(None), None);
    }

    #[test]
    fn configuration_rejections_map_to_closed_reasons() {
        use RNodeReadinessEvidence::{Bandwidth, CodingRate, Firmware, TransmitPower};
        assert_eq!(
            configuration_rejection_reason(Some(TransmitPower)),
            "rejected_tx_power"
        );
        assert_eq!(
            configuration_rejection_reason(Some(Bandwidth)),
            "rejected_bandwidth"
        );
        assert_eq!(
            configuration_rejection_reason(Some(Firmware)),
            "unsupported_firmware"
        );
        assert_eq!(
            configuration_rejection_reason(Some(CodingRate)),
            "rejected_settings"
        );
        assert_eq!(configuration_rejection_reason(None), "rejected_settings");
    }

    #[test]
    fn waiting_radios_explain_why_and_ready_ones_say_nothing() {
        use RNodeRuntimePhase::{AwaitingReadiness, Connecting, Ready, ReconnectBackoff};
        use RNodeRuntimeReason::{ConnectionAttemptFailed, ConnectionLost};
        let waiting = |phase, reason, port| row_state(phase, 1, Some(reason), None, None, port);
        assert_eq!(
            row_state(Connecting, 0, None, None, None, None),
            Some(("connecting", None))
        );
        assert_eq!(
            waiting(ReconnectBackoff, ConnectionAttemptFailed, None),
            Some(("waiting_for_radio", Some("not_reachable")))
        );
        assert_eq!(
            waiting(Connecting, ConnectionLost, None),
            Some(("waiting_for_radio", Some("link_lost")))
        );
        assert_eq!(
            waiting(
                ReconnectBackoff,
                ConnectionAttemptFailed,
                Some(RNodePortFailureClass::NotFound)
            ),
            Some(("waiting_for_radio", Some("port_not_found")))
        );
        assert_eq!(
            waiting(
                ReconnectBackoff,
                ConnectionAttemptFailed,
                Some(RNodePortFailureClass::AccessDenied)
            ),
            Some(("waiting_for_radio", Some("port_access_denied")))
        );
        assert_eq!(
            waiting(
                ReconnectBackoff,
                ConnectionAttemptFailed,
                Some(RNodePortFailureClass::InUse)
            ),
            Some(("waiting_for_radio", Some("port_in_use")))
        );
        assert_eq!(
            row_state(AwaitingReadiness, 0, None, None, None, None),
            Some(("initializing", None))
        );
        assert_eq!(row_state(Ready, 0, None, None, None, None), None);
    }

    #[test]
    fn stopped_radios_name_the_rejection_and_a_pause_says_nothing() {
        use RNodeRuntimePhase::Stopped;
        let stopped = |reason, rejected, admission| {
            row_state(Stopped, 0, Some(reason), rejected, admission, None)
        };
        assert_eq!(
            stopped(
                RNodeRuntimeReason::ConfigurationRejected,
                Some(RNodeReadinessEvidence::TransmitPower),
                None
            ),
            Some(("failed", Some("rejected_tx_power")))
        );
        assert_eq!(
            stopped(
                RNodeRuntimeReason::CapabilityAdmissionRejected,
                None,
                Some(RNodeCapabilityAdmissionFailureClass::DetectionRejected)
            ),
            Some(("failed", Some("unsupported_radio")))
        );
        assert_eq!(
            stopped(RNodeRuntimeReason::DriverTerminated, None, None),
            Some(("failed", Some("connect_failed")))
        );
        assert_eq!(stopped(RNodeRuntimeReason::StopRequested, None, None), None);
    }
}
