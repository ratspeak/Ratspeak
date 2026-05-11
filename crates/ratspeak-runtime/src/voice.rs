//! LXST voice service and native audio bridge.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use lxst_core::{CallRole, Profile, RawAudioFrame, SignallingStatus};
use lxst_telephony::{
    ActiveCallSnapshot, TelephonyControl, TelephonyRnsEndpoint, TelephonyRuntimeCore,
    TelephonyRuntimeSnapshot, TelephonyService, TelephonyServiceEvent,
};
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::state::AppState;

const AUDIO_FRAME_CHANNEL_DEPTH: usize = 8;
const AUDIO_SPEAKER_CHANNEL_DEPTH: usize = 32;
const VOICE_AGC_TARGET_RMS: f32 = 0.17782794;
const VOICE_AGC_MIN_GAIN: f32 = 0.35;
const VOICE_AGC_MAX_GAIN: f32 = 6.0;
const VOICE_AGC_ATTACK: f32 = 0.20;
const VOICE_AGC_RELEASE: f32 = 0.04;
const VOICE_HIGHPASS_HZ: f32 = 250.0;
const VOICE_LOWPASS_HZ: f32 = 8_500.0;
const VOICE_PROFILE_UPGRADE_AFTER: Duration = Duration::from_secs(8);
const VOICE_PROFILE_SWITCH_COOLDOWN: Duration = Duration::from_secs(12);
const VOICE_PROFILE_DOWNGRADE_COOLDOWN: Duration = Duration::from_secs(20);
const VOICE_PROFILE_UPGRADE_LOCKOUT_AFTER_DOWNGRADE: Duration = Duration::from_secs(60);
const VOICE_PROFILE_DROPPED_FRAME_THRESHOLD: usize = 4;
const VOICE_INITIAL_PROFILE: Profile = Profile::QualityMedium;
const LXMF_DELIVERY_DESTINATION_NAME: &str = "lxmf.delivery";

pub type VoiceResult<T> = Result<T, String>;

pub struct LxstVoiceServiceHandle {
    control_tx: mpsc::Sender<TelephonyControl>,
    service_task: Option<JoinHandle<()>>,
    event_task: Option<JoinHandle<()>>,
}

impl LxstVoiceServiceHandle {
    fn new(
        control_tx: mpsc::Sender<TelephonyControl>,
        service_task: JoinHandle<()>,
        event_task: JoinHandle<()>,
    ) -> Self {
        Self {
            control_tx,
            service_task: Some(service_task),
            event_task: Some(event_task),
        }
    }

    fn control_tx(&self) -> mpsc::Sender<TelephonyControl> {
        self.control_tx.clone()
    }

    async fn shutdown(mut self) {
        let _ = self.control_tx.send(TelephonyControl::Shutdown).await;
        if let Some(task) = self.event_task.take() {
            await_or_abort(task).await;
        }
        if let Some(task) = self.service_task.take() {
            await_or_abort(task).await;
        }
    }
}

impl Drop for LxstVoiceServiceHandle {
    fn drop(&mut self) {
        if let Some(task) = self.event_task.take() {
            task.abort();
        }
        if let Some(task) = self.service_task.take() {
            task.abort();
        }
    }
}

async fn await_or_abort(mut task: JoinHandle<()>) {
    tokio::select! {
        _ = &mut task => {}
        _ = tokio::time::sleep(Duration::from_secs(2)) => task.abort(),
    }
}

pub async fn start_voice_service(state: &Arc<AppState>) -> VoiceResult<()> {
    if voice_control_tx(state).is_some() {
        return Ok(());
    }

    let (transport_tx, identity) = voice_runtime_inputs(state)?;
    let endpoint = TelephonyRnsEndpoint::register(transport_tx, &identity)
        .map_err(|e| format!("Failed to register LXST telephony destination: {e}"))?;

    let (control_tx, control_rx) = mpsc::channel::<TelephonyControl>(32);
    let (event_tx, event_rx) = mpsc::channel::<TelephonyServiceEvent>(128);
    let service =
        TelephonyService::new(endpoint, TelephonyRuntimeCore::new(), control_rx, event_tx);

    let service_task = tokio::spawn(async move {
        service.run().await;
    });

    let event_state = Arc::clone(state);
    let event_control_tx = control_tx.clone();
    let runtime = tokio::runtime::Handle::current();
    let event_task = tokio::task::spawn_blocking(move || {
        runtime.block_on(drive_voice_events(event_state, event_control_tx, event_rx));
    });

    let handle = LxstVoiceServiceHandle::new(control_tx, service_task, event_task);
    if let Ok(mut voice) = state.lxst_voice.lock() {
        if voice.is_some() {
            drop(handle);
            return Ok(());
        }
        *voice = Some(handle);
    } else {
        drop(handle);
        return Err("LXST voice state lock is poisoned".to_string());
    }

    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "service",
            "enabled": true,
            "running": true,
        }),
    );
    Ok(())
}

pub async fn shutdown_voice_service(state: &Arc<AppState>) {
    let handle = state
        .lxst_voice
        .lock()
        .ok()
        .and_then(|mut voice| voice.take());

    if let Some(handle) = handle {
        handle.shutdown().await;
    }

    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "service",
            "enabled": true,
            "running": false,
        }),
    );
}

pub fn voice_status(state: &AppState) -> Value {
    let running = state
        .lxst_voice
        .lock()
        .map(|voice| voice.is_some())
        .unwrap_or(false);
    json!({
        "enabled": true,
        "running": running,
    })
}

