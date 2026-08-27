//! Asynchronous voice memo capture and standard Ogg/Opus voice-message playback.
//!
//! LXST live calls carry raw codec frames inside a realtime session. A voice
//! memo has different lifecycle and storage requirements, so this module
//! reuses the trusted LXST Opus implementation and wraps its exact packets in
//! the interoperable LXMF `FIELD_AUDIO` Ogg/Opus representation.

mod ogg_opus;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::Stream;
use lxst_core::{OpusEncoderState, OpusMonoDecoder, Profile, RawAudioFrame};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::state::AppState;

pub const VOICE_MEMO_EXTENSION: &str = "opus";
pub const VOICE_MEMO_MIME: &str = "audio/ogg; codecs=opus";
pub const VOICE_MEMO_FILENAME: &str = "Voice message.opus";
pub const VOICE_MEMO_MAX_DURATION_MS: u32 = 5 * 60 * 1_000;
pub const VOICE_MEMO_MAX_AUDIO_BYTES: usize = 1_000_000;

const PROFILE: Profile = Profile::QualityMedium;
const FRAME_MS: u32 = 60;
const VOICE_MEMO_MIN_DURATION_MS: u32 = 1_000;
const MIN_FRAME_COUNT: usize = (VOICE_MEMO_MIN_DURATION_MS as usize).div_ceil(FRAME_MS as usize);
const MINIMUM_END_TRIM_48K: u64 =
    (MIN_FRAME_COUNT as u64 * FRAME_MS as u64 - VOICE_MEMO_MIN_DURATION_MS as u64) * 48;
const MAX_FRAME_COUNT: usize = (VOICE_MEMO_MAX_DURATION_MS / FRAME_MS) as usize;
const MAX_RECORDING_PACKET_BYTES: usize = 60;
pub const VOICE_MEMO_MAX_GENERATED_OGG_BYTES: usize = 313_550;
const RECORDING_STOP_DRAIN_TIMEOUT: Duration = Duration::from_millis(180);
const RECORDING_SESSION_PREFIX: &str = "vmr-";
const PLAYBACK_LEASE_PREFIX: &str = "vmp-";
#[cfg(target_os = "ios")]
pub(crate) const NATIVE_PLAYBACK_REFILL_TARGET_MS: u32 = 1_500;

pub type VoiceMemoResult<T> = Result<T, String>;

#[derive(Clone, Debug, Serialize)]
pub struct VoiceMemoStatus {
    pub state: String,
    pub duration_ms: u32,
    pub max_duration_ms: u32,
    pub session_id: Option<String>,
}

impl VoiceMemoStatus {
    fn idle() -> Self {
        Self {
            state: "idle".to_string(),
            duration_ms: 0,
            max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
            session_id: None,
        }
    }
}

#[derive(Debug)]
pub struct VoiceMemoDraft {
    pub data: Vec<u8>,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
}

#[derive(Debug)]
pub struct VoiceMemoPlayback {
    pub wav_data: Vec<u8>,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
    pub sample_rate_hz: u32,
    pub channels: u8,
}

#[cfg(any(target_os = "ios", target_os = "android"))]
#[derive(Debug, Serialize)]
pub struct VoiceMemoNativePlaybackStarted {
    pub lease_id: String,
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
    pub position_ms: u32,
}

#[derive(Debug, Serialize)]
pub struct VoiceMemoMetadata {
    pub duration_ms: u32,
    pub waveform: Vec<u8>,
}

enum RecorderCommand {
    SetPaused {
        paused: bool,
        reply: oneshot::Sender<VoiceMemoStatus>,
    },
    Stop {
        reply: oneshot::Sender<VoiceMemoResult<VoiceMemoDraft>>,
    },
    Cancel {
        reply: oneshot::Sender<()>,
    },
}

pub struct VoiceMemoRecordingHandle {
    session_id: u64,
    command_tx: mpsc::Sender<RecorderCommand>,
    status: Arc<Mutex<VoiceMemoStatus>>,
    task: Option<JoinHandle<()>>,
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub struct VoiceMemoPlaybackHandle {
    lease_id: u64,
    command_tx: mpsc::Sender<PlaybackCommand>,
    monitor: crate::voice::NativeVoiceMemoOutputMonitor,
    task: Option<JoinHandle<()>>,
}

#[cfg(any(target_os = "ios", target_os = "android"))]
impl VoiceMemoPlaybackHandle {
    fn matches(&self, lease_id: u64) -> bool {
        self.lease_id == lease_id
    }

    fn position_ms(&self) -> u32 {
        self.monitor.position_ms()
    }

    async fn stop(mut self) -> u32 {
        let fallback_position_ms = self.position_ms();
        let (reply_tx, reply_rx) = oneshot::channel();
        let position_ms = if self
            .command_tx
            .send(PlaybackCommand::Stop { reply: reply_tx })
            .await
            .is_ok()
        {
            tokio::time::timeout(std::time::Duration::from_secs(2), reply_rx)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or(fallback_position_ms)
        } else {
            fallback_position_ms
        };
        self.join().await;
        position_ms
    }

    async fn join(&mut self) {
        if let Some(mut task) = self.task.take() {
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                    // A started spawn_blocking task cannot be force-aborted.
                    // Dropping the join handle merely detaches an unexpectedly
                    // stuck platform worker without blocking app lifecycle.
                }
            }
        }
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
enum PlaybackCommand {
    Stop { reply: oneshot::Sender<u32> },
}

struct NativeVoiceMemoSource {
    decoder: OpusMonoDecoder,
    packets: std::vec::IntoIter<Vec<u8>>,
    discard_samples_48k: u64,
    remaining_samples_48k: u64,
    output_gain: f32,
    exhausted: bool,
}

impl NativeVoiceMemoSource {
    fn new(parsed: ogg_opus::ParsedOggOpus, position_ms: u32) -> VoiceMemoResult<Self> {
        let decoder = OpusMonoDecoder::new()
            .map_err(|error| format!("Could not initialize voice memo playback: {error}"))?;
        let position_samples_48k = u64::from(position_ms)
            .saturating_mul(48)
            .min(parsed.metadata.playable_samples_48k);
        let discard_samples_48k = u64::from(parsed.metadata.pre_skip_48k)
            .checked_add(position_samples_48k)
            .ok_or_else(|| "Voice message seek position overflows".to_string())?;
        let remaining_samples_48k = parsed.metadata.playable_samples_48k - position_samples_48k;
        let output_gain = 10.0f32.powf(f32::from(parsed.metadata.output_gain_q8) / (20.0 * 256.0));
        Ok(Self {
            decoder,
            packets: parsed.packets.into_iter(),
            discard_samples_48k,
            remaining_samples_48k,
            output_gain,
            exhausted: remaining_samples_48k == 0,
        })
    }

