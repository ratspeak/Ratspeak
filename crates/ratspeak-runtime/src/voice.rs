//! LXST voice service and native audio bridge.

use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use lxst_core::{CallRole, Profile, RawAudioFrame, SignallingStatus, TELEPHONY_DESTINATION_NAME};
use lxst_telephony::{
    ActiveCallSnapshot, TelephonyControl, TelephonyRnsEndpoint, TelephonyRuntimeCore,
    TelephonyRuntimeSnapshot, TelephonyService, TelephonyServiceEvent,
};
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_transport::blackhole::BlackholeReason;
use rns_transport::messages::{TransportMessage, TransportQuery, TransportQueryResponse};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::db;
use crate::state::AppState;

const AUDIO_FRAME_CHANNEL_DEPTH: usize = 8;
const AUDIO_SPEAKER_CHANNEL_DEPTH: usize = 32;
const VOICE_AGC_TARGET_RMS: f32 = 0.14125375;
const VOICE_AGC_MIN_GAIN: f32 = 0.35;
const VOICE_AGC_MAX_GAIN: f32 = 3.0;
const VOICE_AGC_ATTACK: f32 = 0.20;
const VOICE_AGC_RELEASE: f32 = 0.04;
const VOICE_HIGHPASS_HZ: f32 = 250.0;
const VOICE_LOWPASS_HZ: f32 = 8_500.0;
const VOICE_NOISE_GATE_INITIAL_FLOOR_RMS: f32 = 0.003;
const VOICE_NOISE_GATE_OPEN_RMS: f32 = 0.006;
const VOICE_NOISE_GATE_CLOSE_RMS: f32 = 0.003;
const VOICE_NOISE_GATE_FLOOR_OPEN_MULTIPLIER: f32 = 2.2;
const VOICE_NOISE_GATE_FLOOR_CLOSE_MULTIPLIER: f32 = 1.25;
const VOICE_NOISE_GATE_CLOSED_GAIN: f32 = 0.35;
const VOICE_NOISE_GATE_ATTACK: f32 = 0.45;
const VOICE_NOISE_GATE_RELEASE: f32 = 0.06;
const VOICE_NOISE_GATE_FLOOR_FAST: f32 = 0.06;
const VOICE_NOISE_GATE_FLOOR_SLOW: f32 = 0.006;
const VOICE_NOISE_GATE_HOLD_MS: usize = 420;
const VOICE_PROFILE_UPGRADE_AFTER: Duration = Duration::ZERO;
const VOICE_PROFILE_SWITCH_COOLDOWN: Duration = Duration::from_secs(12);
const VOICE_PROFILE_DOWNGRADE_COOLDOWN: Duration = Duration::from_secs(20);
const VOICE_PROFILE_UPGRADE_LOCKOUT_AFTER_DOWNGRADE: Duration = Duration::from_secs(60);
const VOICE_PROFILE_DROPPED_FRAME_THRESHOLD: usize = 4;
const VOICE_AUDIO_FADE_IN_MS: usize = 20;
#[cfg_attr(target_os = "android", allow(dead_code))]
const VOICE_AUDIO_OUTPUT_PREBUFFER_MS: usize = 120;
const VOICE_AUDIO_RECOVERY_TICK: Duration = Duration::from_millis(1500);
const VOICE_AUDIO_RECOVERY_INITIAL_DELAY: Duration = Duration::from_millis(750);
const VOICE_AUDIO_RECOVERY_MAX_DELAY: Duration = Duration::from_secs(10);
const VOICE_OUTPUT_GAIN: f32 = 1.85;
const VOICE_OUTPUT_LIMIT: f32 = 0.98;
const VOICE_OUTPUT_LIMIT_CURVE: f32 = 0.35;
const VOICE_INITIAL_PROFILE: Profile = Profile::QualityHigh;
const LXMF_DELIVERY_DESTINATION_NAME: &str = "lxmf.delivery";
const VOICE_CONTACTS_ONLY_NOTICE: &str = "I'm only accepting calls from contacts.";
const VOICE_REJECTED_CALL_BLACKHOLE_THRESHOLD: u32 = 10;
const VOICE_REJECTED_CALL_ATTEMPT_WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
const VOICE_AUTO_BLACKHOLE_REASON: &str = "LXST call spam guard";
const VOICE_BLOCKED_CONTACT_BLACKHOLE_REASON: &str = "Blocked contact LXST call";
static VOICE_MICROPHONE_MUTED: AtomicBool = AtomicBool::new(false);

pub type VoiceResult<T> = Result<T, String>;

pub struct LxstVoiceServiceHandle {
    control_tx: mpsc::Sender<TelephonyControl>,
    audio_control_tx: mpsc::Sender<VoiceAudioControl>,
    service_task: Option<JoinHandle<()>>,
    event_task: Option<JoinHandle<()>>,
}

impl LxstVoiceServiceHandle {
    fn new(
        control_tx: mpsc::Sender<TelephonyControl>,
        audio_control_tx: mpsc::Sender<VoiceAudioControl>,
        service_task: JoinHandle<()>,
        event_task: JoinHandle<()>,
    ) -> Self {
        Self {
            control_tx,
            audio_control_tx,
            service_task: Some(service_task),
            event_task: Some(event_task),
        }
    }

    fn control_tx(&self) -> mpsc::Sender<TelephonyControl> {
        self.control_tx.clone()
    }

    fn audio_control_tx(&self) -> mpsc::Sender<VoiceAudioControl> {
        self.audio_control_tx.clone()
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

enum VoiceAudioControl {
    RestartSpeaker { speakerphone: bool },
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
    let (audio_control_tx, audio_control_rx) = mpsc::channel::<VoiceAudioControl>(8);
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
        runtime.block_on(drive_voice_events(
            event_state,
            event_control_tx,
            event_rx,
            audio_control_rx,
        ));
    });

    let handle =
        LxstVoiceServiceHandle::new(control_tx, audio_control_tx, service_task, event_task);
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
    emit_lxst_activity(state, "LXST voice service started", "", "standard");
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
    VOICE_MICROPHONE_MUTED.store(false, Ordering::Relaxed);

    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "service",
            "enabled": true,
            "running": false,
        }),
    );
    emit_lxst_activity(state, "LXST voice service stopped", "", "standard");
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
        "microphone_muted": microphone_muted(),
    })
}

pub fn set_microphone_muted(state: &AppState, muted: bool) -> VoiceResult<Value> {
    if voice_control_tx(state).is_none() {
        return Err("LXST voice service is not running".to_string());
    }
    VOICE_MICROPHONE_MUTED.store(muted, Ordering::Relaxed);
    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "audio_control",
            "microphone_muted": muted,
        }),
    );
    Ok(json!({
        "ok": true,
        "microphone_muted": muted,
    }))
}