pub async fn call_identity(state: &Arc<AppState>, remote_identity: [u8; 16]) -> VoiceResult<Value> {
    ensure_voice_service_started(state).await?;
    send_control(
        state,
        TelephonyControl::Call {
            remote_identity,
            profile: Some(VOICE_INITIAL_PROFILE),
            discovery_timeout: Duration::from_secs(15),
        },
    )
    .await?;
    Ok(json!({
        "ok": true,
        "remote_identity": hex::encode(remote_identity),
        "remote_lxmf_destination": lxmf_destination_for_identity(remote_identity),
        "profile": profile_key(VOICE_INITIAL_PROFILE),
    }))
}

pub async fn answer(state: &Arc<AppState>) -> VoiceResult<Value> {
    ensure_voice_service_started(state).await?;
    send_control(state, TelephonyControl::Answer).await?;
    Ok(json!({ "ok": true }))
}

pub async fn hangup(state: &Arc<AppState>) -> VoiceResult<Value> {
    if voice_control_tx(state).is_none() {
        return Ok(json!({ "ok": true, "running": false }));
    }
    send_control(
        state,
        TelephonyControl::Hangup {
            ring_timeout: false,
        },
    )
    .await?;
    Ok(json!({ "ok": true }))
}

pub async fn reject(state: &Arc<AppState>) -> VoiceResult<Value> {
    hangup(state).await
}

async fn ensure_voice_service_started(state: &Arc<AppState>) -> VoiceResult<()> {
    if voice_control_tx(state).is_some() {
        Ok(())
    } else {
        start_voice_service(state).await
    }
}

async fn send_control(state: &AppState, control: TelephonyControl) -> VoiceResult<()> {
    let tx =
        voice_control_tx(state).ok_or_else(|| "LXST voice service is not running".to_string())?;
    tx.send(control)
        .await
        .map_err(|_| "LXST voice service is not accepting commands".to_string())
}

fn voice_control_tx(state: &AppState) -> Option<mpsc::Sender<TelephonyControl>> {
    state
        .lxst_voice
        .lock()
        .ok()
        .and_then(|voice| voice.as_ref().map(LxstVoiceServiceHandle::control_tx))
}

fn voice_runtime_inputs(
    state: &AppState,
) -> VoiceResult<(
    mpsc::Sender<rns_transport::messages::TransportMessage>,
    Identity,
)> {
    let transport_tx = state
        .rns
        .read()
        .map_err(|_| "RNS state lock is poisoned".to_string())?
        .as_ref()
        .map(|mgr| mgr.handle.transport_tx.clone())
        .ok_or_else(|| "RNS is not initialized".to_string())?;

    let private_key = state
        .lxmf
        .lock()
        .map_err(|_| "LXMF state lock is poisoned".to_string())?
        .as_ref()
        .and_then(|mgr| mgr.identity.get_private_key())
        .ok_or_else(|| "Active identity private key is unavailable".to_string())?;

    let identity = Identity::from_private_key(&*private_key)
        .map_err(|e| format!("Failed to clone active identity for LXST voice: {e}"))?;

    Ok((transport_tx, identity))
}

async fn drive_voice_events(
    state: Arc<AppState>,
    control_tx: mpsc::Sender<TelephonyControl>,
    mut event_rx: mpsc::Receiver<TelephonyServiceEvent>,
) {
    let mut audio_session: Option<VoiceAudioSession> = None;
    let mut audio_failure: Option<VoiceAudioFailure> = None;
    let mut profile_adaptation = VoiceProfileAdaptation::new();
    let mut latest_snapshot: Option<TelephonyRuntimeSnapshot> = None;

    while let Some(event) = event_rx.recv().await {
        match event {
            TelephonyServiceEvent::IncomingCall {
                link_id,
                remote_identity,
            } => {
                let payload = json!({
                    "type": "incoming",
                    "link_id": hex::encode(link_id),
                    "remote_identity": hex::encode(remote_identity),
                    "remote_lxmf_destination": lxmf_destination_for_identity(remote_identity),
                });
                state.emit_to_all("voice_incoming_call", payload.clone());
                state.emit_to_all("voice_call_update", payload);
            }
            TelephonyServiceEvent::OutgoingCallStarted {
                link_id,
                remote_identity,
            } => {
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "outgoing",
                        "link_id": hex::encode(link_id),
                        "remote_identity": hex::encode(remote_identity),
                        "remote_lxmf_destination": lxmf_destination_for_identity(remote_identity),
                    }),
                );
            }
            TelephonyServiceEvent::CallTerminated { link_id, reason } => {
                stop_audio_session(audio_session.take(), &control_tx).await;
                audio_failure = None;
                profile_adaptation.reset();
                latest_snapshot = None;
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "terminated",
                        "link_id": hex::encode(link_id),
                        "reason": reason.map(status_key),
                    }),
                );
            }
            TelephonyServiceEvent::Snapshot(snapshot) => {
                latest_snapshot = Some(snapshot.clone());
                reconcile_audio_session(
                    &state,
                    &control_tx,
                    &snapshot,
                    &mut audio_session,
                    &mut audio_failure,
                )
                .await;
                maybe_adapt_voice_profile(&state, &control_tx, &snapshot, &mut profile_adaptation)
                    .await;
                emit_snapshot(&state, &snapshot, audio_session.as_ref());
            }
            TelephonyServiceEvent::OpusTransmitStreamStarted { link_id, profile } => {
                emit_media_state(&state, "mic_started", link_id, profile, None);
            }
            TelephonyServiceEvent::OpusTransmitStreamStopped {
                link_id,
                profile,
                reason,
            } => {
                emit_media_state(
                    &state,
                    "mic_stopped",
                    link_id,
                    profile,
                    Some(format!("{reason:?}")),
                );
            }
            TelephonyServiceEvent::OpusReceiveStreamStarted { link_id, profile } => {
                emit_media_state(&state, "speaker_started", link_id, profile, None);
            }
            TelephonyServiceEvent::OpusReceiveStreamStopped {
                link_id,
                profile,
                reason,
            } => {
                emit_media_state(
                    &state,
                    "speaker_stopped",
                    link_id,
                    profile,
                    Some(format!("{reason:?}")),
                );
            }
            TelephonyServiceEvent::OpusReceiveStreamFrames {
                link_id,
                profile,
                frames,
                dropped,
            } => {
                profile_adaptation.record_receive(link_id, dropped);
                if let Some(snapshot) = latest_snapshot.as_ref() {
                    maybe_adapt_voice_profile(
                        &state,
                        &control_tx,
                        snapshot,
                        &mut profile_adaptation,
                    )
                    .await;
                }
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "speaker_frames",
                        "link_id": hex::encode(link_id),
                        "profile": profile_key(profile),
                        "frames": frames,
                        "dropped": dropped,
                    }),
                );
            }
            TelephonyServiceEvent::Error { message } => {
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "error",
                        "message": message,
                    }),
                );
            }
            TelephonyServiceEvent::MediaSent { .. } => {
                if let Some(snapshot) = latest_snapshot.as_ref() {
                    maybe_adapt_voice_profile(
                        &state,
                        &control_tx,
                        snapshot,
                        &mut profile_adaptation,
                    )
                    .await;
                }
            }
            TelephonyServiceEvent::Stopped => {
                stop_audio_session(audio_session.take(), &control_tx).await;
                profile_adaptation.reset();
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "service",
                        "enabled": true,
                        "running": false,
                    }),
                );
                break;
            }
            TelephonyServiceEvent::MediaReceived { .. }
            | TelephonyServiceEvent::OpusFramesReceived { .. }
            | TelephonyServiceEvent::Drive(_) => {}
        }
    }

    stop_audio_session(audio_session.take(), &control_tx).await;
}