    fn next_decoded(&mut self) -> VoiceMemoResult<Option<RawAudioFrame>> {
        while !self.exhausted {
            let Some(packet) = self.packets.next() else {
                self.exhausted = true;
                if self.remaining_samples_48k != 0 {
                    return Err("Voice message ended before its final granule".to_string());
                }
                return Ok(None);
            };
            let frame = self
                .decoder
                .decode_packet(&packet)
                .map_err(|error| format!("Could not decode voice memo audio: {error}"))?;
            let frame_samples = frame.samples.len() as u64;
            if self.discard_samples_48k >= frame_samples {
                self.discard_samples_48k -= frame_samples;
                continue;
            }
            let start = usize::try_from(std::mem::take(&mut self.discard_samples_48k))
                .map_err(|_| "Voice message trim overflows".to_string())?;
            let available = frame.samples.len() - start;
            let take =
                available.min(usize::try_from(self.remaining_samples_48k).unwrap_or(usize::MAX));
            if take == 0 {
                self.exhausted = true;
                return Ok(None);
            }
            let samples = frame.samples[start..start + take]
                .iter()
                .map(|sample| (sample * self.output_gain).clamp(-1.0, 1.0))
                .collect::<Vec<_>>();
            self.remaining_samples_48k -= take as u64;
            if self.remaining_samples_48k == 0 {
                self.exhausted = true;
            }
            return RawAudioFrame::new(1, samples)
                .map(Some)
                .map_err(|error| format!("Could not prepare voice memo audio: {error}"));
        }
        Ok(None)
    }

    #[cfg(any(target_os = "ios", target_os = "android"))]
    fn refill(&mut self, output: &crate::voice::NativeVoiceMemoOutput) -> VoiceMemoResult<()> {
        while !self.exhausted && output.needs_refill() {
            let Some(frame) = self.next_decoded()? else {
                break;
            };
            output.enqueue_frame(&frame, 0)?;
        }
        if self.exhausted {
            output.finish_input()?;
        }
        Ok(())
    }
}

struct RecordingActor {
    state: Arc<AppState>,
    _platform_audio_session: crate::voice::PlatformVoiceAudioSession,
    stream: Stream,
    capture_rx: mpsc::Receiver<RawAudioFrame>,
    command_rx: mpsc::Receiver<RecorderCommand>,
    encoder: OpusEncoderState,
    status: Arc<Mutex<VoiceMemoStatus>>,
    session_id: u64,
}

impl VoiceMemoRecordingHandle {
    fn matches(&self, session_id: u64) -> bool {
        self.session_id == session_id
    }

    fn command_tx(&self) -> mpsc::Sender<RecorderCommand> {
        self.command_tx.clone()
    }

    fn status(&self) -> VoiceMemoStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    async fn join(mut self) {
        if let Some(mut task) = self.task.take() {
            tokio::select! {
                _ = &mut task => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => task.abort(),
            }
        }
    }
}

fn next_nonzero_generation(counter: &AtomicU64) -> u64 {
    loop {
        let generation = counter.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        if generation != 0 {
            return generation;
        }
    }
}

fn format_opaque_id(prefix: &str, id: u64) -> String {
    format!("{prefix}{id:016x}")
}

fn parse_opaque_id(value: &str, prefix: &str) -> Option<u64> {
    let encoded = value.strip_prefix(prefix)?;
    if encoded.len() != 16
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let id = u64::from_str_radix(encoded, 16).ok()?;
    (id != 0).then_some(id)
}

pub fn format_recording_session_id(id: u64) -> String {
    format_opaque_id(RECORDING_SESSION_PREFIX, id)
}

pub fn parse_recording_session_id(value: &str) -> Option<u64> {
    parse_opaque_id(value, RECORDING_SESSION_PREFIX)
}

pub fn format_playback_lease_id(id: u64) -> String {
    format_opaque_id(PLAYBACK_LEASE_PREFIX, id)
}

pub fn parse_playback_lease_id(value: &str) -> Option<u64> {
    parse_opaque_id(value, PLAYBACK_LEASE_PREFIX)
}

impl Drop for VoiceMemoRecordingHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn start_recording(state: &Arc<AppState>) -> VoiceMemoResult<VoiceMemoStatus> {
    if crate::voice::call_audio_reserved(state) {
        return Err("A voice call is using the microphone".to_string());
    }
    let _control = state.voice_memo_control_lock.lock().await;
    invalidate_playback_session_locked(state).await;
    if crate::voice::call_audio_reserved(state) {
        return Err("A voice call is using the microphone".to_string());
    }
    if state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?
        .is_some()
    {
        return Err("A voice memo is already being recorded".to_string());
    }
    let session_id = next_nonzero_generation(&state.voice_memo_recording_generation);
    let (command_tx, command_rx) = mpsc::channel(4);
    let status = Arc::new(Mutex::new(VoiceMemoStatus {
        state: "starting".to_string(),
        duration_ms: 0,
        max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
        session_id: Some(format_recording_session_id(session_id)),
    }));
    let task_state = Arc::clone(state);
    let task_status = Arc::clone(&status);
    let runtime = tokio::runtime::Handle::current();
    let (started_tx, started_rx) = oneshot::channel::<VoiceMemoResult<()>>();
    let task = tokio::task::spawn_blocking(move || {
        let encoder = match OpusEncoderState::new(PROFILE) {
            Ok(encoder) => encoder,
            Err(error) => {
                let _ = started_tx.send(Err(format!(
                    "Could not initialize the voice memo codec: {error}"
                )));
                return;
            }
        };
        let native_session_token = format_recording_session_id(session_id);
        let (platform_audio_session, stream, capture_rx) =
            match crate::voice::start_microphone_capture(PROFILE, &native_session_token) {
                Ok(capture) => capture,
                Err(error) => {
                    let _ = started_tx.send(Err(error));
                    return;
                }
            };
        let _ = update_status(&task_status, session_id, "recording", 0);
        let _ = started_tx.send(Ok(()));
        runtime.block_on(drive_recording(RecordingActor {
            state: task_state,
            _platform_audio_session: platform_audio_session,
            stream,
            capture_rx,
            command_rx,
            encoder,
            status: task_status,
            session_id,
        }));
    });

