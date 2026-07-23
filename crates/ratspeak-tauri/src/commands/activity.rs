//! Typed, bounded Activity recorder commands.
//!
//! This module is the only WebView-to-recorder parsing boundary. Sequence and
//! cursor values stay canonical decimal strings until they have crossed that
//! boundary, and all recorder failures are translated to static application
//! errors so internal error text never reaches IPC.

use std::sync::Arc;
use std::time::Duration;

use ratspeak_runtime::activity::{
    ACTIVITY_REPLAY_MAX_BYTES, ACTIVITY_REPLAY_MAX_EVENTS, ACTIVITY_REPLAY_MIN_BYTES,
    ActivityAttributeKey, ActivityDetailResultV1, ActivityIpcResponse, ActivityRecorderError,
    ActivityReplayResultV1, ActivityRevealResultV1, ActivitySafeCopyResultV1, ActivityStatusV1,
    CaptureProfile, TraceCaptureDuration,
};
use serde::Deserialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::{ActivityRequestFence, AppState};

/// Defensive IPC ceiling for an explicitly finite Trace. Platform default is
/// still ten minutes on mobile and Until stopped on desktop.
const MAX_LIMITED_TRACE_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReplayArgs {
    pub capture_session: String,
    #[serde(default)]
    pub after: Option<String>,
    pub max_events: usize,
    pub max_bytes: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEventArgs {
    pub capture_session: String,
    pub sequence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRevealArgs {
    pub capture_session: String,
    pub sequence: String,
    pub field: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySetProfileArgs {
    pub profile: String,
    /// Omission is identical to `"platform_default"`.
    #[serde(default)]
    pub duration: Option<ActivityTraceDurationArgs>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ActivityTraceDurationArgs {
    /// `platform_default` or `until_stopped`.
    Mode(String),
    /// `{ "limited": { "seconds": "600" } }`.
    Limited(ActivityLimitedTraceEnvelope),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityLimitedTraceEnvelope {
    pub limited: ActivityLimitedTraceArgs,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityLimitedTraceArgs {
    /// Canonical decimal-string u64 in the inclusive range 1 second to 24 hours.
    pub seconds: String,
}

#[tauri::command]
pub async fn activity_status(state: State<'_, Arc<AppState>>) -> AppResult<ActivityStatusV1> {
    // Status includes Trace deadline metadata published by profile changes, so
    // it shares the identity/activity lock order with mutating commands. It is
    // an observation, not queued work: waiting across a transition and reading
    // the resulting current status is safe and keeps initial bootstrap robust.
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    Ok(state.activity.status())
}

#[tauri::command]
pub async fn activity_start(state: State<'_, Arc<AppState>>) -> AppResult<ActivityStatusV1> {
    let request_fence = state.activity_request_fence();
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    ensure_activity_request_fence(&state, request_fence)?;
    let status = state.activity.start().await.map_err(map_recorder_error)?;
    publish_legacy_compatibility(&state, &status, true, "standard");
    Ok(status)
}

#[tauri::command]
pub async fn activity_stop(state: State<'_, Arc<AppState>>) -> AppResult<ActivityStatusV1> {
    let request_fence = state.activity_request_fence();
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    ensure_activity_request_fence(&state, request_fence)?;
    state
        .network_log_enabled
        .store(false, std::sync::atomic::Ordering::Release);
    let status = state.activity.stop().await.map_err(map_recorder_error)?;
    publish_legacy_compatibility(&state, &status, false, "standard");
    Ok(status)
}

#[tauri::command]
pub async fn activity_resume(state: State<'_, Arc<AppState>>) -> AppResult<ActivityStatusV1> {
    let request_fence = state.activity_request_fence();
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    ensure_activity_request_fence(&state, request_fence)?;
    let status = state.activity.resume().await.map_err(map_recorder_error)?;
    publish_legacy_compatibility(&state, &status, true, "standard");
    Ok(status)
}

#[tauri::command]
pub async fn activity_set_profile(
    state: State<'_, Arc<AppState>>,
    args: ActivitySetProfileArgs,
) -> AppResult<ActivityStatusV1> {
    let request_fence = state.activity_request_fence();
    let (profile, trace_duration) = parse_profile(args)?;
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    ensure_activity_request_fence(&state, request_fence)?;
    let status = state
        .activity
        .set_profile(profile, trace_duration)
        .await
        .map_err(map_recorder_error)?;
    publish_legacy_compatibility(
        &state,
        &status,
        true,
        if profile == CaptureProfile::Trace {
            "detailed"
        } else {
            "standard"
        },
    );
    Ok(status)
}

#[tauri::command]
pub async fn activity_replay(
    state: State<'_, Arc<AppState>>,
    args: ActivityReplayArgs,
) -> AppResult<ActivityIpcResponse<ActivityReplayResultV1>> {
    validate_replay_limits(args.max_events, args.max_bytes)?;
    let after = args
        .after
        .as_deref()
        .map(|value| parse_decimal_u64(value, true))
        .transpose()?;
    state
        .activity
        .replay_for_ipc(args.capture_session, after, args.max_events, args.max_bytes)
        .await
        .map_err(map_recorder_error)
}

#[tauri::command]
pub async fn activity_clear(state: State<'_, Arc<AppState>>) -> AppResult<ActivityStatusV1> {
    let request_fence = state.activity_request_fence();
    let _identity_lifecycle = state.identity_switch_lock.lock().await;
    let _activity_control = state.activity_control_lock.lock().await;
    ensure_activity_request_fence(&state, request_fence)?;
    let legacy_was_enabled = state
        .network_log_enabled
        .swap(false, std::sync::atomic::Ordering::AcqRel);
    let status = state.activity.clear().await.map_err(map_recorder_error)?;
    let legacy_enabled = legacy_was_enabled
        && status.state() == ratspeak_runtime::activity::ActivityCaptureState::Capturing;
    let level = if legacy_enabled && status.profile() == Some(CaptureProfile::Trace) {
        "detailed"
    } else {
        "standard"
    };
    publish_legacy_compatibility(&state, &status, legacy_enabled, level);
    state.emit_to_all(
        "activity_legacy_cleared_v1",
        serde_json::json!({
            "version": 1,
            "capture_generation": status.ingress_generation().to_string(),
        }),
    );
    Ok(status)
}

#[tauri::command]
pub async fn activity_detail(
    state: State<'_, Arc<AppState>>,
    args: ActivityEventArgs,
) -> AppResult<ActivityIpcResponse<ActivityDetailResultV1>> {
    let sequence = parse_decimal_u64(&args.sequence, false)?;
    state
        .activity
        .detail_for_ipc(args.capture_session, sequence)
        .await
        .map_err(map_recorder_error)
}

#[tauri::command]
pub async fn activity_reveal(
    state: State<'_, Arc<AppState>>,
    args: ActivityRevealArgs,
) -> AppResult<ActivityIpcResponse<ActivityRevealResultV1>> {
    let sequence = parse_decimal_u64(&args.sequence, false)?;
    let field = parse_reveal_field(&args.field)?;
    state
        .activity
        .reveal_for_ipc(args.capture_session, sequence, field)
        .await
        .map_err(map_recorder_error)
}

#[tauri::command]
pub async fn activity_safe_copy(
    state: State<'_, Arc<AppState>>,
    args: ActivityEventArgs,
) -> AppResult<ActivityIpcResponse<ActivitySafeCopyResultV1>> {
    let sequence = parse_decimal_u64(&args.sequence, false)?;
    state
        .activity
        .safe_copy_for_ipc(args.capture_session, sequence)
        .await
        .map_err(map_recorder_error)
}

fn ensure_activity_request_fence(
    state: &AppState,
    expected: ActivityRequestFence,
) -> AppResult<()> {
    if state.is_current_activity_request_fence_after_identity_lock(expected) {
        Ok(())
    } else {
        Err(AppError::conflict(
            "The active session changed before the Activity request could run.",
        ))
    }
}

fn publish_legacy_compatibility(
    state: &AppState,
    status: &ActivityStatusV1,
    enabled: bool,
    level: &str,
) {
    state
        .network_log_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
    if let Ok(mut stored) = state.network_log_level.write() {
        *stored = level.to_string();
    }
    state.emit_to_all(
        "network_log_level_changed",
        serde_json::json!({
            "level": level,
            "enabled": enabled,
            "restart_required": false,
            "identity_generation": state.current_identity_session_generation().to_string(),
            "activity": status,
        }),
    );
}

fn parse_decimal_u64(value: &str, allow_zero: bool) -> AppResult<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_decimal_error());
    }
    let parsed = value.parse::<u64>().map_err(|_| invalid_decimal_error())?;
    if !allow_zero && parsed == 0 {
        return Err(invalid_sequence_error());
    }
    Ok(parsed)
}

fn invalid_decimal_error() -> AppError {
    AppError::bad_request("Activity cursors must be canonical decimal-string u64 values.")
}

fn invalid_sequence_error() -> AppError {
    AppError::bad_request("Activity event sequences must be greater than zero.")
}

fn validate_replay_limits(max_events: usize, max_bytes: usize) -> AppResult<()> {
    if !(1..=ACTIVITY_REPLAY_MAX_EVENTS).contains(&max_events)
        || !(ACTIVITY_REPLAY_MIN_BYTES..=ACTIVITY_REPLAY_MAX_BYTES).contains(&max_bytes)
    {
        return Err(AppError::bad_request(
            "Activity replay limits are outside the supported bounds.",
        ));
    }
    Ok(())
}

fn parse_profile(
    args: ActivitySetProfileArgs,
) -> AppResult<(CaptureProfile, Option<TraceCaptureDuration>)> {
    let profile = match args.profile.as_str() {
        "normal" => CaptureProfile::Normal,
        "trace" => CaptureProfile::Trace,
        _ => {
            return Err(AppError::bad_request(
                "Activity capture profile must be normal or trace.",
            ));
        }
    };

    let duration = match args.duration {
        None => None,
        Some(duration) => parse_trace_duration(duration)?,
    };
    if profile == CaptureProfile::Normal && duration.is_some() {
        return Err(AppError::bad_request(
            "A Trace duration cannot be used with the normal capture profile.",
        ));
    }
    Ok((profile, duration))
}

fn parse_trace_duration(
    duration: ActivityTraceDurationArgs,
) -> AppResult<Option<TraceCaptureDuration>> {
    match duration {
        ActivityTraceDurationArgs::Mode(mode) if mode == "platform_default" => Ok(None),
        ActivityTraceDurationArgs::Mode(mode) if mode == "until_stopped" => {
            Ok(Some(TraceCaptureDuration::UntilStopped))
        }
        ActivityTraceDurationArgs::Limited(envelope) => {
            let seconds = parse_decimal_u64(&envelope.limited.seconds, false).map_err(|_| {
                AppError::bad_request(
                    "Trace seconds must be a canonical decimal-string u64 greater than zero.",
                )
            })?;
            if seconds > MAX_LIMITED_TRACE_SECONDS {
                return Err(AppError::bad_request(
                    "A limited Trace capture cannot exceed 24 hours.",
                ));
            }
            Ok(Some(TraceCaptureDuration::Limited(Duration::from_secs(
                seconds,
            ))))
        }
        ActivityTraceDurationArgs::Mode(_) => Err(AppError::bad_request(
            "Trace duration must be platform_default, until_stopped, or limited.",
        )),
    }
}

fn parse_reveal_field(field: &str) -> AppResult<ActivityAttributeKey> {
    match field {
        "destination" => Ok(ActivityAttributeKey::Destination),
        "endpoint" => Ok(ActivityAttributeKey::Endpoint),
        "hub" => Ok(ActivityAttributeKey::Hub),
        "identity" => Ok(ActivityAttributeKey::Identity),
        "link" => Ok(ActivityAttributeKey::Link),
        "message" => Ok(ActivityAttributeKey::Message),
        "room" => Ok(ActivityAttributeKey::Room),
        _ => Err(AppError::bad_request(
            "The requested Activity field is not revealable.",
        )),
    }
}

fn map_recorder_error(error: ActivityRecorderError) -> AppError {
    match error {
        ActivityRecorderError::WorkerUnavailable => AppError::new(
            "activity_worker_unavailable",
            "Activity capture is temporarily unavailable.",
        ),
        ActivityRecorderError::ControlBusy => {
            AppError::new("activity_busy", "Activity is busy. Try again shortly.")
        }
        ActivityRecorderError::InvalidTransition => AppError::new(
            "activity_invalid_transition",
            "Activity cannot perform that action in its current state.",
        ),
        ActivityRecorderError::Superseded => AppError::new(
            "activity_superseded",
            "A newer Activity lifecycle action replaced this request.",
        ),
        ActivityRecorderError::GenerationExhausted => AppError::new(
            "activity_restart_required",
            "Activity requires an app restart before capture can continue.",
        ),
        ActivityRecorderError::RingUnavailable => AppError::new(
            "activity_storage_unavailable",
            "Activity could not initialize its in-memory buffer.",
        ),
        ActivityRecorderError::InvalidRequest => {
            AppError::bad_request("The Activity request is invalid.")
        }
        ActivityRecorderError::TimedOut => AppError::new(
            "activity_timed_out",
            "Activity did not finish the request in time.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_profile_args(
        profile: &str,
        mode: Option<&str>,
        seconds: Option<&str>,
    ) -> ActivitySetProfileArgs {
        ActivitySetProfileArgs {
            profile: profile.to_string(),
            duration: mode.map(|mode| {
                if mode == "limited" {
                    ActivityTraceDurationArgs::Limited(ActivityLimitedTraceEnvelope {
                        limited: ActivityLimitedTraceArgs {
                            seconds: seconds.unwrap_or_default().to_string(),
                        },
                    })
                } else {
                    assert!(seconds.is_none());
                    ActivityTraceDurationArgs::Mode(mode.to_string())
                }
            }),
        }
    }

    #[test]
    fn decimal_u64_parser_accepts_only_canonical_wire_values() {
        assert_eq!(parse_decimal_u64("0", true).unwrap(), 0);
        assert_eq!(parse_decimal_u64("1", false).unwrap(), 1);
        assert_eq!(
            parse_decimal_u64("18446744073709551615", false).unwrap(),
            u64::MAX
        );

        for invalid in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1 ",
            "1.0",
            "1_0",
            "18446744073709551616",
        ] {
            assert!(
                parse_decimal_u64(invalid, true).is_err(),
                "accepted {invalid:?}"
            );
        }
        assert!(parse_decimal_u64("0", false).is_err());
    }

    #[test]
    fn profile_parser_preserves_all_three_trace_duration_choices() {
        assert_eq!(
            parse_profile(set_profile_args("trace", None, None)).unwrap(),
            (CaptureProfile::Trace, None)
        );
        assert_eq!(
            parse_profile(set_profile_args("trace", Some("platform_default"), None)).unwrap(),
            (CaptureProfile::Trace, None)
        );
        assert_eq!(
            parse_profile(set_profile_args("trace", Some("until_stopped"), None)).unwrap(),
            (
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::UntilStopped)
            )
        );
        assert_eq!(
            parse_profile(set_profile_args("trace", Some("limited"), Some("1"))).unwrap(),
            (
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(1)))
            )
        );
        assert_eq!(
            parse_profile(set_profile_args("trace", Some("limited"), Some("600"))).unwrap(),
            (
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(600)))
            )
        );
        assert_eq!(
            parse_profile(set_profile_args("trace", Some("limited"), Some("86400"))).unwrap(),
            (
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(86400)))
            )
        );
    }

    #[test]
    fn profile_parser_rejects_unsafe_or_inapplicable_durations() {
        assert!(parse_profile(set_profile_args("normal", Some("until_stopped"), None)).is_err());
        assert!(parse_profile(set_profile_args("trace", Some("limited"), None)).is_err());
        assert!(parse_profile(set_profile_args("trace", Some("limited"), Some("0"))).is_err());
        assert!(parse_profile(set_profile_args("trace", Some("limited"), Some("86401"))).is_err());
        assert!(parse_profile(set_profile_args("trace", Some("unknown"), None)).is_err());
        assert!(parse_profile(set_profile_args("TRACE", None, None)).is_err());
    }

    #[test]
    fn wire_dtos_preserve_null_cursor_and_explicit_duration_shapes() {
        let replay: ActivityReplayArgs = serde_json::from_value(serde_json::json!({
            "capture_session": "00000000000000000000000000000000",
            "after": null,
            "max_events": 50,
            "max_bytes": 65536
        }))
        .unwrap();
        assert_eq!(replay.after, None);

        let platform_default: ActivitySetProfileArgs = serde_json::from_value(serde_json::json!({
            "profile": "trace",
            "duration": "platform_default"
        }))
        .unwrap();
        assert_eq!(
            parse_profile(platform_default).unwrap(),
            (CaptureProfile::Trace, None)
        );

        let limited: ActivitySetProfileArgs = serde_json::from_value(serde_json::json!({
            "profile": "trace",
            "duration": { "limited": { "seconds": "600" } }
        }))
        .unwrap();
        assert_eq!(
            parse_profile(limited).unwrap(),
            (
                CaptureProfile::Trace,
                Some(TraceCaptureDuration::Limited(Duration::from_secs(600)))
            )
        );

        assert!(
            serde_json::from_value::<ActivityReplayArgs>(serde_json::json!({
                "capture_session": "00000000000000000000000000000000",
                "after": 0,
                "max_events": 50,
                "max_bytes": 65536
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivityReplayArgs>(serde_json::json!({
                "capture_session": "00000000000000000000000000000000",
                "after": null,
                "max_events": 50,
                "max_bytes": 65536,
                "unexpected": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<ActivitySetProfileArgs>(serde_json::json!({
                "profile": "trace",
                "duration": { "limited": { "seconds": "600", "unexpected": true } }
            }))
            .is_err()
        );
    }

    #[test]
    fn reveal_field_parser_is_an_explicit_allowlist() {
        for field in [
            "destination",
            "endpoint",
            "hub",
            "identity",
            "link",
            "message",
            "room",
        ] {
            assert!(parse_reveal_field(field).is_ok(), "rejected {field}");
        }
        for field in ["session", "reason", "state", "count", "Destination", ""] {
            assert!(parse_reveal_field(field).is_err(), "accepted {field:?}");
        }
    }

    #[test]
    fn replay_limits_are_bounded_at_the_ipc_boundary() {
        assert!(validate_replay_limits(1, ACTIVITY_REPLAY_MIN_BYTES).is_ok());
        assert!(
            validate_replay_limits(ACTIVITY_REPLAY_MAX_EVENTS, ACTIVITY_REPLAY_MAX_BYTES).is_ok()
        );
        assert!(validate_replay_limits(0, ACTIVITY_REPLAY_MIN_BYTES).is_err());
        assert!(
            validate_replay_limits(ACTIVITY_REPLAY_MAX_EVENTS + 1, ACTIVITY_REPLAY_MIN_BYTES)
                .is_err()
        );
        assert!(validate_replay_limits(1, ACTIVITY_REPLAY_MIN_BYTES - 1).is_err());
        assert!(validate_replay_limits(1, ACTIVITY_REPLAY_MAX_BYTES + 1).is_err());
    }

    #[test]
    fn recorder_errors_have_static_public_mappings() {
        let cases = [
            (
                ActivityRecorderError::WorkerUnavailable,
                "activity_worker_unavailable",
            ),
            (ActivityRecorderError::ControlBusy, "activity_busy"),
            (
                ActivityRecorderError::InvalidTransition,
                "activity_invalid_transition",
            ),
            (ActivityRecorderError::Superseded, "activity_superseded"),
            (
                ActivityRecorderError::GenerationExhausted,
                "activity_restart_required",
            ),
            (
                ActivityRecorderError::RingUnavailable,
                "activity_storage_unavailable",
            ),
            (ActivityRecorderError::InvalidRequest, "bad_request"),
            (ActivityRecorderError::TimedOut, "activity_timed_out"),
        ];
        for (source, expected_code) in cases {
            let public = map_recorder_error(source);
            assert_eq!(public.code, expected_code);
            assert!(!public.message.is_empty());
        }
    }
}