async fn reconcile_audio_session(
    state: &AppState,
    control_tx: &mpsc::Sender<TelephonyControl>,
    snapshot: &TelephonyRuntimeSnapshot,
    audio_session: &mut Option<VoiceAudioSession>,
    audio_failure: &mut Option<VoiceAudioFailure>,
) {
    let Some(active) = snapshot.active_call.as_ref() else {
        stop_audio_session(audio_session.take(), control_tx).await;
        *audio_failure = None;
        return;
    };

    if active.status != SignallingStatus::Established {
        stop_audio_session(audio_session.take(), control_tx).await;
        *audio_failure = None;
        return;
    }

    let profile = active.profile.unwrap_or(Profile::DEFAULT);
    if profile.opus_payload_ceiling_bytes().is_none() {
        state.emit_to_all(
            "voice_call_update",
            json!({
                "type": "error",
                "message": "Only Opus LXST voice profiles are supported by the live audio bridge",
            }),
        );
        return;
    }

    let current_matches = audio_session
        .as_ref()
        .is_some_and(|session| session.link_id == active.link_id && session.profile == profile);
    if current_matches {
        return;
    }

    if audio_failure
        .as_ref()
        .is_some_and(|failure| failure.matches(active.link_id, profile))
    {
        return;
    }

    stop_audio_session(audio_session.take(), control_tx).await;
    match VoiceAudioSession::start(active.link_id, profile, control_tx.clone()).await {
        Ok(session) => {
            let microphone = session.microphone;
            let speaker = session.speaker;
            let warnings = session.warnings.clone();
            tracing::info!(
                link_id = %hex::encode(active.link_id),
                profile = profile_key(profile),
                microphone,
                speaker,
                "started LXST native audio"
            );
            *audio_session = Some(session);
            *audio_failure = None;
            state.emit_to_all(
                "voice_call_update",
                json!({
                    "type": "audio",
                    "state": "started",
                    "link_id": hex::encode(active.link_id),
                    "profile": profile_key(profile),
                    "running": microphone || speaker,
                    "microphone": microphone,
                    "speaker": speaker,
                    "warnings": warnings,
                }),
            );
        }
        Err(message) => {
            *audio_failure = Some(VoiceAudioFailure {
                link_id: active.link_id,
                profile,
            });
            state.emit_to_all(
                "voice_call_update",
                json!({
                    "type": "error",
                    "message": message,
                }),
            );
        }
    }
}

async fn maybe_adapt_voice_profile(
    state: &AppState,
    control_tx: &mpsc::Sender<TelephonyControl>,
    snapshot: &TelephonyRuntimeSnapshot,
    adaptation: &mut VoiceProfileAdaptation,
) {
    let Some(active) = snapshot.active_call.as_ref() else {
        adaptation.reset();
        return;
    };

    if active.status != SignallingStatus::Established {
        adaptation.reset_for_link(active.link_id);
        return;
    }

    let current = active.profile.unwrap_or(Profile::DEFAULT);
    let Some(next) = adaptation.next_profile(active.link_id, current) else {
        return;
    };

    if control_tx
        .send(TelephonyControl::SwitchProfile { profile: next })
        .await
        .is_ok()
    {
        tracing::info!(
            link_id = %hex::encode(active.link_id),
            from = profile_key(current),
            to = profile_key(next),
            "switching LXST voice profile"
        );
        adaptation.mark_switch(next);
        state.emit_to_all(
            "voice_call_update",
            json!({
                "type": "profile_adaptation",
                "link_id": hex::encode(active.link_id),
                "from": profile_key(current),
                "to": profile_key(next),
            }),
        );
    }
}

async fn stop_audio_session(
    session: Option<VoiceAudioSession>,
    control_tx: &mpsc::Sender<TelephonyControl>,
) {
    if let Some(session) = session {
        if session.microphone {
            let _ = control_tx.send(TelephonyControl::StopOpusStream).await;
        }
        if session.speaker {
            let _ = control_tx
                .send(TelephonyControl::StopOpusReceiveStream)
                .await;
        }
        drop(session);
    }
}