    let handle = VoiceMemoRecordingHandle {
        session_id,
        command_tx,
        status,
        task: Some(task),
    };
    {
        let mut slot = state
            .voice_memo_recording
            .lock()
            .map_err(|_| "Voice memo state is unavailable".to_string())?;
        *slot = Some(handle);
    }

    let start_result = started_rx
        .await
        .map_err(|_| "Voice memo recorder stopped while starting".to_string())?;
    if let Err(error) = start_result {
        let failed_handle = state
            .voice_memo_recording
            .lock()
            .ok()
            .and_then(|mut slot| slot.take());
        if let Some(handle) = failed_handle {
            handle.join().await;
        }
        return Err(error);
    }
    let response = recording_status(state);
    state.emit_to_all(
        "voice_memo_recording",
        serde_json::json!({
            "state": "recording",
            "duration_ms": 0,
            "level": 0,
            "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
            "session_id": format_recording_session_id(session_id),
        }),
    );
    Ok(response)
}

pub fn recording_status(state: &AppState) -> VoiceMemoStatus {
    state
        .voice_memo_recording
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(VoiceMemoRecordingHandle::status))
        .unwrap_or_else(VoiceMemoStatus::idle)
}

pub async fn set_paused(
    state: &AppState,
    session_id: u64,
    paused: bool,
) -> VoiceMemoResult<VoiceMemoStatus> {
    let _control = state.voice_memo_control_lock.lock().await;
    let command_tx = state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?
        .as_ref()
        .filter(|handle| handle.matches(session_id))
        .map(VoiceMemoRecordingHandle::command_tx)
        .ok_or_else(|| "No matching voice memo recording is active".to_string())?;
    let (reply_tx, reply_rx) = oneshot::channel();
    command_tx
        .send(RecorderCommand::SetPaused {
            paused,
            reply: reply_tx,
        })
        .await
        .map_err(|_| "Voice memo recorder stopped unexpectedly".to_string())?;
    reply_rx
        .await
        .map_err(|_| "Voice memo recorder stopped unexpectedly".to_string())
}

pub async fn stop_recording(state: &AppState, session_id: u64) -> VoiceMemoResult<VoiceMemoDraft> {
    let _control = state.voice_memo_control_lock.lock().await;
    let handle = take_matching_recording(state, session_id)?;
    let command_tx = handle.command_tx();
    let (reply_tx, reply_rx) = oneshot::channel();
    command_tx
        .send(RecorderCommand::Stop { reply: reply_tx })
        .await
        .map_err(|_| "Voice memo recorder stopped unexpectedly".to_string())?;
    let result = reply_rx
        .await
        .map_err(|_| "Voice memo recorder stopped unexpectedly".to_string())?;
    handle.join().await;
    result
}

pub async fn cancel_recording(state: &AppState) -> VoiceMemoResult<()> {
    let _control = state.voice_memo_control_lock.lock().await;
    invalidate_playback_session_locked(state).await;
    let handle = state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?
        .take();
    let Some(handle) = handle else {
        return Ok(());
    };
    let command_tx = handle.command_tx();
    let (reply_tx, reply_rx) = oneshot::channel();
    if command_tx
        .send(RecorderCommand::Cancel { reply: reply_tx })
        .await
        .is_ok()
    {
        let _ = reply_rx.await;
    }
    handle.join().await;
    Ok(())
}

pub async fn cancel_recording_session(state: &AppState, session_id: u64) -> VoiceMemoResult<()> {
    let _control = state.voice_memo_control_lock.lock().await;
    let handle = take_matching_recording(state, session_id)?;
    cancel_handle(handle).await;
    Ok(())
}

fn take_matching_recording(
    state: &AppState,
    session_id: u64,
) -> VoiceMemoResult<VoiceMemoRecordingHandle> {
    let mut slot = state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?;
    if !slot
        .as_ref()
        .is_some_and(|handle| handle.matches(session_id))
    {
        return Err("No matching voice memo recording is active".to_string());
    }
    slot.take()
        .ok_or_else(|| "No matching voice memo recording is active".to_string())
}

async fn cancel_handle(handle: VoiceMemoRecordingHandle) {
    let command_tx = handle.command_tx();
    let (reply_tx, reply_rx) = oneshot::channel();
    if command_tx
        .send(RecorderCommand::Cancel { reply: reply_tx })
        .await
        .is_ok()
    {
        let _ = reply_rx.await;
    }
    handle.join().await;
}