pub async fn restart_speaker(state: &AppState, speakerphone: bool) -> VoiceResult<Value> {
    let tx = voice_audio_control_tx(state)
        .ok_or_else(|| "LXST voice service is not running".to_string())?;
    tx.send(VoiceAudioControl::RestartSpeaker { speakerphone })
        .await
        .map_err(|_| "LXST voice audio controls are not accepting commands".to_string())?;
    Ok(json!({
        "ok": true,
        "speakerphone": speakerphone,
    }))
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

pub async fn announce_if_running(state: &AppState) -> VoiceResult<bool> {
    let Some(tx) = voice_control_tx(state) else {
        return Ok(false);
    };
    tx.send(TelephonyControl::Announce)
        .await
        .map_err(|_| "LXST voice service is not accepting commands".to_string())?;
    Ok(true)
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

fn microphone_muted() -> bool {
    VOICE_MICROPHONE_MUTED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
struct IncomingCallPolicy {
    allowed: bool,
    reason: &'static str,
    send_contacts_only_notice: bool,
    auto_blackhole: bool,
    rejected_attempts: u32,
    remote_lxmf_destination: String,
    remote_public_key: Option<[u8; 64]>,
}

impl IncomingCallPolicy {
    fn allow(remote_lxmf_destination: String) -> Self {
        Self {
            allowed: true,
            reason: "allowed",
            send_contacts_only_notice: false,
            auto_blackhole: false,
            rejected_attempts: 0,
            remote_lxmf_destination,
            remote_public_key: None,
        }
    }

    fn reject(
        reason: &'static str,
        remote_lxmf_destination: String,
        remote_public_key: Option<[u8; 64]>,
        send_contacts_only_notice: bool,
        rejected_attempts: u32,
    ) -> Self {
        Self {
            allowed: false,
            reason,
            send_contacts_only_notice,
            auto_blackhole: rejected_attempts >= VOICE_REJECTED_CALL_BLACKHOLE_THRESHOLD,
            rejected_attempts,
            remote_lxmf_destination,
            remote_public_key,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct RemoteAnnounceInfo {
    direct: bool,
    public_key: Option<[u8; 64]>,
}

async fn evaluate_incoming_call_policy(
    state: &Arc<AppState>,
    remote_identity: [u8; 16],
) -> IncomingCallPolicy {
    let remote_lxmf_destination = lxmf_destination_for_identity(remote_identity);
    let (is_contact, is_blocked_contact) =
        contact_call_state(state, &remote_lxmf_destination).await;

    if is_network_blackholed(state, remote_identity).await {
        return IncomingCallPolicy::reject(
            "network_blackholed",
            remote_lxmf_destination,
            None,
            false,
            0,
        );
    }

    if is_blocked_contact {
        blackhole_voice_identity(
            state,
            remote_identity,
            BlackholeReason::Manual,
            Some(VOICE_BLOCKED_CONTACT_BLACKHOLE_REASON.to_string()),
        )
        .await;
        return IncomingCallPolicy::reject(
            "blocked_contact",
            remote_lxmf_destination,
            None,
            false,
            0,
        );
    }

    let announce_info = remote_announce_info(state, remote_identity).await;
    let direct = announce_info.direct || cached_zero_hop_path(state, remote_identity);
    if is_contact || direct {
        clear_rejected_call_attempts(state, remote_identity);
        return IncomingCallPolicy::allow(remote_lxmf_destination);
    }

    let rejected_attempts = record_rejected_call_attempt(state, remote_identity);
    IncomingCallPolicy::reject(
        "contacts_only",
        remote_lxmf_destination,
        announce_info.public_key,
        true,
        rejected_attempts,
    )
}

async fn contact_call_state(state: &AppState, remote_lxmf_destination: &str) -> (bool, bool) {
    let identity_id = crate::helpers::active_identity_id(state);
    if identity_id.is_empty() {
        return (false, false);
    }
    let dest_for_db = remote_lxmf_destination.to_string();
    let id_for_db = identity_id.clone();
    db::spawn_db(state.db.clone(), move |pool| {
        (
            db::get_contact(&pool, &dest_for_db, &id_for_db).is_some(),
            db::is_blocked(&pool, &dest_for_db, &id_for_db),
        )
    })
    .await
    .unwrap_or((false, false))
}

fn notify_incoming_call_if_background(
    state: &AppState,
    remote_lxmf_destination: &str,
    link_id: [u8; 16],
) {
    if state.is_foreground() || !state.native_notifications_enabled() {
        return;
    }
    let identity_id = crate::helpers::active_identity_id(state);
    let label = if identity_id.is_empty() {
        remote_lxmf_destination.to_string()
    } else {
        crate::contact_label_from_db(&state.db, remote_lxmf_destination, &identity_id)
    };
    let link_hex = hex::encode(link_id);
    state.emit_native_notification(ratspeak_core::NativeNotification::call(
        format!("Incoming call from {label}"),
        "Tap to open Ratspeak",
        format!("lxst:{link_hex}"),
        crate::stable_notification_id(&link_hex, 3_000_000),
    ));
}

async fn is_network_blackholed(state: &AppState, remote_identity: [u8; 16]) -> bool {
    matches!(
        transport_query(
            state,
            TransportQuery::IsBlackholed {
                hash: remote_identity,
            }
        )
        .await,
        Some(TransportQueryResponse::BoolResult(true))
    )
}

async fn blackhole_voice_identity(
    state: &AppState,
    remote_identity: [u8; 16],
    reason: BlackholeReason,
    reason_label: Option<String>,
) -> bool {
    let blackholed = matches!(
        transport_query(
            state,
            TransportQuery::BlackholeIdentity {
                hash: remote_identity,
                ttl: None,
                reason,
                reason_label,
            }
        )
        .await,
        Some(TransportQueryResponse::Ok)
    );
    if blackholed {
        state.emit_to_all(
            "blackhole_update",
            json!({ "identity_hash": hex::encode(remote_identity) }),
        );
    }
    blackholed
}

async fn remote_announce_info(state: &AppState, remote_identity: [u8; 16]) -> RemoteAnnounceInfo {
    let Some(TransportQueryResponse::Announces(announces)) =
        transport_query(state, TransportQuery::GetRecentAnnounces).await
    else {
        return RemoteAnnounceInfo::default();
    };

    let mut info = RemoteAnnounceInfo::default();
    for announce in announces {
        let Some(public_key) = announce.public_key else {
            continue;
        };
        if rns_crypto::sha::truncated_hash(&public_key) != remote_identity {
            continue;
        }
        if announce.hops == 0 {
            info.direct = true;
        }
        if info.public_key.is_none() {
            info.public_key = Some(public_key);
        }
        if info.direct && info.public_key.is_some() {
            break;
        }
    }
    info
}

fn cached_zero_hop_path(state: &AppState, remote_identity: [u8; 16]) -> bool {
    let lxmf_destination = lxmf_destination_for_identity(remote_identity);
    let telephony_destination = hex::encode(Destination::hash_from_name_and_identity(
        TELEPHONY_DESTINATION_NAME,
        Some(&remote_identity),
    ));
    let Ok(stats_guard) = state.last_stats.read() else {
        return false;
    };
    let Some(stats) = stats_guard.as_ref() else {
        return false;
    };
    [lxmf_destination.as_str(), telephony_destination.as_str()]
        .iter()
        .any(|dest| cached_path_hops(stats, dest).is_some_and(|hops| hops == 0))
}

fn cached_path_hops(stats: &Value, dest: &str) -> Option<u64> {
    if let Some(hops) = stats
        .get("path_index")
        .and_then(|index| index.get(dest))
        .and_then(|entry| entry.get("hops"))
        .and_then(|hops| hops.as_u64())
    {
        return Some(hops);
    }
    stats
        .get("path_table")
        .and_then(|table| table.as_array())
        .and_then(|table| {
            table.iter().find_map(|entry| {
                if entry.get("hash").and_then(|hash| hash.as_str()) == Some(dest) {
                    entry.get("hops").and_then(|hops| hops.as_u64())
                } else {
                    None
                }
            })
        })
}

fn record_rejected_call_attempt(state: &AppState, remote_identity: [u8; 16]) -> u32 {
    let key = hex::encode(remote_identity);
    let now = Instant::now();
    let Ok(mut attempts) = state.lxst_rejected_call_attempts.lock() else {
        return 1;
    };
    let entry = attempts.entry(key).or_insert((0, now));
    if now.duration_since(entry.1) > VOICE_REJECTED_CALL_ATTEMPT_WINDOW {
        *entry = (0, now);
    }
    entry.0 = entry.0.saturating_add(1);
    entry.1 = now;
    entry.0
}

fn clear_rejected_call_attempts(state: &AppState, remote_identity: [u8; 16]) {
    if let Ok(mut attempts) = state.lxst_rejected_call_attempts.lock() {
        attempts.remove(&hex::encode(remote_identity));
    }
}

fn spawn_contacts_only_notice(
    state: Arc<AppState>,
    remote_identity: [u8; 16],
    remote_lxmf_destination: String,
    remote_public_key: Option<[u8; 64]>,
) {
    tokio::spawn(async move {
        request_lxmf_path(&state, &remote_lxmf_destination);
        if let Some(public_key) = remote_public_key {
            cache_remote_lxmf_crypto(&state, &remote_lxmf_destination, public_key);
        }
        let sent = state
            .lxmf
            .lock()
            .ok()
            .and_then(|mut lxmf| {
                lxmf.as_mut().map(|mgr| {
                    mgr.send_ephemeral_opportunistic_message(
                        &remote_lxmf_destination,
                        VOICE_CONTACTS_ONLY_NOTICE,
                        "",
                    )
                })
            })
            .unwrap_or(false);
        if !sent {
            tracing::debug!(
                remote_identity = %hex::encode(remote_identity),
                remote_lxmf_destination = %remote_lxmf_destination,
                "could not queue LXMF contacts-only call notice"
            );
        }
    });
}

fn request_lxmf_path(state: &AppState, remote_lxmf_destination: &str) {
    let Some(dest) = hex_to_array16(remote_lxmf_destination) else {
        return;
    };
    if let Some(tx) = transport_sender(state) {
        let _ = tx.try_send(TransportMessage::RequestPath {
            destination_hash: dest,
        });
    }
}

fn cache_remote_lxmf_crypto(state: &AppState, remote_lxmf_destination: &str, public_key: [u8; 64]) {
    if let Ok(mut lxmf) = state.lxmf.lock()
        && let Some(mgr) = lxmf.as_mut()
    {
        mgr.update_remote_crypto(remote_lxmf_destination, &public_key, None);
    }
}

async fn transport_query(
    state: &AppState,
    query: TransportQuery,
) -> Option<TransportQueryResponse> {
    let tx = transport_sender(state)?;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    tx.send(TransportMessage::Rpc { query, response_tx })
        .await
        .ok()?;
    response_rx.await.ok()
}

fn transport_sender(
    state: &AppState,
) -> Option<mpsc::Sender<rns_transport::messages::TransportMessage>> {
    state
        .rns
        .read()
        .ok()
        .and_then(|rns| rns.as_ref().map(|mgr| mgr.handle.transport_tx.clone()))
}

fn hex_to_array16(value: &str) -> Option<[u8; 16]> {
    let bytes = hex::decode(value).ok()?;
    if bytes.len() != 16 {
        return None;
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn voice_control_tx(state: &AppState) -> Option<mpsc::Sender<TelephonyControl>> {
    state
        .lxst_voice
        .lock()
        .ok()
        .and_then(|voice| voice.as_ref().map(LxstVoiceServiceHandle::control_tx))
}

fn voice_audio_control_tx(state: &AppState) -> Option<mpsc::Sender<VoiceAudioControl>> {
    state
        .lxst_voice
        .lock()
        .ok()
        .and_then(|voice| voice.as_ref().map(LxstVoiceServiceHandle::audio_control_tx))
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
    mut audio_control_rx: mpsc::Receiver<VoiceAudioControl>,
) {
    let mut audio_session: Option<VoiceAudioSession> = None;
    let mut audio_failure: Option<VoiceAudioFailure> = None;
    let mut profile_adaptation = VoiceProfileAdaptation::new();
    let mut latest_snapshot: Option<TelephonyRuntimeSnapshot> = None;
    let mut suppressed_call_links: HashSet<[u8; 16]> = HashSet::new();
    let mut audio_recovery_tick = tokio::time::interval(VOICE_AUDIO_RECOVERY_TICK);
    audio_recovery_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        let event = tokio::select! {
            event = event_rx.recv() => event,
            control = audio_control_rx.recv() => {
                if let Some(control) = control {
                    handle_voice_audio_control(
                        &state,
                        &control_tx,
                        latest_snapshot.as_ref(),
                        &mut audio_session,
                        &mut audio_failure,
                        control,
                    )
                    .await;
                } else {
                    break;
                }
                continue;
            }
            _ = audio_recovery_tick.tick() => {
                if let Some(snapshot) = latest_snapshot.as_ref() {
                    reconcile_audio_session(
                        &state,
                        &control_tx,
                        snapshot,
                        &mut audio_session,
                        &mut audio_failure,
                    )
                    .await;
                }
                continue;
            }
        };
        let Some(event) = event else {
            break;
        };
        match event {
            TelephonyServiceEvent::IncomingCall {
                link_id,
                remote_identity,
            } => {
                let policy = evaluate_incoming_call_policy(&state, remote_identity).await;
                if !policy.allowed {
                    suppressed_call_links.insert(link_id);
                    tracing::info!(
                        link_id = %hex::encode(link_id),
                        remote_identity = %hex::encode(remote_identity),
                        reason = policy.reason,
                        rejected_attempts = policy.rejected_attempts,
                        "silently rejecting LXST incoming call"
                    );
                    if policy.send_contacts_only_notice {
                        spawn_contacts_only_notice(
                            Arc::clone(&state),
                            remote_identity,
                            policy.remote_lxmf_destination.clone(),
                            policy.remote_public_key,
                        );
                    }
                    if let Err(err) = control_tx
                        .send(TelephonyControl::Hangup {
                            ring_timeout: false,
                        })
                        .await
                    {
                        tracing::warn!(
                            error = %err,
                            "failed to hang up rejected LXST incoming call"
                        );
                    }
                    if policy.auto_blackhole {
                        let blackholed = blackhole_voice_identity(
                            &state,
                            remote_identity,
                            BlackholeReason::RateLimit,
                            Some(VOICE_AUTO_BLACKHOLE_REASON.to_string()),
                        )
                        .await;
                        if blackholed {
                            clear_rejected_call_attempts(&state, remote_identity);
                        }
                    }
                    continue;
                }

                let remote_lxmf_destination = policy.remote_lxmf_destination.clone();
                let payload = json!({
                    "type": "incoming",
                    "link_id": hex::encode(link_id),
                    "remote_identity": hex::encode(remote_identity),
                    "remote_lxmf_destination": remote_lxmf_destination.clone(),
                });
                notify_incoming_call_if_background(&state, &remote_lxmf_destination, link_id);
                state.emit_to_all("voice_incoming_call", payload.clone());
                state.emit_to_all("voice_call_update", payload);
                emit_lxst_activity(
                    &state,
                    "Incoming LXST call",
                    &voice_remote_detail(remote_identity, Some(link_id)),
                    "standard",
                );
            }
            TelephonyServiceEvent::OutgoingCallPending { remote_identity } => {
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "outgoing_pending",
                        "remote_identity": hex::encode(remote_identity),
                        "remote_lxmf_destination": lxmf_destination_for_identity(remote_identity),
                    }),
                );
                emit_lxst_activity(
                    &state,
                    "Resolving LXST call path",
                    &voice_remote_detail(remote_identity, None),
                    "standard",
                );
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
                emit_lxst_activity(
                    &state,
                    "LXST call link requested",
                    &voice_remote_detail(remote_identity, Some(link_id)),
                    "standard",
                );
            }
            TelephonyServiceEvent::OutgoingCallFailed {
                remote_identity,
                message,
            } => {
                state.emit_to_all(
                    "voice_call_update",
                    json!({
                        "type": "outgoing_failed",
                        "remote_identity": hex::encode(remote_identity),
                        "remote_lxmf_destination": lxmf_destination_for_identity(remote_identity),
                        "message": message.clone(),
                    }),
                );
                emit_lxst_activity(
                    &state,
                    "LXST call failed",
                    &format!("{} {}", voice_remote_detail(remote_identity, None), message),
                    "standard",
                );
            }
            TelephonyServiceEvent::CallTerminated { link_id, reason } => {
                if suppressed_call_links.remove(&link_id) {
                    continue;
                }
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
                emit_lxst_activity(
                    &state,
                    "LXST call ended",
                    &format!(
                        "link={} reason={}",
                        hex::encode(link_id),
                        reason.map(status_key).unwrap_or("none")
                    ),
                    "standard",
                );
            }
            TelephonyServiceEvent::Snapshot(snapshot) => {
                if snapshot
                    .active_call
                    .as_ref()
                    .is_some_and(|active| suppressed_call_links.contains(&active.link_id))
                {
                    latest_snapshot = Some(snapshot);
                    continue;
                }
                latest_snapshot = Some(snapshot.clone());
                maybe_adapt_voice_profile(&state, &control_tx, &snapshot, &mut profile_adaptation)
                    .await;
                if profile_switch_pending(&profile_adaptation, &snapshot) {
                    stop_audio_session(audio_session.take(), &control_tx).await;
                    audio_failure = None;
                } else {
                    reconcile_audio_session(
                        &state,
                        &control_tx,
                        &snapshot,
                        &mut audio_session,
                        &mut audio_failure,
                    )
                    .await;
                }
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
                        "message": message.clone(),
                    }),
                );
                emit_lxst_activity(&state, "LXST voice error", &message, "standard");
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

async fn handle_voice_audio_control(
    state: &AppState,
    control_tx: &mpsc::Sender<TelephonyControl>,
    latest_snapshot: Option<&TelephonyRuntimeSnapshot>,
    audio_session: &mut Option<VoiceAudioSession>,
    audio_failure: &mut Option<VoiceAudioFailure>,
    control: VoiceAudioControl,
) {
    match control {
        VoiceAudioControl::RestartSpeaker { speakerphone } => {
            let Some(snapshot) = latest_snapshot else {
                return;
            };
            let Some(active) = snapshot.active_call.as_ref() else {
                return;
            };
            if active.status != SignallingStatus::Established {
                return;
            }

            if let Some(session) = audio_session.as_mut() {
                match session.restart_speaker(control_tx.clone()).await {
                    Ok(()) => {
                        state.emit_to_all(
                            "voice_call_update",
                            json!({
                                "type": "audio",
                                "state": "restarted",
                                "link_id": hex::encode(session.link_id),
                                "profile": profile_key(session.profile),
                                "running": session.running(),
                                "microphone": session.microphone,
                                "microphone_muted": microphone_muted(),
                                "speaker": session.speaker,
                                "speakerphone": speakerphone,
                                "warnings": session.warnings.clone(),
                            }),
                        );
                    }
                    Err(message) => {
                        state.emit_to_all(
                            "voice_call_update",
                            json!({
                                "type": "audio",
                                "state": "speaker_restart_failed",
                                "link_id": hex::encode(session.link_id),
                                "profile": profile_key(session.profile),
                                "running": session.running(),
                                "microphone": session.microphone,
                                "microphone_muted": microphone_muted(),
                                "speaker": session.speaker,
                                "speakerphone": speakerphone,
                                "warnings": [message],
                            }),
                        );
                    }
                }
                emit_snapshot(state, snapshot, audio_session.as_ref());
            } else {
                *audio_failure = None;
                reconcile_audio_session(state, control_tx, snapshot, audio_session, audio_failure)
                    .await;
                emit_snapshot(state, snapshot, audio_session.as_ref());
            }
        }
    }
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
        if let Some(session) = audio_session.as_mut() {
            if session.retry_missing_audio(control_tx.clone()).await {
                emit_audio_session_state(state, "recovered", session);
            }
        }
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
                    "microphone_muted": microphone_muted(),
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
) -> bool {
    let Some(active) = snapshot.active_call.as_ref() else {
        adaptation.reset();
        return false;
    };

    if active.status != SignallingStatus::Established {
        adaptation.reset_for_link(active.link_id);
        return false;
    }

    let current = active.profile.unwrap_or(Profile::DEFAULT);
    let Some(next) = adaptation.next_profile(active.link_id, current) else {
        return false;
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
        true
    } else {
        false
    }
}

fn profile_switch_pending(
    adaptation: &VoiceProfileAdaptation,
    snapshot: &TelephonyRuntimeSnapshot,
) -> bool {
    snapshot.active_call.as_ref().is_some_and(|active| {
        let current = active.profile.unwrap_or(Profile::DEFAULT);
        active.status == SignallingStatus::Established
            && adaptation.pending_switch(active.link_id, current)
    })
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
    VOICE_MICROPHONE_MUTED.store(false, Ordering::Relaxed);
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
                "microphone_muted": microphone_muted(),
                "speaker": session.speaker,
            })),
            "active_call": snapshot.active_call.as_ref().map(active_call_payload),
        }),
    );
}