fn emit_snapshot(
    state: &AppState,
    snapshot: &TelephonyRuntimeSnapshot,
    audio: Option<&VoiceAudioSession>,
) {
    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "snapshot",
            "external_busy": snapshot.external_busy,
            "pending_link_count": snapshot.pending_link_count,
            "audio": audio.map(|session| json!({
                "link_id": hex::encode(session.link_id),
                "profile": profile_key(session.profile),
                "running": session.running(),
                "microphone": session.microphone,
                "speaker": session.speaker,
            })),
            "active_call": snapshot.active_call.as_ref().map(active_call_payload),
        }),
    );
}

fn active_call_payload(active: &ActiveCallSnapshot) -> Value {
    json!({
        "link_id": hex::encode(active.link_id),
        "remote_identity": hex::encode(active.remote_identity),
        "remote_lxmf_destination": lxmf_destination_for_identity(active.remote_identity),
        "role": role_key(active.role),
        "status": status_key(active.status),
        "profile": active.profile.map(profile_key),
        "answered": active.answered,
    })
}

fn lxmf_destination_for_identity(identity_hash: [u8; 16]) -> String {
    hex::encode(Destination::hash_from_name_and_identity(
        LXMF_DELIVERY_DESTINATION_NAME,
        Some(&identity_hash),
    ))
}

fn emit_media_state(
    state: &AppState,
    event_type: &str,
    link_id: [u8; 16],
    profile: Profile,
    reason: Option<String>,
) {
    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": event_type,
            "link_id": hex::encode(link_id),
            "profile": profile_key(profile),
            "reason": reason,
        }),
    );
}

struct VoiceProfileAdaptation {
    link_id: Option<[u8; 16]>,
    stable_since: Option<Instant>,
    last_switch_at: Option<Instant>,
    current_profile: Option<Profile>,
    requested_profile: Option<Profile>,
    upgrade_blocked_until: Option<Instant>,
    dropped_since_switch: usize,
}

impl VoiceProfileAdaptation {
    fn new() -> Self {
        Self {
            link_id: None,
            stable_since: None,
            last_switch_at: None,
            current_profile: None,
            requested_profile: None,
            upgrade_blocked_until: None,
            dropped_since_switch: 0,
        }
    }

    fn reset(&mut self) {
        self.link_id = None;
        self.stable_since = None;
        self.last_switch_at = None;
        self.current_profile = None;
        self.requested_profile = None;
        self.upgrade_blocked_until = None;
        self.dropped_since_switch = 0;
    }

    fn reset_for_link(&mut self, link_id: [u8; 16]) {
        if self.link_id == Some(link_id) {
            self.stable_since = None;
            self.current_profile = None;
            self.requested_profile = None;
            self.upgrade_blocked_until = None;
            self.dropped_since_switch = 0;
        } else {
            self.reset();
        }
    }

    fn record_receive(&mut self, link_id: [u8; 16], dropped: usize) {
        if self.link_id == Some(link_id) {
            self.dropped_since_switch = self.dropped_since_switch.saturating_add(dropped);
        }
    }

    fn next_profile(&mut self, link_id: [u8; 16], current: Profile) -> Option<Profile> {
        if !is_adaptive_opus_quality_profile(current) {
            self.reset_for_link(link_id);
            return None;
        }

        let now = Instant::now();
        if self.link_id == Some(link_id)
            && let Some(requested) = self.requested_profile
        {
            if current == requested {
                self.requested_profile = None;
            } else {
                return None;
            }
        }

        self.sync(link_id, current, now);

        if self.dropped_since_switch >= VOICE_PROFILE_DROPPED_FRAME_THRESHOLD
            && self.can_switch(now, VOICE_PROFILE_DOWNGRADE_COOLDOWN)
            && let Some(profile) = lower_quality_profile(current)
        {
            self.upgrade_blocked_until = Some(
                now.checked_add(VOICE_PROFILE_UPGRADE_LOCKOUT_AFTER_DOWNGRADE)
                    .unwrap_or(now),
            );
            return Some(profile);
        }

        if self.dropped_since_switch > 0
            || !self.can_switch(now, VOICE_PROFILE_SWITCH_COOLDOWN)
            || self
                .upgrade_blocked_until
                .is_some_and(|blocked_until| now < blocked_until)
        {
            return None;
        }

        let stable_for = self
            .stable_since
            .map(|stable_since| now.saturating_duration_since(stable_since))
            .unwrap_or_default();

        match current {
            Profile::QualityMedium if stable_for >= VOICE_PROFILE_UPGRADE_AFTER => {
                Some(Profile::QualityHigh)
            }
            _ => None,
        }
    }

    fn mark_switch(&mut self, profile: Profile) {
        let now = Instant::now();
        self.current_profile = Some(profile);
        self.requested_profile = Some(profile);
        self.stable_since = Some(now);
        self.last_switch_at = Some(now);
        self.dropped_since_switch = 0;
    }

    fn sync(&mut self, link_id: [u8; 16], profile: Profile, now: Instant) {
        if self.link_id != Some(link_id) {
            self.link_id = Some(link_id);
            self.stable_since = Some(now);
            self.current_profile = Some(profile);
            self.requested_profile = None;
            self.upgrade_blocked_until = None;
            self.dropped_since_switch = 0;
            return;
        }

        if self.current_profile != Some(profile) {
            self.current_profile = Some(profile);
            self.requested_profile = None;
            self.stable_since = Some(now);
            self.dropped_since_switch = 0;
        } else if self.stable_since.is_none() {
            self.stable_since = Some(now);
        }
    }