async fn invalidate_playback_session_locked(state: &AppState) {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let playback = state
            .voice_memo_playback
            .lock()
            .ok()
            .and_then(|mut playback| playback.take());
        if let Some(playback) = playback {
            playback.stop().await;
        }
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    let _ = state;
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub async fn start_native_playback(
    state: &Arc<AppState>,
    data: Vec<u8>,
    requested_position_ms: u32,
) -> VoiceMemoResult<VoiceMemoNativePlaybackStarted> {
    let parsed = {
        let _decode = state.voice_memo_decode_lock.lock().await;
        tokio::task::spawn_blocking(move || ogg_opus::parse_ogg_opus(&data))
            .await
            .map_err(|_| "Voice message decoder task panicked".to_string())??
    };

    let _control = state.voice_memo_control_lock.lock().await;
    if crate::voice::call_audio_reserved(state) {
        return Err("A voice call is using audio".to_string());
    }
    if recording_status(state).state != "idle" {
        return Err("A voice message is being recorded".to_string());
    }
    invalidate_playback_session_locked(state).await;

    let lease_id = next_nonzero_generation(&state.voice_memo_playback_generation);
    let duration_ms = parsed.metadata.duration_ms;
    let position_ms = if requested_position_ms >= duration_ms {
        0
    } else {
        requested_position_ms
    };
    let waveform = Vec::new();
    let (command_tx, command_rx) = mpsc::channel(1);
    let (started_tx, started_rx) = oneshot::channel();
    let (committed_tx, committed_rx) = oneshot::channel();
    let runtime = tokio::runtime::Handle::current();
    let worker_state = Arc::clone(state);
    let task = tokio::task::spawn_blocking(move || {
        let mut source = match NativeVoiceMemoSource::new(parsed, position_ms) {
            Ok(source) => source,
            Err(error) => {
                let _ = started_tx.send(Err(error));
                return;
            }
        };
        let platform_audio_session =
            match crate::voice::start_platform_voice_memo_playback_session(lease_id) {
                Ok(session) => session,
                Err(error) => {
                    let _ = started_tx.send(Err(error));
                    return;
                }
            };
        let output = match crate::voice::start_voice_memo_output(48_000, position_ms, duration_ms) {
            Ok(output) => output,
            Err(error) => {
                let _ = started_tx.send(Err(error));
                return;
            }
        };
        if let Err(error) = source.refill(&output).and_then(|()| output.play()) {
            let _ = started_tx.send(Err(error));
            return;
        }
        let monitor = output.monitor();
        if started_tx.send(Ok(monitor)).is_err() {
            return;
        }
        if runtime.block_on(committed_rx).is_err() {
            return;
        }
        runtime.block_on(drive_native_playback(
            worker_state,
            lease_id,
            position_ms,
            duration_ms,
            output,
            platform_audio_session,
            source,
            command_rx,
        ));
    });
    let monitor = match started_rx.await {
        Ok(Ok(monitor)) => monitor,
        Ok(Err(error)) => {
            let mut task = task;
            let _ = (&mut task).await;
            return Err(error);
        }
        Err(_) => {
            let mut task = task;
            let _ = (&mut task).await;
            return Err("Voice message output stopped while starting".to_string());
        }
    };

    {
        let mut slot = state
            .voice_memo_playback
            .lock()
            .map_err(|_| "Voice message playback state is unavailable".to_string())?;
        *slot = Some(VoiceMemoPlaybackHandle {
            lease_id,
            command_tx,
            monitor,
            task: Some(task),
        });
    }
    if committed_tx.send(()).is_err() {
        let mut failed = state.voice_memo_playback.lock().ok().and_then(|mut slot| {
            if slot.as_ref().is_some_and(|handle| handle.matches(lease_id)) {
                slot.take()
            } else {
                None
            }
        });
        if let Some(handle) = failed.as_mut() {
            handle.join().await;
        }
        return Err("Voice message output stopped while starting".to_string());
    }

    Ok(VoiceMemoNativePlaybackStarted {
        lease_id: format_playback_lease_id(lease_id),
        duration_ms,
        waveform,
        position_ms,
    })
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub async fn stop_native_playback(state: &AppState, lease_id: u64) -> VoiceMemoResult<Option<u32>> {
    let _control = state.voice_memo_control_lock.lock().await;
    let handle = {
        let mut slot = state
            .voice_memo_playback
            .lock()
            .map_err(|_| "Voice message playback state is unavailable".to_string())?;
        if !slot.as_ref().is_some_and(|handle| handle.matches(lease_id)) {
            return Ok(None);
        }
        slot.take()
    };
    match handle {
        Some(handle) => Ok(Some(handle.stop().await)),
        None => Ok(None),
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
async fn drive_native_playback(
    state: Arc<AppState>,
    lease_id: u64,
    start_position_ms: u32,
    duration_ms: u32,
    output: crate::voice::NativeVoiceMemoOutput,
    platform_audio_session: crate::voice::PlatformVoiceMemoPlaybackSession,
    mut source: NativeVoiceMemoSource,
    mut command_rx: mpsc::Receiver<PlaybackCommand>,
) {
    let mut last_position_ms = start_position_ms;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
    loop {
        tokio::select! {
            biased;
            command = command_rx.recv() => {
                match command {
                    Some(PlaybackCommand::Stop { reply }) => {
                        let _ = reply.send(output.position_ms());
                    }
                    None => {}
                }
                break;
            }
            _ = interval.tick() => {
                if !output.healthy() {
                    tracing::warn!(reason = "native_output_interrupted", "voice message playback failed");
                    state.emit_to_all(
                        "voice_memo_playback",
                        serde_json::json!({
                            "lease_id": format_playback_lease_id(lease_id),
                            "state": "error",
                            "position_ms": output.position_ms(),
                            "duration_ms": duration_ms,
                        }),
                    );
                    break;
                }
                if source.refill(&output).is_err() {
                    tracing::warn!(reason = "native_decode_failed", "voice message playback failed");
                    state.emit_to_all(
                        "voice_memo_playback",
                        serde_json::json!({
                            "lease_id": format_playback_lease_id(lease_id),
                            "state": "error",
                            "position_ms": output.position_ms(),
                            "duration_ms": duration_ms,
                        }),
                    );
                    break;
                }
                let current = output.position_ms();
                if current > last_position_ms {
                    last_position_ms = current;
                    state.emit_to_all(
                        "voice_memo_playback",
                        serde_json::json!({
                            "lease_id": format_playback_lease_id(lease_id),
                            "state": "playing",
                            "position_ms": current,
                            "duration_ms": duration_ms,
                        }),
                    );
                }
                if output.finished() {
                    state.emit_to_all(
                        "voice_memo_playback",
                        serde_json::json!({
                            "lease_id": format_playback_lease_id(lease_id),
                            "state": "ended",
                            "position_ms": duration_ms,
                            "duration_ms": duration_ms,
                        }),
                    );
                    break;
                }
            }
        }
    }

    // Native output must be destroyed before its exact platform audio lease is
    // released. Keeping both objects local also preserves CPAL's iOS !Send
    // contract and Android AudioTrack ownership outside shared app state.
    drop(output);
    drop(platform_audio_session);
    if let Ok(mut slot) = state.voice_memo_playback.lock() {
        if slot.as_ref().is_some_and(|handle| handle.matches(lease_id)) {
            slot.take();
        }
    }
}

async fn drive_recording(actor: RecordingActor) {
    let RecordingActor {
        state,
        _platform_audio_session,
        stream,
        mut capture_rx,
        mut command_rx,
        mut encoder,
        status,
        session_id,
    } = actor;
    let mut stream = Some(stream);
    let mut capture_open = true;
    let mut paused = false;
    let mut frames = Vec::<Vec<u8>>::with_capacity(MAX_FRAME_COUNT);
    let mut waveform = Vec::<u8>::with_capacity(MAX_FRAME_COUNT);

    loop {
        tokio::select! {
            biased;
            command = command_rx.recv() => {
                let Some(command) = command else { break; };
                match command {
                    RecorderCommand::SetPaused { paused: next_paused, reply } => {
                        paused = next_paused;
                        let next_state = if paused { "paused" } else { "recording" };
                        let snapshot = update_status(&status, session_id, next_state, frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": next_state,
                                "duration_ms": snapshot.duration_ms,
                                "level": 0,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                                "session_id": format_recording_session_id(session_id),
                            }),
                        );
                        let _ = reply.send(snapshot);
                    }
                    RecorderCommand::Stop { reply } => {
                        let should_drain = should_drain_capture_on_stop(capture_open, paused);
                        let drain_result = if should_drain {
                            drain_capture_before_stream_stop(
                                &mut capture_rx,
                                &mut encoder,
                                &mut frames,
                                &mut waveform,
                            )
                            .await
                        } else {
                            Ok(())
                        };
                        // The callback-owned sender must remain alive until the
                        // bounded final receive above completes. Dropping the
                        // stream first made this only an empty-channel check.
                        stream.take();
                        if let Err(error) = drain_result {
                            let _ = reply.send(Err(error));
                            return;
                        }
                        if should_drain {
                            if let Err(error) = drain_ready_capture(
                                &mut capture_rx,
                                &mut encoder,
                                &mut frames,
                                &mut waveform,
                            ) {
                                let _ = reply.send(Err(error));
                                return;
                            }
                        }
                        let result = pad_recording_to_minimum_duration(
                            &mut encoder,
                            &mut frames,
                            &mut waveform,
                        )
                        .and_then(|end_trim_48k| {
                            finish_draft_with_end_trim(frames, waveform, end_trim_48k)
                        });
                        let _ = reply.send(result);
                        break;
                    }
                    RecorderCommand::Cancel { reply } => {
                        stream.take();
                        let _ = reply.send(());
                        break;
                    }
                }
            }
            captured = capture_rx.recv(), if capture_open => {
                let Some(frame) = captured else {
                    capture_open = false;
                    continue;
                };
                if paused { continue; }
                match encode_captured_frame(&mut encoder, frame, &mut frames, &mut waveform) {
                    Ok(level) => {
                        let snapshot = update_status(&status, session_id, "recording", frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": "recording",
                                "duration_ms": snapshot.duration_ms,
                                "level": level,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                                "session_id": format_recording_session_id(session_id),
                            }),
                        );
                        if frames.len() >= MAX_FRAME_COUNT {
                            stream.take();
                            capture_open = false;
                            let snapshot = update_status(&status, session_id, "limit", frames.len());
                            state.emit_to_all(
                                "voice_memo_recording",
                                serde_json::json!({
                                    "state": "limit",
                                    "duration_ms": snapshot.duration_ms,
                                    "level": 0,
                                    "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                                    "session_id": format_recording_session_id(session_id),
                                }),
                            );
                        }
                    }
                    Err(message) => {
                        stream.take();
                        capture_open = false;
                        let snapshot = update_status(&status, session_id, "error", frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": "error",
                                "duration_ms": snapshot.duration_ms,
                                "level": 0,
                                "message": message,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                                "session_id": format_recording_session_id(session_id),
                            }),
                        );
                    }
                }
            }
        }
    }

    stream.take();
    let _ = update_status(&status, session_id, "idle", 0);
    state.emit_to_all(
        "voice_memo_recording",
        serde_json::json!({
            "state": "idle",
            "duration_ms": 0,
            "level": 0,
            "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
            "session_id": format_recording_session_id(session_id),
        }),
    );
}