fn emit_audio_session_state(state: &AppState, state_key: &str, session: &VoiceAudioSession) {
    state.emit_to_all(
        "voice_call_update",
        json!({
            "type": "audio",
            "state": state_key,
            "link_id": hex::encode(session.link_id),
            "profile": profile_key(session.profile),
            "running": session.running(),
            "microphone": session.microphone,
            "microphone_muted": microphone_muted(),
            "speaker": session.speaker,
            "warnings": session.warnings.clone(),
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

fn emit_lxst_activity(state: &AppState, message: &str, detail: &str, level: &str) {
    state.emit_network_event("lxst", message, detail, level);
}

fn voice_remote_detail(remote_identity: [u8; 16], link_id: Option<[u8; 16]>) -> String {
    let mut detail = format!(
        "identity={} lxmf={}",
        hex::encode(remote_identity),
        lxmf_destination_for_identity(remote_identity)
    );
    if let Some(link_id) = link_id {
        detail.push_str(" link=");
        detail.push_str(&hex::encode(link_id));
    }
    detail
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

    fn pending_switch(&self, link_id: [u8; 16], current: Profile) -> bool {
        self.link_id == Some(link_id)
            && self
                .requested_profile
                .is_some_and(|requested| requested != current)
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
    microphone_retry_attempts: u32,
    speaker_retry_attempts: u32,
    next_microphone_retry_at: Option<Instant>,
    next_speaker_retry_at: Option<Instant>,
    _input_stream: Option<cpal::Stream>,
    _output_stream: Option<VoiceOutputStream>,
    sink_task: Option<JoinHandle<()>>,
}

#[cfg(target_os = "android")]
type VoiceOutputStream = AndroidVoiceOutput;

#[cfg(not(target_os = "android"))]
type VoiceOutputStream = cpal::Stream;

#[cfg(target_os = "android")]
struct AndroidVoiceOutput;

#[cfg(target_os = "android")]
impl Drop for AndroidVoiceOutput {
    fn drop(&mut self) {
        android_voice_audio::stop();
    }
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

fn audio_recovery_delay(attempts: u32) -> Duration {
    let shift = attempts.min(4);
    let factor = 1u32 << shift;
    (VOICE_AUDIO_RECOVERY_INITIAL_DELAY * factor).min(VOICE_AUDIO_RECOVERY_MAX_DELAY)
}

impl VoiceAudioSession {
    fn running(&self) -> bool {
        self.microphone || self.speaker
    }

    fn schedule_microphone_retry(&mut self) {
        let delay = audio_recovery_delay(self.microphone_retry_attempts);
        self.microphone_retry_attempts = self.microphone_retry_attempts.saturating_add(1);
        self.next_microphone_retry_at = Some(Instant::now() + delay);
    }

    fn schedule_speaker_retry(&mut self) {
        let delay = audio_recovery_delay(self.speaker_retry_attempts);
        self.speaker_retry_attempts = self.speaker_retry_attempts.saturating_add(1);
        self.next_speaker_retry_at = Some(Instant::now() + delay);
    }

    async fn retry_missing_audio(&mut self, control_tx: mpsc::Sender<TelephonyControl>) -> bool {
        let mut recovered = false;
        let now = Instant::now();

        if !self.microphone
            && self
                .next_microphone_retry_at
                .is_some_and(|retry_at| now >= retry_at)
        {
            let host = cpal::default_host();
            let target_channels = usize::from(self.profile.channels());
            let target_sample_rate = self.profile.sample_rate_hz();
            let target_frames = self.profile.sample_frames_per_packet();
            match start_microphone_side(
                &host,
                self.profile,
                control_tx.clone(),
                target_channels,
                target_sample_rate,
                target_frames,
            )
            .await
            {
                Ok(stream) => {
                    tracing::info!(
                        link_id = %hex::encode(self.link_id),
                        profile = profile_key(self.profile),
                        "recovered LXST microphone stream"
                    );
                    self._input_stream = Some(stream);
                    self.microphone = true;
                    self.microphone_retry_attempts = 0;
                    self.next_microphone_retry_at = None;
                    recovered = true;
                }
                Err(message) => {
                    tracing::warn!(
                        link_id = %hex::encode(self.link_id),
                        profile = profile_key(self.profile),
                        error = %message,
                        "LXST microphone recovery failed"
                    );
                    self.schedule_microphone_retry();
                }
            }
        }

        if !self.speaker
            && self
                .next_speaker_retry_at
                .is_some_and(|retry_at| now >= retry_at)
        {
            let host = cpal::default_host();
            match start_speaker_side(&host, control_tx, self.profile.sample_rate_hz()).await {
                Ok((stream, sink_task)) => {
                    tracing::info!(
                        link_id = %hex::encode(self.link_id),
                        profile = profile_key(self.profile),
                        "recovered LXST speaker stream"
                    );
                    self._output_stream = Some(stream);
                    self.sink_task = Some(sink_task);
                    self.speaker = true;
                    self.speaker_retry_attempts = 0;
                    self.next_speaker_retry_at = None;
                    recovered = true;
                }
                Err(message) => {
                    tracing::warn!(
                        link_id = %hex::encode(self.link_id),
                        profile = profile_key(self.profile),
                        error = %message,
                        "LXST speaker recovery failed"
                    );
                    self.schedule_speaker_retry();
                }
            }
        }

        if recovered && self.microphone && self.speaker {
            self.warnings.clear();
        }

        recovered
    }

    async fn restart_speaker(
        &mut self,
        control_tx: mpsc::Sender<TelephonyControl>,
    ) -> VoiceResult<()> {
        let had_microphone = self.microphone;
        let had_speaker = self.speaker;
        if self.microphone {
            let _ = control_tx.send(TelephonyControl::StopOpusStream).await;
        }
        if self.speaker {
            let _ = control_tx
                .send(TelephonyControl::StopOpusReceiveStream)
                .await;
        }
        if let Some(task) = self.sink_task.take() {
            await_or_abort(task).await;
        }
        self._output_stream.take();
        self._input_stream.take();
        self.microphone = false;
        self.speaker = false;

        let host = cpal::default_host();
        let target_channels = usize::from(self.profile.channels());
        let target_sample_rate = self.profile.sample_rate_hz();
        let target_frames = self.profile.sample_frames_per_packet();
        let mut restart_warnings = Vec::new();

        match start_microphone_side(
            &host,
            self.profile,
            control_tx.clone(),
            target_channels,
            target_sample_rate,
            target_frames,
        )
        .await
        {
            Ok(stream) => {
                self._input_stream = Some(stream);
                self.microphone = true;
                self.microphone_retry_attempts = 0;
                self.next_microphone_retry_at = None;
            }
            Err(message) => {
                restart_warnings.push(message);
                self.schedule_microphone_retry();
            }
        }

        match start_speaker_side(&host, control_tx, target_sample_rate).await {
            Ok((stream, sink_task)) => {
                self._output_stream = Some(stream);
                self.sink_task = Some(sink_task);
                self.speaker = true;
                self.speaker_retry_attempts = 0;
                self.next_speaker_retry_at = None;
            }
            Err(message) => {
                restart_warnings.push(message);
                self.schedule_speaker_retry();
            }
        }

        if (had_microphone && !self.microphone) || (had_speaker && !self.speaker) {
            let detail = restart_warnings.join("; ");
            self.warnings.extend(restart_warnings);
            return Err(if detail.is_empty() {
                "Failed to restart LXST audio route".to_string()
            } else {
                format!("Failed to restart LXST audio route: {detail}")
            });
        }

        self.warnings.extend(restart_warnings);
        Ok(())
    }

    async fn start(
        link_id: [u8; 16],
        profile: Profile,
        control_tx: mpsc::Sender<TelephonyControl>,
    ) -> VoiceResult<Self> {
        VOICE_MICROPHONE_MUTED.store(false, Ordering::Relaxed);
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

        let (output_stream, sink_task) =
            match start_speaker_side(&host, control_tx, target_sample_rate).await {
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

        let mut session = Self {
            link_id,
            profile,
            microphone: input_stream.is_some(),
            speaker: output_stream.is_some(),
            warnings,
            microphone_retry_attempts: 0,
            speaker_retry_attempts: 0,
            next_microphone_retry_at: None,
            next_speaker_retry_at: None,
            _input_stream: input_stream,
            _output_stream: output_stream,
            sink_task,
        };
        if !session.microphone {
            session.schedule_microphone_retry();
        }
        if !session.speaker {
            session.schedule_speaker_retry();
        }
        Ok(session)
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
    target_sample_rate: u32,
) -> VoiceResult<(VoiceOutputStream, JoinHandle<()>)> {
    #[cfg(target_os = "android")]
    {
        let _ = host;
        return start_android_speaker_side(control_tx, target_sample_rate).await;
    }

    #[cfg(not(target_os = "android"))]
    {
        let output_device = host
            .default_output_device()
            .ok_or_else(|| "No default speaker is available".to_string())?;
        let output_config = select_output_config(&output_device, target_sample_rate)?;
        let (speaker_tx, mut speaker_rx) =
            mpsc::channel::<RawAudioFrame>(AUDIO_SPEAKER_CHANNEL_DEPTH);

        let output_channels = usize::from(output_config.channels());
        let output_sample_rate = output_config.sample_rate().0;
        let max_queue_samples = output_channels * output_sample_rate as usize;
        let prebuffer_samples =
            output_channels * output_sample_rate as usize * VOICE_AUDIO_OUTPUT_PREBUFFER_MS / 1000;
        let output_queue = Arc::new(Mutex::new(AudioOutputQueue::new(
            max_queue_samples,
            prebuffer_samples,
        )));
        let output_stream =
            build_output_stream(&output_device, &output_config, Arc::clone(&output_queue))?;

        let sink_task = tokio::spawn(async move {
            let mut fade_samples_remaining = fade_sample_count(output_sample_rate, output_channels);
            let fade_samples_total = fade_samples_remaining;
            while let Some(frame) = speaker_rx.recv().await {
                let mut converted = resample_output_frame(
                    &frame,
                    target_sample_rate,
                    output_sample_rate,
                    output_channels,
                );
                apply_fade_in(
                    &mut converted,
                    &mut fade_samples_remaining,
                    fade_samples_total,
                );
                apply_voice_output_leveling(&mut converted);
                if let Ok(mut queue) = output_queue.lock() {
                    queue.push_samples(converted);
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
}

#[cfg(target_os = "android")]
async fn start_android_speaker_side(
    control_tx: mpsc::Sender<TelephonyControl>,
    target_sample_rate: u32,
) -> VoiceResult<(AndroidVoiceOutput, JoinHandle<()>)> {
    const ANDROID_OUTPUT_SAMPLE_RATE: u32 = 48_000;
    const ANDROID_OUTPUT_CHANNELS: usize = 1;

    android_voice_audio::start(ANDROID_OUTPUT_SAMPLE_RATE, ANDROID_OUTPUT_CHANNELS)?;
    let (speaker_tx, mut speaker_rx) = mpsc::channel::<RawAudioFrame>(AUDIO_SPEAKER_CHANNEL_DEPTH);

    let sink_task = tokio::task::spawn_blocking(move || {
        let mut fade_samples_remaining =
            fade_sample_count(ANDROID_OUTPUT_SAMPLE_RATE, ANDROID_OUTPUT_CHANNELS);
        let fade_samples_total = fade_samples_remaining;
        while let Some(frame) = speaker_rx.blocking_recv() {
            let mut converted = resample_output_frame(
                &frame,
                target_sample_rate,
                ANDROID_OUTPUT_SAMPLE_RATE,
                ANDROID_OUTPUT_CHANNELS,
            );
            apply_fade_in(
                &mut converted,
                &mut fade_samples_remaining,
                fade_samples_total,
            );
            apply_voice_output_leveling(&mut converted);
            if let Err(err) = write_android_voice_samples(&converted) {
                tracing::warn!(error = %err, "LXST Android voice output write failed");
                if android_voice_audio::start(ANDROID_OUTPUT_SAMPLE_RATE, ANDROID_OUTPUT_CHANNELS)
                    .is_ok()
                {
                    let _ = write_android_voice_samples(&converted);
                } else {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });

    if let Err(e) = control_tx
        .send(TelephonyControl::StartOpusReceiveStream { frames: speaker_tx })
        .await
    {
        sink_task.abort();
        android_voice_audio::stop();
        return Err(format!("Failed to start LXST Android speaker stream: {e}"));
    }

    Ok((AndroidVoiceOutput, sink_task))
}

#[cfg(target_os = "android")]
fn write_android_voice_samples(samples: &[f32]) -> VoiceResult<()> {
    let mut offset = 0usize;
    let mut empty_writes = 0u8;
    while offset < samples.len() {
        match android_voice_audio::write(&samples[offset..]) {
            Ok(0) => {
                empty_writes = empty_writes.saturating_add(1);
                if empty_writes >= 4 {
                    return Err("Android voice AudioTrack stopped accepting samples".to_string());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(written) => {
                let remaining = samples.len() - offset;
                offset += written.min(remaining);
                empty_writes = 0;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
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
    fade_samples_remaining: usize,
    fade_samples_total: usize,
}

impl InputFrameBuilder {
    fn new(
        source_channels: usize,
        source_sample_rate: u32,
        target_channels: usize,
        target_sample_rate: u32,
        target_frames: usize,
    ) -> Self {
        let target_channels = target_channels.max(1);
        let target_sample_rate = target_sample_rate.max(1);
        let fade_samples_total = fade_sample_count(target_sample_rate, target_channels);
        Self {
            source_channels: source_channels.max(1),
            source_sample_rate: source_sample_rate.max(1),
            target_channels,
            target_sample_rate,
            target_samples_per_frame: target_frames * target_channels,
            source_samples: Vec::with_capacity(target_frames * target_channels * 2),
            source_cursor: 0.0,
            pending_frame: Vec::with_capacity(target_frames * target_channels),
            processor: VoiceInputProcessor::new(target_channels, target_sample_rate),
            fade_samples_remaining: fade_samples_total,
            fade_samples_total,
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
                apply_fade_in(
                    &mut samples,
                    &mut self.fade_samples_remaining,
                    self.fade_samples_total,
                );
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

    fn clear_pending_audio(&mut self) {
        self.source_samples.clear();
        self.pending_frame.clear();
        self.source_cursor = 0.0;
        self.fade_samples_remaining = self.fade_samples_total;
    }
}

struct VoiceInputProcessor {
    channels: usize,
    highpass: Vec<HighPassFilter>,
    lowpass: Vec<LowPassFilter>,
    agc_gain: f32,
    noise_floor_rms: f32,
    gate_gain: f32,
    gate_hold_samples: usize,
    gate_hold_samples_total: usize,
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
            noise_floor_rms: VOICE_NOISE_GATE_INITIAL_FLOOR_RMS,
            gate_gain: 1.0,
            gate_hold_samples: 0,
            gate_hold_samples_total: ((sample_rate as usize) * VOICE_NOISE_GATE_HOLD_MS / 1000)
                .max(1),
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

        let rms = frame_rms(samples);
        let gate_open = self.update_noise_gate(rms, samples.len() / self.channels);
        self.apply_noise_gate(samples);

        let gated_rms = frame_rms(samples);
        if gate_open && gated_rms > 0.0001 {
            let desired_gain =
                (VOICE_AGC_TARGET_RMS / gated_rms).clamp(VOICE_AGC_MIN_GAIN, VOICE_AGC_MAX_GAIN);
            let coefficient = if desired_gain < self.agc_gain {
                VOICE_AGC_ATTACK
            } else {
                VOICE_AGC_RELEASE
            };
            self.agc_gain += (desired_gain - self.agc_gain) * coefficient;
        } else {
            self.agc_gain += (1.0 - self.agc_gain) * VOICE_AGC_RELEASE;
        }

        for sample in samples {
            *sample = (*sample * self.agc_gain).clamp(-0.98, 0.98);
        }
    }

    fn update_noise_gate(&mut self, rms: f32, sample_frames: usize) -> bool {
        let open_threshold = VOICE_NOISE_GATE_OPEN_RMS
            .max(self.noise_floor_rms * VOICE_NOISE_GATE_FLOOR_OPEN_MULTIPLIER);
        let close_threshold = VOICE_NOISE_GATE_CLOSE_RMS
            .max(self.noise_floor_rms * VOICE_NOISE_GATE_FLOOR_CLOSE_MULTIPLIER);

        if rms < open_threshold {
            let coefficient = if rms < self.noise_floor_rms {
                VOICE_NOISE_GATE_FLOOR_FAST
            } else {
                VOICE_NOISE_GATE_FLOOR_SLOW
            };
            self.noise_floor_rms += (rms - self.noise_floor_rms) * coefficient;
            self.noise_floor_rms = self
                .noise_floor_rms
                .clamp(0.0001, VOICE_NOISE_GATE_OPEN_RMS);
        }

        let active = rms >= open_threshold || self.gate_hold_samples > 0;
        if rms >= open_threshold {
            self.gate_hold_samples = self.gate_hold_samples_total;
        } else if rms < close_threshold {
            self.gate_hold_samples = self.gate_hold_samples.saturating_sub(sample_frames);
        }

        let target_gain = if active {
            1.0
        } else {
            VOICE_NOISE_GATE_CLOSED_GAIN
        };
        let coefficient = if target_gain > self.gate_gain {
            VOICE_NOISE_GATE_ATTACK
        } else {
            VOICE_NOISE_GATE_RELEASE
        };
        self.gate_gain += (target_gain - self.gate_gain) * coefficient;
        self.gate_gain = self.gate_gain.clamp(VOICE_NOISE_GATE_CLOSED_GAIN, 1.0);
        active
    }

    fn apply_noise_gate(&self, samples: &mut [f32]) {
        if (self.gate_gain - 1.0).abs() < f32::EPSILON {
            return;
        }
        for sample in samples {
            *sample *= self.gate_gain;
        }
    }
}

fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
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

#[cfg_attr(target_os = "android", allow(dead_code))]
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

#[cfg_attr(target_os = "android", allow(dead_code))]
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

#[cfg_attr(target_os = "android", allow(dead_code))]
fn build_output_stream(
    device: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    output_queue: Arc<Mutex<AudioOutputQueue>>,
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
    if microphone_muted() {
        builder.clear_pending_audio();
        return;
    }
    for frame in builder.push_interleaved(samples) {
        let _ = capture_tx.try_send(frame);
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
struct AudioOutputQueue {
    samples: VecDeque<f32>,
    max_samples: usize,
    prebuffer_samples_remaining: usize,
}

#[cfg_attr(target_os = "android", allow(dead_code))]
impl AudioOutputQueue {
    fn new(max_samples: usize, prebuffer_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples.min(prebuffer_samples).max(1)),
            max_samples: max_samples.max(1),
            prebuffer_samples_remaining: prebuffer_samples,
        }
    }

    fn push_samples(&mut self, samples: Vec<f32>) {
        if samples.len() >= self.max_samples {
            let keep_from = samples.len() - self.max_samples;
            self.samples.clear();
            self.samples.extend(samples.into_iter().skip(keep_from));
            return;
        }

        let projected = self.samples.len().saturating_add(samples.len());
        if projected > self.max_samples {
            let drop_count = projected - self.max_samples;
            self.samples.drain(..drop_count.min(self.samples.len()));
        }
        self.samples.extend(samples);
    }

    fn pop_sample(&mut self) -> f32 {
        if self.prebuffer_samples_remaining > 0 {
            self.prebuffer_samples_remaining -= 1;
            return 0.0;
        }
        self.samples.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0)
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
fn fill_output_f32(data: &mut [f32], output_queue: &Arc<Mutex<AudioOutputQueue>>) {
    if let Ok(mut queue) = output_queue.lock() {
        for sample in data {
            *sample = queue.pop_sample();
        }
    } else {
        data.fill(0.0);
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
fn fill_output_i16(data: &mut [i16], output_queue: &Arc<Mutex<AudioOutputQueue>>) {
    if let Ok(mut queue) = output_queue.lock() {
        for sample in data {
            let value = queue.pop_sample();
            *sample = (value * i16::MAX as f32) as i16;
        }
    } else {
        data.fill(0);
    }
}

#[cfg_attr(target_os = "android", allow(dead_code))]
fn fill_output_u16(data: &mut [u16], output_queue: &Arc<Mutex<AudioOutputQueue>>) {
    if let Ok(mut queue) = output_queue.lock() {
        for sample in data {
            let value = queue.pop_sample();
            *sample = ((value * 0.5 + 0.5) * u16::MAX as f32) as u16;
        }
    } else {
        data.fill(u16::MAX / 2);
    }
}

fn resample_output_frame(
    frame: &RawAudioFrame,
    source_sample_rate: u32,
    output_sample_rate: u32,
    output_channels: usize,
) -> Vec<f32> {
    let source_channels = usize::from(frame.channels).max(1);
    let source_frames = frame.samples.len() / source_channels;
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
        let Some(source) = frame
            .samples
            .get(source_base..source_base + source_channels)
        else {
            break;
        };
        let Some(next) = frame.samples.get(next_base..next_base + source_channels) else {
            break;
        };
        for output_channel in 0..output_channels {
            let a = channel_sample(source, output_channel, output_channels);
            let b = channel_sample(next, output_channel, output_channels);
            out.push((a + (b - a) * fraction).clamp(-1.0, 1.0));
        }
    }

    out
}

fn fade_sample_count(sample_rate: u32, channels: usize) -> usize {
    (sample_rate.max(1) as usize * VOICE_AUDIO_FADE_IN_MS / 1000) * channels.max(1)
}

fn apply_fade_in(samples: &mut [f32], remaining: &mut usize, total: usize) {
    if samples.is_empty() || *remaining == 0 || total == 0 {
        return;
    }

    let already_faded = total.saturating_sub(*remaining);
    let count = samples.len().min(*remaining);
    for (index, sample) in samples.iter_mut().take(count).enumerate() {
        let position = already_faded + index + 1;
        let gain = (position as f32 / total as f32).clamp(0.0, 1.0);
        *sample *= gain;
    }
    *remaining -= count;
}

fn apply_voice_output_leveling(samples: &mut [f32]) {
    for sample in samples {
        let boosted = if sample.is_finite() {
            *sample * VOICE_OUTPUT_GAIN
        } else {
            0.0
        };
        *sample = (boosted / (1.0 + VOICE_OUTPUT_LIMIT_CURVE * boosted.abs()))
            .clamp(-VOICE_OUTPUT_LIMIT, VOICE_OUTPUT_LIMIT);
    }
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

#[cfg_attr(target_os = "android", allow(dead_code))]
fn log_output_stream_error(err: cpal::StreamError) {
    tracing::warn!(error = %err, "LXST speaker stream error");
}

#[cfg(target_os = "android")]
mod android_voice_audio {
    use std::sync::OnceLock;

    use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};

    use super::VoiceResult;

    const CLASS_NAME: &str = "org.ratspeak.android.RatspeakVoiceAudio";
    static APP_CLASS_LOADER: OnceLock<GlobalRef> = OnceLock::new();

    pub fn start(sample_rate_hz: u32, channels: usize) -> VoiceResult<()> {
        with_env(|env| {
            let class = find_app_class(env, CLASS_NAME)?;
            let ok = env
                .call_static_method(
                    class,
                    "start",
                    "(II)Z",
                    &[
                        JValue::Int(sample_rate_hz as i32),
                        JValue::Int(channels as i32),
                    ],
                )
                .map_err(|e| {
                    clear_exception(env);
                    format!("RatspeakVoiceAudio.start: {e}")
                })?
                .z()
                .map_err(|e| format!("RatspeakVoiceAudio.start result: {e}"))?;
            if ok {
                Ok(())
            } else {
                let detail = last_error(env, class);
                if detail.is_empty() {
                    Err("Android voice AudioTrack could not be initialized".to_string())
                } else {
                    Err(format!(
                        "Android voice AudioTrack could not be initialized: {detail}"
                    ))
                }
            }
        })
    }

    pub fn write(samples: &[f32]) -> VoiceResult<usize> {
        if samples.is_empty() {
            return Ok(0);
        }
        with_env(|env| {
            let class = find_app_class(env, CLASS_NAME)?;
            let array = env
                .new_float_array(samples.len() as i32)
                .map_err(|e| format!("voice float array: {e}"))?;
            env.set_float_array_region(array, 0, samples)
                .map_err(|e| format!("voice float array region: {e}"))?;
            let written = env
                .call_static_method(
                    class,
                    "write",
                    "([FI)I",
                    &[
                        JValue::Object(JObject::from(array)),
                        JValue::Int(samples.len() as i32),
                    ],
                )
                .map_err(|e| {
                    clear_exception(env);
                    format!("RatspeakVoiceAudio.write: {e}")
                })?
                .i()
                .map_err(|e| format!("RatspeakVoiceAudio.write result: {e}"))?;
            if written < 0 {
                Err(format!(
                    "Android voice AudioTrack write failed with code {written}"
                ))
            } else {
                Ok(written as usize)
            }
        })
    }

    pub fn stop() {
        let _ = with_env(|env| {
            let class = find_app_class(env, CLASS_NAME)?;
            env.call_static_method(class, "stop", "()V", &[])
                .map_err(|e| {
                    clear_exception(env);
                    format!("RatspeakVoiceAudio.stop: {e}")
                })?;
            Ok(())
        });
    }

    fn last_error(env: &jni::JNIEnv, class: JClass) -> String {
        let value = match env
            .call_static_method(class, "lastError", "()Ljava/lang/String;", &[])
            .and_then(|result| result.l())
        {
            Ok(value) => value,
            Err(_) => {
                clear_exception(env);
                return String::new();
            }
        };
        if value.is_null() {
            return String::new();
        }
        env.get_string(JString::from(value))
            .map(|s| s.into())
            .unwrap_or_default()
    }

    fn with_env<F, T>(f: F) -> VoiceResult<T>
    where
        F: FnOnce(&jni::JNIEnv) -> VoiceResult<T>,
    {
        let vm = rns_interface::android_usb::java_vm()
            .ok_or_else(|| "JavaVM not initialized for Android voice audio".to_string())?;
        let env = vm
            .attach_current_thread()
            .map_err(|e| format!("JNI attach for Android voice audio: {e}"))?;
        f(&env)
    }

    fn get_app_context<'a>(env: &'a jni::JNIEnv) -> VoiceResult<JObject<'a>> {
        let activity_thread = env.find_class("android/app/ActivityThread").map_err(|e| {
            clear_exception(env);
            format!("ActivityThread class: {e}")
        })?;
        let app = env
            .call_static_method(
                activity_thread,
                "currentApplication",
                "()Landroid/app/Application;",
                &[],
            )
            .map_err(|e| {
                clear_exception(env);
                format!("currentApplication: {e}")
            })?
            .l()
            .map_err(|e| format!("currentApplication object: {e}"))?;
        if app.is_null() {
            return Err("ActivityThread.currentApplication returned null".to_string());
        }
        Ok(app)
    }

    fn ensure_class_loader(env: &jni::JNIEnv) -> VoiceResult<&'static GlobalRef> {
        if APP_CLASS_LOADER.get().is_none() {
            let context = get_app_context(env)?;
            let loader = env
                .call_method(context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
                .map_err(|e| {
                    clear_exception(env);
                    format!("getClassLoader: {e}")
                })?
                .l()
                .map_err(|e| format!("ClassLoader object: {e}"))?;
            let global = env
                .new_global_ref(loader)
                .map_err(|e| format!("ClassLoader global ref: {e}"))?;
            let _ = APP_CLASS_LOADER.set(global);
        }
        APP_CLASS_LOADER
            .get()
            .ok_or_else(|| "Application ClassLoader not initialized".to_string())
    }

    fn find_app_class<'a>(env: &'a jni::JNIEnv, dotted: &str) -> VoiceResult<JClass<'a>> {
        let loader = ensure_class_loader(env)?;
        let name = env
            .new_string(dotted)
            .map_err(|e| format!("voice class name: {e}"))?;
        let class = env
            .call_method(
                loader.as_obj(),
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(name.into())],
            )
            .map_err(|e| {
                clear_exception(env);
                format!("loadClass({dotted}): {e}")
            })?
            .l()
            .map_err(|e| format!("voice class object: {e}"))?;
        Ok(JClass::from(class))
    }

    fn clear_exception(env: &jni::JNIEnv) {
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use ratspeak_core::{NativeNotification, NativeNotificationKind, NativeNotifier};
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct RecordingNotifier {
        notifications: StdMutex<Vec<NativeNotification>>,
    }

    impl NativeNotifier for RecordingNotifier {
        fn notify(&self, notification: NativeNotification) {
            self.notifications.lock().unwrap().push(notification);
        }
    }

    fn make_notification_state(notifier: Arc<RecordingNotifier>) -> AppState {
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-voice-notification-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        db::init_schema(&pool).unwrap();
        AppState::new(config, pool, Arc::new(ratspeak_core::NoopEmitter), notifier)
    }

    #[test]
    fn incoming_call_native_notification_uses_contact_label_and_call_kind() {
        let notifier = Arc::new(RecordingNotifier::default());
        let state = make_notification_state(notifier.clone());
        state.is_foreground.store(false, Ordering::Relaxed);
        db::save_identity(&state.db, "identity-a", "lxmf-a", "Me", "Me");
        db::set_active_identity(&state.db, "identity-a").unwrap();

        let remote_identity = [0x42; 16];
        let remote_lxmf_destination = lxmf_destination_for_identity(remote_identity);
        db::save_contact(
            &state.db,
            &remote_lxmf_destination,
            Some("Caller Alice"),
            "trusted",
            "identity-a",
        );

        notify_incoming_call_if_background(&state, &remote_lxmf_destination, [0x77; 16]);

        let seen = notifier.notifications.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].kind, NativeNotificationKind::Call);
        assert_eq!(seen[0].title, "Incoming call from Caller Alice");
        assert_eq!(seen[0].body, "Tap to open Ratspeak");
        assert_eq!(
            seen[0].thread_id.as_deref(),
            Some("lxst:77777777777777777777777777777777")
        );
    }

    #[test]
    fn output_resampler_uses_decoded_frame_channel_count() {
        let frame = RawAudioFrame::new(1, vec![0.0; 640]).unwrap();

        let converted = resample_output_frame(&frame, 24_000, 48_000, 2);

        assert_eq!(converted.len(), 1_280 * 2);
    }

    #[test]
    fn output_resampler_handles_medium_quality_frame_shape() {
        let frame = RawAudioFrame::new(
            1,
            vec![0.0; Profile::QualityMedium.sample_frames_per_packet()],
        )
        .unwrap();

        let converted = resample_output_frame(&frame, 24_000, 48_000, 2);

        assert_eq!(converted.len(), 2_880 * 2);
    }

    #[test]
    fn profile_adaptation_prefers_high_quality_immediately() {
        let link_id = [0x42; 16];
        let mut adaptation = VoiceProfileAdaptation::new();

        assert_eq!(
            adaptation.next_profile(link_id, Profile::QualityMedium),
            Some(Profile::QualityHigh)
        );
        adaptation.mark_switch(Profile::QualityHigh);
        assert!(adaptation.pending_switch(link_id, Profile::QualityMedium));

        assert_eq!(
            adaptation.next_profile(link_id, Profile::QualityMedium),
            None
        );
        assert!(adaptation.pending_switch(link_id, Profile::QualityMedium));

        assert_eq!(adaptation.next_profile(link_id, Profile::QualityHigh), None);
        assert!(!adaptation.pending_switch(link_id, Profile::QualityHigh));
    }

    #[test]
    fn fade_in_ramps_once_without_changing_length() {
        let mut samples = vec![1.0; 4];
        let mut remaining = 4;

        apply_fade_in(&mut samples, &mut remaining, 4);

        assert_eq!(samples, vec![0.25, 0.5, 0.75, 1.0]);
        assert_eq!(remaining, 0);

        apply_fade_in(&mut samples, &mut remaining, 4);
        assert_eq!(samples, vec![0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn voice_output_leveling_lifts_quiet_samples_and_limits_peaks() {
        let mut samples = vec![0.0, 0.2, -0.2, 1.0, -1.0, f32::NAN];

        apply_voice_output_leveling(&mut samples);

        assert_eq!(samples[0], 0.0);
        assert!(samples[1] > 0.2);
        assert!(samples[2] < -0.2);
        assert!(samples[3] <= VOICE_OUTPUT_LIMIT);
        assert!(samples[4] >= -VOICE_OUTPUT_LIMIT);
        assert_eq!(samples[5], 0.0);
    }

    #[test]
    fn voice_input_processor_gates_low_level_room_noise() {
        let mut processor = VoiceInputProcessor::new(1, 48_000);
        let mut loudest = 0.0_f32;

        for _ in 0..8 {
            let mut samples = sine_samples(0.0025, 960, 64.0);
            processor.process(&mut samples);
            loudest = loudest.max(
                samples
                    .iter()
                    .map(|sample| sample.abs())
                    .fold(0.0, f32::max),
            );
        }

        assert!(loudest < 0.006, "noise gate allowed {loudest}");
        assert!(
            processor.agc_gain < 1.4,
            "AGC pumped noise to {}",
            processor.agc_gain
        );
    }

    #[test]
    fn voice_input_processor_preserves_close_speech_level() {
        let mut processor = VoiceInputProcessor::new(1, 48_000);
        let mut samples = sine_samples(0.08, 960, 64.0);

        processor.process(&mut samples);

        let rms = frame_rms(&samples);
        assert!(rms > 0.03, "speech RMS was gated too aggressively: {rms}");
    }

    #[test]
    fn voice_input_processor_keeps_sentence_tails_open() {
        let mut processor = VoiceInputProcessor::new(1, 48_000);

        for _ in 0..4 {
            let mut samples = sine_samples(0.08, 960, 64.0);
            processor.process(&mut samples);
        }

        let mut quiet_tail = sine_samples(0.01, 960, 64.0);
        processor.process(&mut quiet_tail);

        let rms = frame_rms(&quiet_tail);
        assert!(
            rms > 0.004,
            "sentence tail was gated too aggressively: {rms}"
        );
    }

    #[test]
    fn input_frame_builder_clear_pending_audio_drops_pre_mute_samples() {
        let mut builder = InputFrameBuilder::new(1, 48_000, 1, 48_000, 4);

        assert!(builder.push_interleaved(&[0.8, 0.8, 0.8]).is_empty());
        assert!(!builder.pending_frame.is_empty());

        builder.clear_pending_audio();
        assert!(builder.pending_frame.is_empty());
        assert!(builder.source_samples.is_empty());

        let frames = builder.push_interleaved(&[0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].samples.iter().all(|sample| sample.abs() < 0.0001));
    }

    fn sine_samples(amplitude: f32, len: usize, period_samples: f32) -> Vec<f32> {
        (0..len)
            .map(|index| {
                let phase = index as f32 * std::f32::consts::TAU / period_samples;
                phase.sin() * amplitude
            })
            .collect()
    }

    #[test]
    fn audio_output_queue_primes_with_silence_before_playback() {
        let mut queue = AudioOutputQueue::new(8, 3);
        queue.push_samples(vec![0.25, 0.5]);

        assert_eq!(queue.pop_sample(), 0.0);
        assert_eq!(queue.pop_sample(), 0.0);
        assert_eq!(queue.pop_sample(), 0.0);
        assert_eq!(queue.pop_sample(), 0.25);
        assert_eq!(queue.pop_sample(), 0.5);
        assert_eq!(queue.pop_sample(), 0.0);
    }

    #[test]
    fn audio_output_queue_caps_oldest_samples() {
        let mut queue = AudioOutputQueue::new(4, 0);
        queue.push_samples(vec![0.1, 0.2, 0.3]);
        queue.push_samples(vec![0.4, 0.5, 0.6]);

        assert_eq!(queue.pop_sample(), 0.3);
        assert_eq!(queue.pop_sample(), 0.4);
        assert_eq!(queue.pop_sample(), 0.5);
        assert_eq!(queue.pop_sample(), 0.6);
        assert_eq!(queue.pop_sample(), 0.0);
    }

    #[test]
    fn audio_output_queue_caps_single_large_batch() {
        let mut queue = AudioOutputQueue::new(3, 0);
        queue.push_samples(vec![0.1, 0.2, 0.3, 0.4, 0.5]);

        assert_eq!(queue.pop_sample(), 0.3);
        assert_eq!(queue.pop_sample(), 0.4);
        assert_eq!(queue.pop_sample(), 0.5);
        assert_eq!(queue.pop_sample(), 0.0);
    }
}