    fn can_switch(&self, now: Instant, cooldown: Duration) -> bool {
        self.last_switch_at
            .map(|last_switch_at| now.saturating_duration_since(last_switch_at) >= cooldown)
            .unwrap_or(true)
    }
}

fn is_adaptive_opus_quality_profile(profile: Profile) -> bool {
    matches!(
        profile,
        Profile::QualityMedium | Profile::QualityHigh | Profile::QualityMax
    )
}

fn lower_quality_profile(profile: Profile) -> Option<Profile> {
    match profile {
        Profile::QualityMax => Some(Profile::QualityHigh),
        Profile::QualityHigh => Some(Profile::QualityMedium),
        _ => None,
    }
}

fn role_key(role: CallRole) -> &'static str {
    match role {
        CallRole::Incoming => "incoming",
        CallRole::Outgoing => "outgoing",
    }
}

fn status_key(status: SignallingStatus) -> &'static str {
    match status {
        SignallingStatus::Busy => "busy",
        SignallingStatus::Rejected => "rejected",
        SignallingStatus::Calling => "calling",
        SignallingStatus::Available => "available",
        SignallingStatus::Ringing => "ringing",
        SignallingStatus::Connecting => "connecting",
        SignallingStatus::Established => "established",
    }
}

fn profile_key(profile: Profile) -> &'static str {
    match profile {
        Profile::BandwidthUltraLow => "bandwidth_ultra_low",
        Profile::BandwidthVeryLow => "bandwidth_very_low",
        Profile::BandwidthLow => "bandwidth_low",
        Profile::QualityMedium => "quality_medium",
        Profile::QualityHigh => "quality_high",
        Profile::QualityMax => "quality_max",
        Profile::LatencyUltraLow => "latency_ultra_low",
        Profile::LatencyLow => "latency_low",
    }
}

struct VoiceAudioSession {
    link_id: [u8; 16],
    profile: Profile,
    microphone: bool,
    speaker: bool,
    warnings: Vec<String>,
    _input_stream: Option<cpal::Stream>,
    _output_stream: Option<cpal::Stream>,
    sink_task: Option<JoinHandle<()>>,
}

struct VoiceAudioFailure {
    link_id: [u8; 16],
    profile: Profile,
}

impl VoiceAudioFailure {
    fn matches(&self, link_id: [u8; 16], profile: Profile) -> bool {
        self.link_id == link_id && self.profile == profile
    }
}

impl VoiceAudioSession {
    fn running(&self) -> bool {
        self.microphone || self.speaker
    }

    async fn start(
        link_id: [u8; 16],
        profile: Profile,
        control_tx: mpsc::Sender<TelephonyControl>,
    ) -> VoiceResult<Self> {
        let host = cpal::default_host();
        let target_channels = usize::from(profile.channels());
        let target_sample_rate = profile.sample_rate_hz();
        let target_frames = profile.sample_frames_per_packet();

        let mut warnings = Vec::new();
        let input_stream = match start_microphone_side(
            &host,
            profile,
            control_tx.clone(),
            target_channels,
            target_sample_rate,
            target_frames,
        )
        .await
        {
            Ok(stream) => Some(stream),
            Err(message) => {
                warnings.push(message);
                None
            }
        };

        let (output_stream, sink_task) = match start_speaker_side(
            &host,
            control_tx,
            target_channels,
            target_sample_rate,
        )
        .await
        {
            Ok((stream, sink_task)) => (Some(stream), Some(sink_task)),
            Err(message) => {
                warnings.push(message);
                (None, None)
            }
        };

        if input_stream.is_none() && output_stream.is_none() {
            let detail = if warnings.is_empty() {
                "no native audio streams could be started".to_string()
            } else {
                warnings.join("; ")
            };
            return Err(format!("Failed to start LXST audio: {detail}"));
        }

        Ok(Self {
            link_id,
            profile,
            microphone: input_stream.is_some(),
            speaker: output_stream.is_some(),
            warnings,
            _input_stream: input_stream,
            _output_stream: output_stream,
            sink_task,
        })
    }
}

async fn start_microphone_side(
    host: &cpal::Host,
    profile: Profile,
    control_tx: mpsc::Sender<TelephonyControl>,
    target_channels: usize,
    target_sample_rate: u32,
    target_frames: usize,
) -> VoiceResult<cpal::Stream> {
    let input_device = host
        .default_input_device()
        .ok_or_else(|| "No default microphone is available".to_string())?;
    let input_config = select_input_config(&input_device, target_sample_rate)?;
    let (capture_tx, capture_rx) = mpsc::channel::<RawAudioFrame>(AUDIO_FRAME_CHANNEL_DEPTH);
    let input_builder = Arc::new(Mutex::new(InputFrameBuilder::new(
        usize::from(input_config.channels()),
        input_config.sample_rate().0,
        target_channels,
        target_sample_rate,
        target_frames,
    )));
    let input_stream = build_input_stream(&input_device, &input_config, input_builder, capture_tx)?;

    input_stream
        .play()
        .map_err(|e| format!("Failed to start microphone stream: {e}"))?;

    if let Err(e) = control_tx
        .send(TelephonyControl::StartOpusStream {
            profile,
            frames: capture_rx,
        })
        .await
    {
        return Err(format!("Failed to start LXST microphone stream: {e}"));
    }

    Ok(input_stream)
}