fn update_status(
    status: &Arc<Mutex<VoiceMemoStatus>>,
    session_id: u64,
    state: &str,
    frame_count: usize,
) -> VoiceMemoStatus {
    let snapshot = VoiceMemoStatus {
        state: state.to_string(),
        duration_ms: duration_for_frames(frame_count),
        max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
        session_id: Some(format_recording_session_id(session_id)),
    };
    match status.lock() {
        Ok(mut current) => *current = snapshot.clone(),
        Err(poisoned) => *poisoned.into_inner() = snapshot.clone(),
    }
    snapshot
}

fn should_drain_capture_on_stop(capture_open: bool, paused: bool) -> bool {
    capture_open && !paused
}

fn encode_captured_frame(
    encoder: &mut OpusEncoderState,
    frame: RawAudioFrame,
    frames: &mut Vec<Vec<u8>>,
    waveform: &mut Vec<u8>,
) -> VoiceMemoResult<u8> {
    let peak = frame
        .samples
        .iter()
        .fold(0.0f32, |current, sample| current.max(sample.abs()))
        .clamp(0.0, 1.0);
    let level = (peak.sqrt() * 255.0).round() as u8;
    let encoded = encoder
        .encode_frame(&frame)
        .map_err(|error| format!("Could not encode voice memo audio: {error}"))?;
    if encoded.payload.is_empty() || encoded.payload.len() > MAX_RECORDING_PACKET_BYTES {
        return Err("Voice memo encoder produced an invalid frame".to_string());
    }
    frames.push(encoded.payload);
    waveform.push(level);
    Ok(level)
}

async fn drain_capture_before_stream_stop(
    capture_rx: &mut mpsc::Receiver<RawAudioFrame>,
    encoder: &mut OpusEncoderState,
    frames: &mut Vec<Vec<u8>>,
    waveform: &mut Vec<u8>,
) -> VoiceMemoResult<()> {
    drain_ready_capture(capture_rx, encoder, frames, waveform)?;
    if frames.len() >= MAX_FRAME_COUNT {
        return Ok(());
    }

    // Rescue at most one pending callback edge while the stream still owns
    // its sender. This avoids both the old zero-frame teardown race and a
    // fixed 180 ms recording tail after the user pressed Stop.
    let deadline = tokio::time::Instant::now() + RECORDING_STOP_DRAIN_TIMEOUT;
    if let Ok(Some(frame)) = tokio::time::timeout_at(deadline, capture_rx.recv()).await {
        encode_captured_frame(encoder, frame, frames, waveform)?;
    }
    drain_ready_capture(capture_rx, encoder, frames, waveform)
}

fn drain_ready_capture(
    capture_rx: &mut mpsc::Receiver<RawAudioFrame>,
    encoder: &mut OpusEncoderState,
    frames: &mut Vec<Vec<u8>>,
    waveform: &mut Vec<u8>,
) -> VoiceMemoResult<()> {
    while frames.len() < MAX_FRAME_COUNT {
        let Ok(frame) = capture_rx.try_recv() else {
            break;
        };
        encode_captured_frame(encoder, frame, frames, waveform)?;
    }
    Ok(())
}

fn pad_recording_to_minimum_duration(
    encoder: &mut OpusEncoderState,
    frames: &mut Vec<Vec<u8>>,
    waveform: &mut Vec<u8>,
) -> VoiceMemoResult<u64> {
    if frames.len() >= MIN_FRAME_COUNT {
        return Ok(0);
    }
    while frames.len() < MIN_FRAME_COUNT {
        let silence = RawAudioFrame::new(
            PROFILE.channels(),
            vec![0.0; PROFILE.sample_frames_per_packet() * usize::from(PROFILE.channels())],
        )
        .map_err(|error| format!("Could not prepare voice memo silence: {error}"))?;
        encode_captured_frame(encoder, silence, frames, waveform)?;
    }
    Ok(MINIMUM_END_TRIM_48K)
}

fn finish_draft_with_end_trim(
    frames: Vec<Vec<u8>>,
    waveform: Vec<u8>,
    end_trim_48k: u64,
) -> VoiceMemoResult<VoiceMemoDraft> {
    if frames.is_empty() {
        return Err("Voice memo contains no encoded audio".to_string());
    }
    if frames.len() != waveform.len() || frames.len() > MAX_FRAME_COUNT {
        return Err("Voice memo frame metadata is invalid".to_string());
    }
    let serial = u32::from_le_bytes(
        Uuid::new_v4().as_bytes()[..4]
            .try_into()
            .expect("UUID serial slice"),
    );
    let data = ogg_opus::mux_opus_packets_with_timing(&frames, serial, 0, end_trim_48k, 0, 0)?;
    let duration_ms = if end_trim_48k == 0 {
        duration_for_frames(frames.len())
    } else {
        VOICE_MEMO_MIN_DURATION_MS
    };
    Ok(VoiceMemoDraft {
        data,
        duration_ms,
        waveform,
    })
}

#[cfg(test)]
fn finish_draft(frames: Vec<Vec<u8>>, waveform: Vec<u8>) -> VoiceMemoResult<VoiceMemoDraft> {
    finish_draft_with_end_trim(frames, waveform, 0)
}

fn duration_for_frames(frame_count: usize) -> u32 {
    (frame_count as u32).saturating_mul(FRAME_MS)
}

pub fn decode_voice_memo(data: &[u8]) -> VoiceMemoResult<VoiceMemoPlayback> {
    let parsed = ogg_opus::parse_ogg_opus(data)?;
    let duration_ms = parsed.metadata.duration_ms;
    let capacity = usize::try_from(parsed.metadata.playable_samples_48k)
        .map_err(|_| "Decoded voice memo is too large".to_string())?;
    let mut source = NativeVoiceMemoSource::new(parsed, 0)?;
    let mut wav_data = begin_pcm16_wav(capacity, 48_000, 1)?;
    let mut waveform = Vec::with_capacity(duration_ms.div_ceil(FRAME_MS) as usize);
    let mut bucket_peak = 0.0f32;
    let mut bucket_samples = 0usize;
    while let Some(frame) = source.next_decoded()? {
        for sample in frame.samples {
            bucket_peak = bucket_peak.max(sample.abs());
            bucket_samples += 1;
            let pcm = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            wav_data.extend_from_slice(&pcm.to_le_bytes());
            if bucket_samples == 2_880 {
                waveform.push((bucket_peak.sqrt() * 255.0).round() as u8);
                bucket_peak = 0.0;
                bucket_samples = 0;
            }
        }
    }
    if bucket_samples != 0 {
        waveform.push((bucket_peak.sqrt() * 255.0).round() as u8);
    }
    let expected_wav_len = capacity
        .checked_mul(2)
        .and_then(|bytes| 44usize.checked_add(bytes))
        .ok_or_else(|| "Decoded voice memo is too large".to_string())?;
    if wav_data.len() != expected_wav_len {
        return Err("Decoded voice memo sample count does not match its granules".to_string());
    }
    Ok(VoiceMemoPlayback {
        wav_data,
        duration_ms,
        waveform,
        sample_rate_hz: 48_000,
        channels: 1,
    })
}

pub fn inspect_voice_memo(data: &[u8]) -> VoiceMemoResult<VoiceMemoMetadata> {
    let parsed = ogg_opus::parse_ogg_opus(data)?;
    Ok(VoiceMemoMetadata {
        duration_ms: parsed.metadata.duration_ms,
        waveform: Vec::new(),
    })
}