async fn start_speaker_side(
    host: &cpal::Host,
    control_tx: mpsc::Sender<TelephonyControl>,
    target_channels: usize,
    target_sample_rate: u32,
) -> VoiceResult<(cpal::Stream, JoinHandle<()>)> {
    let output_device = host
        .default_output_device()
        .ok_or_else(|| "No default speaker is available".to_string())?;
    let output_config = select_output_config(&output_device, target_sample_rate)?;
    let (speaker_tx, mut speaker_rx) = mpsc::channel::<RawAudioFrame>(AUDIO_SPEAKER_CHANNEL_DEPTH);

    let output_channels = usize::from(output_config.channels());
    let output_sample_rate = output_config.sample_rate().0;
    let output_queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(
        output_channels * output_sample_rate as usize / 2,
    )));
    let output_stream =
        build_output_stream(&output_device, &output_config, Arc::clone(&output_queue))?;

    let sink_task = tokio::spawn(async move {
        let max_queue_samples = output_channels * output_sample_rate as usize;
        while let Some(frame) = speaker_rx.recv().await {
            let converted = resample_output_frame(
                &frame,
                target_sample_rate,
                target_channels,
                output_sample_rate,
                output_channels,
            );
            if let Ok(mut queue) = output_queue.lock() {
                let projected = queue.len().saturating_add(converted.len());
                if projected > max_queue_samples {
                    let drop_count = projected - max_queue_samples;
                    let queue_len = queue.len();
                    queue.drain(..drop_count.min(queue_len));
                }
                queue.extend(converted);
            }
        }
    });

    if let Err(e) = output_stream.play() {
        sink_task.abort();
        return Err(format!("Failed to start speaker stream: {e}"));
    }

    if let Err(e) = control_tx
        .send(TelephonyControl::StartOpusReceiveStream { frames: speaker_tx })
        .await
    {
        sink_task.abort();
        return Err(format!("Failed to start LXST speaker stream: {e}"));
    }

    Ok((output_stream, sink_task))
}

impl Drop for VoiceAudioSession {
    fn drop(&mut self) {
        if let Some(task) = self.sink_task.take() {
            task.abort();
        }
    }
}

struct InputFrameBuilder {
    source_channels: usize,
    source_sample_rate: u32,
    target_channels: usize,
    target_sample_rate: u32,
    target_samples_per_frame: usize,
    source_samples: Vec<f32>,
    source_cursor: f64,
    pending_frame: Vec<f32>,
    processor: VoiceInputProcessor,
}

impl InputFrameBuilder {
    fn new(
        source_channels: usize,
        source_sample_rate: u32,
        target_channels: usize,
        target_sample_rate: u32,
        target_frames: usize,
    ) -> Self {
        Self {
            source_channels: source_channels.max(1),
            source_sample_rate: source_sample_rate.max(1),
            target_channels: target_channels.max(1),
            target_sample_rate: target_sample_rate.max(1),
            target_samples_per_frame: target_frames * target_channels.max(1),
            source_samples: Vec::with_capacity(target_frames * target_channels.max(1) * 2),
            source_cursor: 0.0,
            pending_frame: Vec::with_capacity(target_frames * target_channels.max(1)),
            processor: VoiceInputProcessor::new(target_channels.max(1), target_sample_rate.max(1)),
        }
    }

    fn push_interleaved(&mut self, samples: &[f32]) -> Vec<RawAudioFrame> {
        for source_frame in samples.chunks_exact(self.source_channels) {
            for target_channel in 0..self.target_channels {
                self.source_samples.push(channel_sample(
                    source_frame,
                    target_channel,
                    self.target_channels,
                ));
            }
        }

        let mut frames = Vec::new();
        let step = self.source_sample_rate as f64 / self.target_sample_rate as f64;

        loop {
            let available_frames = self.source_samples.len() / self.target_channels;
            let source_index = self.source_cursor.floor() as usize;
            if source_index + 1 >= available_frames {
                break;
            }

            let source_base = source_index * self.target_channels;
            let next_base = source_base + self.target_channels;
            let fraction = (self.source_cursor - source_index as f64) as f32;
            for channel in 0..self.target_channels {
                let a = self.source_samples[source_base + channel];
                let b = self.source_samples[next_base + channel];
                self.pending_frame.push(a + (b - a) * fraction);
            }
            self.source_cursor += step;

            if self.pending_frame.len() >= self.target_samples_per_frame {
                let mut samples = self
                    .pending_frame
                    .drain(..self.target_samples_per_frame)
                    .collect::<Vec<_>>();
                self.processor.process(&mut samples);
                if let Ok(frame) = RawAudioFrame::new(self.target_channels as u8, samples) {
                    frames.push(frame);
                }
            }
        }

        let consumed_frames = self.source_cursor.floor() as usize;
        if consumed_frames > 0 {
            let consumed_samples = consumed_frames * self.target_channels;
            self.source_samples
                .drain(..consumed_samples.min(self.source_samples.len()));
            self.source_cursor -= consumed_frames as f64;
        }

        frames
    }
}

struct VoiceInputProcessor {
    channels: usize,
    highpass: Vec<HighPassFilter>,
    lowpass: Vec<LowPassFilter>,
    agc_gain: f32,
}

impl VoiceInputProcessor {
    fn new(channels: usize, sample_rate: u32) -> Self {
        let channels = channels.max(1);
        let sample_rate = sample_rate.max(1) as f32;
        let max_filter_cutoff = (sample_rate * 0.45).max(40.0);
        let highpass_cutoff = VOICE_HIGHPASS_HZ
            .min((max_filter_cutoff * 0.8).max(20.0))
            .max(20.0);
        let lowpass_cutoff = VOICE_LOWPASS_HZ
            .min(max_filter_cutoff)
            .max((highpass_cutoff + 100.0).min(max_filter_cutoff));

        Self {
            channels,
            highpass: (0..channels)
                .map(|_| HighPassFilter::new(sample_rate, highpass_cutoff))
                .collect(),
            lowpass: (0..channels)
                .map(|_| LowPassFilter::new(sample_rate, lowpass_cutoff))
                .collect(),
            agc_gain: 1.0,
        }
    }

    fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }

        for frame in samples.chunks_exact_mut(self.channels) {
            for (channel, sample) in frame.iter_mut().enumerate() {
                let filtered = self.highpass[channel].process(*sample);
                *sample = self.lowpass[channel].process(filtered).clamp(-1.0, 1.0);
            }
        }

        let rms = (samples.iter().map(|sample| sample * sample).sum::<f32>()
            / samples.len() as f32)
            .sqrt();
        if rms > 0.0001 {
            let desired_gain =
                (VOICE_AGC_TARGET_RMS / rms).clamp(VOICE_AGC_MIN_GAIN, VOICE_AGC_MAX_GAIN);
            let coefficient = if desired_gain < self.agc_gain {
                VOICE_AGC_ATTACK
            } else {
                VOICE_AGC_RELEASE
            };
            self.agc_gain += (desired_gain - self.agc_gain) * coefficient;
        }

        for sample in samples {
            *sample = (*sample * self.agc_gain).clamp(-0.98, 0.98);
        }
    }
}

struct HighPassFilter {
    alpha: f32,
    previous_input: f32,
    previous_output: f32,
}

impl HighPassFilter {
    fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let dt = 1.0 / sample_rate.max(1.0);
        let rc = 1.0 / (std::f32::consts::TAU * cutoff_hz.max(1.0));
        Self {
            alpha: rc / (rc + dt),
            previous_input: 0.0,
            previous_output: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let output = self.alpha * (self.previous_output + input - self.previous_input);
        self.previous_input = input;
        self.previous_output = output;
        output
    }
}

struct LowPassFilter {
    alpha: f32,
    previous_output: f32,
}

impl LowPassFilter {
    fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let dt = 1.0 / sample_rate.max(1.0);
        let rc = 1.0 / (std::f32::consts::TAU * cutoff_hz.max(1.0));
        Self {
            alpha: dt / (rc + dt),
            previous_output: 0.0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        self.previous_output += self.alpha * (input - self.previous_output);
        self.previous_output
    }
}

fn build_input_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    builder: Arc<Mutex<InputFrameBuilder>>,
    capture_tx: mpsc::Sender<RawAudioFrame>,
) -> VoiceResult<cpal::Stream> {
    let config = supported.config();
    match supported.sample_format() {
        cpal::SampleFormat::F32 => device
            .build_input_stream(
                &config,
                move |data: &[f32], _| push_input_samples(data, &builder, &capture_tx),
                log_input_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build f32 microphone stream: {e}")),
        cpal::SampleFormat::I16 => device
            .build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect();
                    push_input_samples(&converted, &builder, &capture_tx);
                },
                log_input_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build i16 microphone stream: {e}")),
        cpal::SampleFormat::U16 => device
            .build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    push_input_samples(&converted, &builder, &capture_tx);
                },
                log_input_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build u16 microphone stream: {e}")),
        other => Err(format!("Unsupported microphone sample format: {other:?}")),
    }
}

fn select_input_config(
    device: &cpal::Device,
    preferred_sample_rate: u32,
) -> VoiceResult<cpal::SupportedStreamConfig> {
    match device.default_input_config() {
        Ok(config) if supported_sample_format(config.sample_format()) => Ok(config),
        Ok(config) => fallback_input_config(device, preferred_sample_rate).map_err(|fallback| {
            format!(
                "Default microphone sample format {:?} is unsupported, and no fallback configuration could be used: {fallback}",
                config.sample_format()
            )
        }),
        Err(default_error) => fallback_input_config(device, preferred_sample_rate).map_err(
            |fallback| {
                format!(
                    "Failed to read microphone configuration: {default_error}; fallback configuration failed: {fallback}"
                )
            },
        ),
    }
}

fn select_output_config(
    device: &cpal::Device,
    preferred_sample_rate: u32,
) -> VoiceResult<cpal::SupportedStreamConfig> {
    match device.default_output_config() {
        Ok(config) if supported_sample_format(config.sample_format()) => Ok(config),
        Ok(config) => fallback_output_config(device, preferred_sample_rate).map_err(|fallback| {
            format!(
                "Default speaker sample format {:?} is unsupported, and no fallback configuration could be used: {fallback}",
                config.sample_format()
            )
        }),
        Err(default_error) => fallback_output_config(device, preferred_sample_rate).map_err(
            |fallback| {
                format!(
                    "Failed to read speaker configuration: {default_error}; fallback configuration failed: {fallback}"
                )
            },
        ),
    }
}

fn fallback_input_config(
    device: &cpal::Device,
    preferred_sample_rate: u32,
) -> VoiceResult<cpal::SupportedStreamConfig> {
    let configs = device
        .supported_input_configs()
        .map_err(|e| format!("failed to enumerate microphone configurations: {e}"))?;
    choose_supported_config(configs, preferred_sample_rate)
        .ok_or_else(|| "no supported microphone configuration was found".to_string())
}

fn fallback_output_config(
    device: &cpal::Device,
    preferred_sample_rate: u32,
) -> VoiceResult<cpal::SupportedStreamConfig> {
    let configs = device
        .supported_output_configs()
        .map_err(|e| format!("failed to enumerate speaker configurations: {e}"))?;
    choose_supported_config(configs, preferred_sample_rate)
        .ok_or_else(|| "no supported speaker configuration was found".to_string())
}

fn choose_supported_config(
    configs: impl IntoIterator<Item = cpal::SupportedStreamConfigRange>,
    preferred_sample_rate: u32,
) -> Option<cpal::SupportedStreamConfig> {
    let preferred_sample_rate = preferred_sample_rate.max(1);
    configs
        .into_iter()
        .filter(|config| config.channels() > 0 && supported_sample_format(config.sample_format()))
        .map(|range| {
            let sample_rate = bounded_sample_rate(&range, preferred_sample_rate);
            range.with_sample_rate(sample_rate)
        })
        .min_by_key(|config| stream_config_penalty(config, preferred_sample_rate))
}