fn begin_pcm16_wav(
    sample_count: usize,
    sample_rate_hz: u32,
    channels: u8,
) -> VoiceMemoResult<Vec<u8>> {
    let data_len = sample_count
        .checked_mul(2)
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| "Decoded voice memo is too large".to_string())?;
    let riff_len = 36u32
        .checked_add(data_len)
        .ok_or_else(|| "Decoded voice memo is too large".to_string())?;
    let byte_rate = sample_rate_hz
        .checked_mul(u32::from(channels))
        .and_then(|rate| rate.checked_mul(2))
        .ok_or_else(|| "Voice memo WAV metadata is invalid".to_string())?;
    let block_align = u16::from(channels) * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&u16::from(channels).to_le_bytes());
    wav.extend_from_slice(&sample_rate_hz.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxst_core::{AudioCodec, OpusProfile};

    fn synthetic_frames(count: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
        let mut encoder = OpusEncoderState::new(PROFILE).unwrap();
        let frames = (0..count)
            .map(|index| {
                let samples = (0..PROFILE.sample_frames_per_packet())
                    .map(|sample| {
                        let phase = ((index * PROFILE.sample_frames_per_packet() + sample) as f32)
                            * 440.0
                            * std::f32::consts::TAU
                            / PROFILE.sample_rate_hz() as f32;
                        phase.sin() * 0.2
                    })
                    .collect::<Vec<_>>();
                let raw = RawAudioFrame::new(PROFILE.channels(), samples).unwrap();
                encoder.encode_frame(&raw).unwrap().payload
            })
            .collect::<Vec<_>>();
        let waveform = (0..count).map(|index| (index % 255) as u8).collect();
        (frames, waveform)
    }

    fn recording_contract_packets() -> Vec<Vec<u8>> {
        let mut encoder = OpusEncoderState::new(PROFILE).unwrap();
        (0..4)
            .map(|packet_index| {
                let samples = (0..PROFILE.sample_frames_per_packet())
                    .map(|sample| match ((packet_index * 17 + sample) / 120) % 8 {
                        0 => 0.0,
                        1 => 0.125,
                        2 => 0.25,
                        3 => 0.125,
                        4 => 0.0,
                        5 => -0.125,
                        6 => -0.25,
                        _ => -0.125,
                    })
                    .collect::<Vec<_>>();
                encoder
                    .encode_frame(&RawAudioFrame::new(1, samples).unwrap())
                    .unwrap()
                    .payload
            })
            .collect()
    }

    #[test]
    fn opaque_session_ids_are_canonical_and_domain_separated() {
        let recording = format_recording_session_id(0x12ab);
        let playback = format_playback_lease_id(0x12ab);

        assert_eq!(recording, "vmr-00000000000012ab");
        assert_eq!(playback, "vmp-00000000000012ab");
        assert_eq!(parse_recording_session_id(&recording), Some(0x12ab));
        assert_eq!(parse_playback_lease_id(&playback), Some(0x12ab));
        assert_eq!(parse_recording_session_id(&playback), None);
        assert_eq!(parse_playback_lease_id(&recording), None);
        assert_eq!(parse_recording_session_id("vmr-00000000000012AB"), None);
        assert_eq!(parse_recording_session_id("vmr-0000000000000000"), None);
    }

    #[test]
    fn opaque_generation_skips_zero_even_after_wraparound() {
        let counter = AtomicU64::new(0);
        assert_eq!(next_nonzero_generation(&counter), 1);
        assert_eq!(next_nonzero_generation(&counter), 2);

        let wrapping = AtomicU64::new(u64::MAX);
        assert_eq!(next_nonzero_generation(&wrapping), 1);
    }

    #[test]
    fn recording_profile_and_packet_fixture_are_frozen_before_ogg_wrapping() {
        assert_eq!(PROFILE, Profile::QualityMedium);
        assert_eq!(PROFILE.sample_rate_hz(), 24_000);
        assert_eq!(PROFILE.channels(), 1);
        assert_eq!(PROFILE.frame_time_ms(), 60);
        assert_eq!(PROFILE.sample_frames_per_packet(), 1_440);
        assert_eq!(PROFILE.opus_payload_ceiling_bytes(), Some(60));
        assert_eq!(
            PROFILE.audio_codec(),
            AudioCodec::Opus(OpusProfile::VoiceMedium)
        );
        assert_eq!(OpusProfile::VoiceMedium.bitrate_ceiling(), 8_000);

        let packets = recording_contract_packets();
        let lengths = packets.iter().map(Vec::len).collect::<Vec<_>>();
        let mut length_prefixed = Vec::new();
        for packet in packets {
            length_prefixed.extend_from_slice(&(packet.len() as u16).to_be_bytes());
            length_prefixed.extend_from_slice(&packet);
        }
        let digest = hex::encode(rns_crypto::sha::sha256(&length_prefixed));

        assert_eq!(lengths, vec![53, 56, 56, 54]);
        assert!(lengths.windows(2).any(|window| window[0] != window[1]));
        assert_eq!(
            digest,
            "09a4f9cbc808eee88cd3619aaae3bf478f784a8799dfc81c8256c8886091be8e"
        );
    }

    #[test]
    fn ogg_round_trip_retains_lxst_packets_byte_for_byte() {
        let (frames, waveform) = synthetic_frames(8);
        let hashes = frames
            .iter()
            .map(|packet| rns_crypto::sha::sha256(packet))
            .collect::<Vec<_>>();
        let encoded = ogg_opus::mux_opus_packets(&frames, 42).unwrap();
        let parsed = ogg_opus::parse_ogg_opus(&encoded).unwrap();

        assert_eq!(parsed.metadata.duration_ms, 8 * FRAME_MS);
        assert_eq!(parsed.packets, frames);
        assert_eq!(
            parsed
                .packets
                .iter()
                .map(|packet| rns_crypto::sha::sha256(packet))
                .collect::<Vec<_>>(),
            hashes
        );
        assert_eq!(waveform.len(), 8);
    }

    #[test]
    fn incremental_native_source_primes_opus_state_and_keeps_exact_seek_offset() {
        let (frames, _) = synthetic_frames(8);
        let encoded = ogg_opus::mux_opus_packets(&frames, 43).unwrap();
        let parsed = ogg_opus::parse_ogg_opus(&encoded).unwrap();
        let mut source = NativeVoiceMemoSource::new(parsed, 150).unwrap();

        let first = source.next_decoded().unwrap().unwrap();
        assert_eq!(first.channels, 1);
        assert_eq!(first.sample_frames(), 1_440);

        let mut remaining = 0;
        while source.next_decoded().unwrap().is_some() {
            remaining += 1;
        }
        assert_eq!(remaining, 5);
        assert!(source.exhausted);
    }

    #[test]
    fn native_source_applies_sample_exact_pre_skip_seek_and_end_trim() {
        let (frames, _) = synthetic_frames(3);
        let encoded = ogg_opus::mux_opus_packets_with_timing(&frames, 46, 312, 480, 0, 0).unwrap();
        let parsed = ogg_opus::parse_ogg_opus(&encoded).unwrap();
        let mut source = NativeVoiceMemoSource::new(parsed, 17).unwrap();
        let mut samples = 0usize;
        while let Some(frame) = source.next_decoded().unwrap() {
            samples += frame.sample_frames();
        }

        assert_eq!(samples, 7_032);
        assert!(source.exhausted);
    }

    #[test]
    fn native_source_applies_positive_and_negative_opus_header_gain() {
        fn decoded_peak(data: &[u8]) -> f32 {
            let parsed = ogg_opus::parse_ogg_opus(data).unwrap();
            let mut source = NativeVoiceMemoSource::new(parsed, 0).unwrap();
            let mut peak = 0.0f32;
            while let Some(frame) = source.next_decoded().unwrap() {
                peak = frame
                    .samples
                    .iter()
                    .fold(peak, |current, sample| current.max(sample.abs()));
            }
            peak
        }

        let packets = recording_contract_packets();
        let neutral = ogg_opus::mux_opus_packets_with_timing(&packets, 47, 0, 0, 0, 0).unwrap();
        let positive =
            ogg_opus::mux_opus_packets_with_timing(&packets, 48, 0, 0, 0, 6 * 256).unwrap();
        let negative =
            ogg_opus::mux_opus_packets_with_timing(&packets, 49, 0, 0, 0, -6 * 256).unwrap();
        let neutral_peak = decoded_peak(&neutral);
        let positive_ratio = decoded_peak(&positive) / neutral_peak;
        let negative_ratio = decoded_peak(&negative) / neutral_peak;

        assert!((1.95..=2.05).contains(&positive_ratio));
        assert!((0.49..=0.51).contains(&negative_ratio));
    }

    #[test]
    fn ogg_round_trip_decodes_to_bounded_wav() {
        let (frames, _) = synthetic_frames(8);
        let encoded = ogg_opus::mux_opus_packets(&frames, 44).unwrap();
        let playback = decode_voice_memo(&encoded).unwrap();
        assert_eq!(playback.duration_ms, 8 * FRAME_MS);
        assert_eq!(playback.waveform.len(), 8);
        assert_eq!(playback.sample_rate_hz, 48_000);
        assert_eq!(playback.channels, 1);
        assert_eq!(&playback.wav_data[..4], b"RIFF");
        assert_eq!(&playback.wav_data[8..12], b"WAVE");
        let peak = playback.wav_data[44..]
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]).unsigned_abs())
            .max()
            .unwrap_or_default();
        assert!(peak > 100, "decoded memo audio must retain audible PCM");
    }

    #[test]
    fn parser_rejects_truncation_and_trailing_bytes() {
        let (frames, _) = synthetic_frames(4);
        let encoded = ogg_opus::mux_opus_packets(&frames, 45).unwrap();
        assert!(inspect_voice_memo(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(inspect_voice_memo(&trailing).is_err());
    }

    #[test]
    fn encoder_keeps_max_memo_under_reticulum_efficient_resource_limit() {
        const {
            assert!(
                VOICE_MEMO_MAX_GENERATED_OGG_BYTES < rns_protocol::resource::MAX_EFFICIENT_SIZE
            );
        }
    }

    #[test]
    fn raw_empty_draft_is_invalid_but_one_frame_is_valid() {
        assert!(finish_draft(Vec::new(), Vec::new()).is_err());

        let (frames, waveform) = synthetic_frames(1);
        let draft = finish_draft(frames, waveform).unwrap();
        assert_eq!(draft.duration_ms, FRAME_MS);
        let parsed = ogg_opus::parse_ogg_opus(&draft.data).unwrap();
        assert_eq!(parsed.metadata.duration_ms, FRAME_MS);
        assert_eq!(parsed.packets.len(), 1);
    }

    #[test]
    fn stopped_recordings_pad_zero_or_short_capture_to_exactly_one_second() {
        for captured_count in [0, 1, 16] {
            let mut encoder = OpusEncoderState::new(PROFILE).unwrap();
            let (mut frames, mut waveform) = synthetic_frames(captured_count);
            let captured_packets = frames.clone();
            let end_trim =
                pad_recording_to_minimum_duration(&mut encoder, &mut frames, &mut waveform)
                    .unwrap();
            let draft = finish_draft_with_end_trim(frames, waveform, end_trim).unwrap();
            let parsed = ogg_opus::parse_ogg_opus(&draft.data).unwrap();

            assert_eq!(draft.duration_ms, VOICE_MEMO_MIN_DURATION_MS);
            assert_eq!(parsed.metadata.duration_ms, VOICE_MEMO_MIN_DURATION_MS);
            assert_eq!(parsed.metadata.end_trim_48k, MINIMUM_END_TRIM_48K);
            assert_eq!(parsed.packets.len(), MIN_FRAME_COUNT);
            assert_eq!(&parsed.packets[..captured_count], captured_packets);
        }
    }

    #[test]
    fn recording_at_or_above_minimum_keeps_every_real_packet_untrimmed() {
        let mut encoder = OpusEncoderState::new(PROFILE).unwrap();
        let (mut frames, mut waveform) = synthetic_frames(MIN_FRAME_COUNT);
        let captured_packets = frames.clone();
        let end_trim =
            pad_recording_to_minimum_duration(&mut encoder, &mut frames, &mut waveform).unwrap();
        let draft = finish_draft_with_end_trim(frames, waveform, end_trim).unwrap();
        let parsed = ogg_opus::parse_ogg_opus(&draft.data).unwrap();

        assert_eq!(end_trim, 0);
        assert_eq!(draft.duration_ms, MIN_FRAME_COUNT as u32 * FRAME_MS);
        assert_eq!(parsed.metadata.end_trim_48k, 0);
        assert_eq!(parsed.packets, captured_packets);
    }

    #[test]
    fn stopped_capture_drains_only_an_open_unpaused_microphone_generation() {
        assert!(should_drain_capture_on_stop(true, false));
        assert!(!should_drain_capture_on_stop(true, true));
        assert!(!should_drain_capture_on_stop(false, false));
        assert!(!should_drain_capture_on_stop(false, true));
    }

    #[tokio::test]
    async fn stop_drain_waits_for_a_final_in_flight_capture_frame() {
        let (capture_tx, mut capture_rx) = mpsc::channel(1);
        let samples = (0..PROFILE.sample_frames_per_packet())
            .map(|sample| {
                let phase =
                    sample as f32 * 440.0 * std::f32::consts::TAU / PROFILE.sample_rate_hz() as f32;
                phase.sin() * 0.2
            })
            .collect::<Vec<_>>();
        let frame = RawAudioFrame::new(PROFILE.channels(), samples.clone()).unwrap();
        let after_stop_frame = RawAudioFrame::new(PROFILE.channels(), samples).unwrap();
        let after_stop_tx = capture_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            capture_tx.send(frame).await.unwrap();
        });

        let mut encoder = OpusEncoderState::new(PROFILE).unwrap();
        let mut frames = Vec::new();
        let mut waveform = Vec::new();
        drain_capture_before_stream_stop(&mut capture_rx, &mut encoder, &mut frames, &mut waveform)
            .await
            .unwrap();

        assert_eq!(frames.len(), 1);
        assert_eq!(waveform.len(), 1);

        after_stop_tx.send(after_stop_frame).await.unwrap();
        drain_ready_capture(&mut capture_rx, &mut encoder, &mut frames, &mut waveform).unwrap();
        assert_eq!(
            frames.len(),
            2,
            "the post-stream-drop queue edge must be retained"
        );
        assert_eq!(waveform.len(), 2);
    }
}