fn bounded_sample_rate(
    range: &cpal::SupportedStreamConfigRange,
    preferred_sample_rate: u32,
) -> cpal::SampleRate {
    let min = range.min_sample_rate().0;
    let max = range.max_sample_rate().0;
    cpal::SampleRate(preferred_sample_rate.clamp(min, max))
}

fn stream_config_penalty(
    config: &cpal::SupportedStreamConfig,
    preferred_sample_rate: u32,
) -> (u32, u16, u8) {
    (
        config.sample_rate().0.abs_diff(preferred_sample_rate),
        config.channels().abs_diff(1),
        sample_format_penalty(config.sample_format()),
    )
}

fn supported_sample_format(format: cpal::SampleFormat) -> bool {
    matches!(
        format,
        cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
    )
}

fn sample_format_penalty(format: cpal::SampleFormat) -> u8 {
    match format {
        cpal::SampleFormat::F32 => 0,
        cpal::SampleFormat::I16 => 1,
        cpal::SampleFormat::U16 => 2,
        _ => 3,
    }
}

fn build_output_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    output_queue: Arc<Mutex<VecDeque<f32>>>,
) -> VoiceResult<cpal::Stream> {
    let config = supported.config();
    match supported.sample_format() {
        cpal::SampleFormat::F32 => device
            .build_output_stream(
                &config,
                move |data: &mut [f32], _| fill_output_f32(data, &output_queue),
                log_output_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build f32 speaker stream: {e}")),
        cpal::SampleFormat::I16 => device
            .build_output_stream(
                &config,
                move |data: &mut [i16], _| fill_output_i16(data, &output_queue),
                log_output_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build i16 speaker stream: {e}")),
        cpal::SampleFormat::U16 => device
            .build_output_stream(
                &config,
                move |data: &mut [u16], _| fill_output_u16(data, &output_queue),
                log_output_stream_error,
                None,
            )
            .map_err(|e| format!("Failed to build u16 speaker stream: {e}")),
        other => Err(format!("Unsupported speaker sample format: {other:?}")),
    }
}

fn push_input_samples(
    samples: &[f32],
    builder: &Arc<Mutex<InputFrameBuilder>>,
    capture_tx: &mpsc::Sender<RawAudioFrame>,
) {
    let Ok(mut builder) = builder.try_lock() else {
        return;
    };
    for frame in builder.push_interleaved(samples) {
        let _ = capture_tx.try_send(frame);
    }
}

fn fill_output_f32(data: &mut [f32], output_queue: &Arc<Mutex<VecDeque<f32>>>) {
    if let Ok(mut queue) = output_queue.try_lock() {
        for sample in data {
            *sample = queue.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
        }
    } else {
        data.fill(0.0);
    }
}

fn fill_output_i16(data: &mut [i16], output_queue: &Arc<Mutex<VecDeque<f32>>>) {
    if let Ok(mut queue) = output_queue.try_lock() {
        for sample in data {
            let value = queue.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
            *sample = (value * i16::MAX as f32) as i16;
        }
    } else {
        data.fill(0);
    }
}

fn fill_output_u16(data: &mut [u16], output_queue: &Arc<Mutex<VecDeque<f32>>>) {
    if let Ok(mut queue) = output_queue.try_lock() {
        for sample in data {
            let value = queue.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
            *sample = ((value * 0.5 + 0.5) * u16::MAX as f32) as u16;
        }
    } else {
        data.fill(u16::MAX / 2);
    }
}

fn resample_output_frame(
    frame: &RawAudioFrame,
    source_sample_rate: u32,
    source_channels: usize,
    output_sample_rate: u32,
    output_channels: usize,
) -> Vec<f32> {
    let source_frames = frame.sample_frames();
    if source_frames == 0 || source_channels == 0 || output_channels == 0 {
        return Vec::new();
    }

    let source_sample_rate = source_sample_rate.max(1);
    let output_sample_rate = output_sample_rate.max(1);
    let output_frames = ((source_frames as f64 * output_sample_rate as f64
        / source_sample_rate as f64)
        .round()
        .max(1.0)) as usize;
    let ratio = source_sample_rate as f64 / output_sample_rate as f64;
    let mut out = Vec::with_capacity(output_frames * output_channels);

    for output_frame in 0..output_frames {
        let source_position = output_frame as f64 * ratio;
        let source_index = (source_position.floor() as usize).min(source_frames.saturating_sub(1));
        let next_index = (source_index + 1).min(source_frames.saturating_sub(1));
        let fraction = (source_position - source_index as f64) as f32;
        let source_base = source_index * source_channels;
        let next_base = next_index * source_channels;
        let source = &frame.samples[source_base..source_base + source_channels];
        let next = &frame.samples[next_base..next_base + source_channels];
        for output_channel in 0..output_channels {
            let a = channel_sample(source, output_channel, output_channels);
            let b = channel_sample(next, output_channel, output_channels);
            out.push((a + (b - a) * fraction).clamp(-1.0, 1.0));
        }
    }

    out
}

fn channel_sample(source: &[f32], target_channel: usize, target_channels: usize) -> f32 {
    if source.is_empty() {
        return 0.0;
    }
    if target_channels == 1 && source.len() > 1 {
        return source.iter().copied().sum::<f32>() / source.len() as f32;
    }
    if source.len() == 1 {
        return source[0];
    }
    source.get(target_channel).copied().unwrap_or(0.0)
}

fn log_input_stream_error(err: cpal::StreamError) {
    tracing::warn!(error = %err, "LXST microphone stream error");
}

fn log_output_stream_error(err: cpal::StreamError) {
    tracing::warn!(error = %err, "LXST speaker stream error");
}
