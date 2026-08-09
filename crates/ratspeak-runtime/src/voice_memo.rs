//! Asynchronous voice memo capture and the Ratspeak LXST voice-memo container.
//!
//! LXST live calls carry raw codec frames inside a realtime session. A voice
//! memo has different lifecycle and storage requirements, so this module
//! reuses the trusted LXST Opus implementation while giving asynchronous media
//! a small, versioned, strictly bounded container of its own.

use std::sync::{Arc, Mutex};

use cpal::Stream;
use lxst_core::{CodecKind, Frame, OpusDecoderState, OpusEncoderState, Profile, RawAudioFrame};
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::state::AppState;

pub const VOICE_MEMO_EXTENSION: &str = "lxvm";
pub const VOICE_MEMO_MIME: &str = "audio/x-lxst-voice-memo";
pub const VOICE_MEMO_FILENAME: &str = "Voice message.lxvm";
pub const VOICE_MEMO_MAX_DURATION_MS: u32 = 5 * 60 * 1_000;

const MAGIC: &[u8; 4] = b"LXVM";
const VERSION: u8 = 1;
const PROFILE: Profile = Profile::QualityMedium;
const PROFILE_WIRE: u8 = 0x40;
const HEADER_LEN: usize = 16;
const FRAME_MS: u32 = 60;
const MAX_FRAME_COUNT: usize = (VOICE_MEMO_MAX_DURATION_MS / FRAME_MS) as usize;
const MAX_FRAME_PAYLOAD: usize = 60;
const MAX_CONTAINER_BYTES: usize =
    HEADER_LEN + MAX_FRAME_COUNT * (1 + std::mem::size_of::<u16>() + MAX_FRAME_PAYLOAD);
const MIN_FRAME_COUNT: usize = 3;

pub type VoiceMemoResult<T> = Result<T, String>;

#[derive(Clone, Debug, Serialize)]
pub struct VoiceMemoStatus {
    pub state: String,
    pub duration_ms: u32,
    pub max_duration_ms: u32,
}

impl VoiceMemoStatus {
    fn idle() -> Self {
        Self {
            state: "idle".to_string(),
            duration_ms: 0,
            max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
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
    command_tx: mpsc::Sender<RecorderCommand>,
    status: Arc<Mutex<VoiceMemoStatus>>,
    task: Option<JoinHandle<()>>,
}

impl VoiceMemoRecordingHandle {
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
    let (command_tx, command_rx) = mpsc::channel(4);
    let status = Arc::new(Mutex::new(VoiceMemoStatus {
        state: "starting".to_string(),
        duration_ms: 0,
        max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
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
        let (platform_audio_session, stream, capture_rx) =
            match crate::voice::start_microphone_capture(PROFILE) {
                Ok(capture) => capture,
                Err(error) => {
                    let _ = started_tx.send(Err(error));
                    return;
                }
            };
        let _ = update_status(&task_status, "recording", 0);
        let _ = started_tx.send(Ok(()));
        runtime.block_on(drive_recording(
            task_state,
            platform_audio_session,
            stream,
            capture_rx,
            command_rx,
            encoder,
            task_status,
        ));
    });

    let handle = VoiceMemoRecordingHandle {
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

pub async fn set_paused(state: &AppState, paused: bool) -> VoiceMemoResult<VoiceMemoStatus> {
    let command_tx = state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?
        .as_ref()
        .map(VoiceMemoRecordingHandle::command_tx)
        .ok_or_else(|| "No voice memo is being recorded".to_string())?;
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

pub async fn stop_recording(state: &AppState) -> VoiceMemoResult<VoiceMemoDraft> {
    let _control = state.voice_memo_control_lock.lock().await;
    let handle = state
        .voice_memo_recording
        .lock()
        .map_err(|_| "Voice memo state is unavailable".to_string())?
        .take()
        .ok_or_else(|| "No voice memo is being recorded".to_string())?;
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

async fn drive_recording(
    state: Arc<AppState>,
    _platform_audio_session: crate::voice::PlatformVoiceAudioSession,
    stream: Stream,
    mut capture_rx: mpsc::Receiver<RawAudioFrame>,
    mut command_rx: mpsc::Receiver<RecorderCommand>,
    mut encoder: OpusEncoderState,
    status: Arc<Mutex<VoiceMemoStatus>>,
) {
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
                        let snapshot = update_status(&status, next_state, frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": next_state,
                                "duration_ms": snapshot.duration_ms,
                                "level": 0,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                            }),
                        );
                        let _ = reply.send(snapshot);
                    }
                    RecorderCommand::Stop { reply } => {
                        stream.take();
                        if capture_open && !paused {
                            while let Ok(frame) = capture_rx.try_recv() {
                                if frames.len() >= MAX_FRAME_COUNT { break; }
                                if let Err(error) = encode_captured_frame(&mut encoder, frame, &mut frames, &mut waveform) {
                                    let _ = reply.send(Err(error));
                                    return;
                                }
                            }
                        }
                        let result = finish_draft(frames, waveform);
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
                        let snapshot = update_status(&status, "recording", frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": "recording",
                                "duration_ms": snapshot.duration_ms,
                                "level": level,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                            }),
                        );
                        if frames.len() >= MAX_FRAME_COUNT {
                            stream.take();
                            capture_open = false;
                            let snapshot = update_status(&status, "limit", frames.len());
                            state.emit_to_all(
                                "voice_memo_recording",
                                serde_json::json!({
                                    "state": "limit",
                                    "duration_ms": snapshot.duration_ms,
                                    "level": 0,
                                    "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                                }),
                            );
                        }
                    }
                    Err(message) => {
                        stream.take();
                        capture_open = false;
                        let snapshot = update_status(&status, "error", frames.len());
                        state.emit_to_all(
                            "voice_memo_recording",
                            serde_json::json!({
                                "state": "error",
                                "duration_ms": snapshot.duration_ms,
                                "level": 0,
                                "message": message,
                                "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
                            }),
                        );
                    }
                }
            }
        }
    }

    stream.take();
    let _ = update_status(&status, "idle", 0);
    state.emit_to_all(
        "voice_memo_recording",
        serde_json::json!({
            "state": "idle",
            "duration_ms": 0,
            "level": 0,
            "max_duration_ms": VOICE_MEMO_MAX_DURATION_MS,
        }),
    );
}

fn update_status(
    status: &Arc<Mutex<VoiceMemoStatus>>,
    state: &str,
    frame_count: usize,
) -> VoiceMemoStatus {
    let snapshot = VoiceMemoStatus {
        state: state.to_string(),
        duration_ms: duration_for_frames(frame_count),
        max_duration_ms: VOICE_MEMO_MAX_DURATION_MS,
    };
    match status.lock() {
        Ok(mut current) => *current = snapshot.clone(),
        Err(poisoned) => *poisoned.into_inner() = snapshot.clone(),
    }
    snapshot
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
    if encoded.payload.is_empty() || encoded.payload.len() > MAX_FRAME_PAYLOAD {
        return Err("Voice memo encoder produced an invalid frame".to_string());
    }
    frames.push(encoded.payload);
    waveform.push(level);
    Ok(level)
}

fn finish_draft(frames: Vec<Vec<u8>>, waveform: Vec<u8>) -> VoiceMemoResult<VoiceMemoDraft> {
    if frames.len() < MIN_FRAME_COUNT {
        return Err("Voice memo is too short".to_string());
    }
    let duration_ms = duration_for_frames(frames.len());
    let data = encode_container(&frames, &waveform)?;
    Ok(VoiceMemoDraft {
        data,
        duration_ms,
        waveform,
    })
}

fn duration_for_frames(frame_count: usize) -> u32 {
    (frame_count as u32).saturating_mul(FRAME_MS)
}

fn encode_container(frames: &[Vec<u8>], waveform: &[u8]) -> VoiceMemoResult<Vec<u8>> {
    if frames.is_empty() || frames.len() > MAX_FRAME_COUNT || frames.len() != waveform.len() {
        return Err("Voice memo frame metadata is invalid".to_string());
    }
    let frame_count = u16::try_from(frames.len())
        .map_err(|_| "Voice memo contains too many frames".to_string())?;
    let mut data = Vec::with_capacity(
        HEADER_LEN + waveform.len() + frames.iter().map(|frame| frame.len() + 2).sum::<usize>(),
    );
    data.extend_from_slice(MAGIC);
    data.push(VERSION);
    data.push(PROFILE_WIRE);
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&duration_for_frames(frames.len()).to_be_bytes());
    data.extend_from_slice(&frame_count.to_be_bytes());
    data.extend_from_slice(&frame_count.to_be_bytes());
    data.extend_from_slice(waveform);
    for frame in frames {
        if frame.is_empty() || frame.len() > MAX_FRAME_PAYLOAD {
            return Err("Voice memo contains an invalid Opus frame".to_string());
        }
        let length = frame.len() as u16;
        data.extend_from_slice(&length.to_be_bytes());
        data.extend_from_slice(frame);
    }
    if data.len() > MAX_CONTAINER_BYTES {
        return Err("Voice memo exceeds the container limit".to_string());
    }
    Ok(data)
}

struct ParsedVoiceMemo {
    duration_ms: u32,
    waveform: Vec<u8>,
    frames: Vec<Vec<u8>>,
}

fn parse_container(data: &[u8]) -> VoiceMemoResult<ParsedVoiceMemo> {
    if data.len() < HEADER_LEN || data.len() > MAX_CONTAINER_BYTES {
        return Err("Voice memo size is invalid".to_string());
    }
    if &data[..4] != MAGIC {
        return Err("Voice memo header is invalid".to_string());
    }
    if data[4] != VERSION {
        return Err("Voice memo version is not supported".to_string());
    }
    if data[5] != PROFILE_WIRE || u16::from_be_bytes([data[6], data[7]]) != 0 {
        return Err("Voice memo codec profile is not supported".to_string());
    }
    let duration_ms = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let waveform_count = usize::from(u16::from_be_bytes([data[12], data[13]]));
    let frame_count = usize::from(u16::from_be_bytes([data[14], data[15]]));
    if frame_count == 0
        || frame_count > MAX_FRAME_COUNT
        || waveform_count != frame_count
        || duration_ms != duration_for_frames(frame_count)
        || duration_ms > VOICE_MEMO_MAX_DURATION_MS
    {
        return Err("Voice memo metadata is invalid".to_string());
    }
    let waveform_end = HEADER_LEN
        .checked_add(waveform_count)
        .ok_or_else(|| "Voice memo metadata overflows".to_string())?;
    if waveform_end > data.len() {
        return Err("Voice memo waveform is truncated".to_string());
    }
    let waveform = data[HEADER_LEN..waveform_end].to_vec();
    let mut cursor = waveform_end;
    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        let length_end = cursor
            .checked_add(2)
            .ok_or_else(|| "Voice memo frame length overflows".to_string())?;
        if length_end > data.len() {
            return Err("Voice memo frame table is truncated".to_string());
        }
        let length = usize::from(u16::from_be_bytes([data[cursor], data[cursor + 1]]));
        if length == 0 || length > MAX_FRAME_PAYLOAD {
            return Err("Voice memo contains an invalid Opus frame".to_string());
        }
        cursor = length_end;
        let frame_end = cursor
            .checked_add(length)
            .ok_or_else(|| "Voice memo frame length overflows".to_string())?;
        if frame_end > data.len() {
            return Err("Voice memo audio is truncated".to_string());
        }
        frames.push(data[cursor..frame_end].to_vec());
        cursor = frame_end;
    }
    if cursor != data.len() {
        return Err("Voice memo has unexpected trailing data".to_string());
    }
    Ok(ParsedVoiceMemo {
        duration_ms,
        waveform,
        frames,
    })
}

pub fn decode_voice_memo(data: &[u8]) -> VoiceMemoResult<VoiceMemoPlayback> {
    let parsed = parse_container(data)?;
    let mut decoder = OpusDecoderState::new(PROFILE)
        .map_err(|error| format!("Could not initialize voice memo playback: {error}"))?;
    let mut pcm = Vec::<i16>::with_capacity(
        parsed.frames.len() * PROFILE.sample_frames_per_packet() * usize::from(PROFILE.channels()),
    );
    for payload in &parsed.frames {
        let decoded = decoder
            .decode_frame(&Frame::new(CodecKind::Opus, payload.clone()))
            .map_err(|error| format!("Could not decode voice memo audio: {error}"))?;
        pcm.extend(
            decoded
                .samples
                .iter()
                .map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16),
        );
    }
    let wav_data = encode_pcm16_wav(&pcm, PROFILE.sample_rate_hz(), PROFILE.channels())?;
    Ok(VoiceMemoPlayback {
        wav_data,
        duration_ms: parsed.duration_ms,
        waveform: parsed.waveform,
        sample_rate_hz: PROFILE.sample_rate_hz(),
        channels: PROFILE.channels(),
    })
}

pub fn inspect_voice_memo(data: &[u8]) -> VoiceMemoResult<VoiceMemoMetadata> {
    let parsed = parse_container(data)?;
    Ok(VoiceMemoMetadata {
        duration_ms: parsed.duration_ms,
        waveform: parsed.waveform,
    })
}

fn encode_pcm16_wav(
    samples: &[i16],
    sample_rate_hz: u32,
    channels: u8,
) -> VoiceMemoResult<Vec<u8>> {
    let data_len = samples
        .len()
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
    for sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn container_round_trip_decodes_to_bounded_wav() {
        let (frames, waveform) = synthetic_frames(8);
        let encoded = encode_container(&frames, &waveform).unwrap();
        let playback = decode_voice_memo(&encoded).unwrap();
        assert_eq!(playback.duration_ms, 8 * FRAME_MS);
        assert_eq!(playback.waveform, waveform);
        assert_eq!(playback.sample_rate_hz, 24_000);
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
        let (frames, waveform) = synthetic_frames(4);
        let encoded = encode_container(&frames, &waveform).unwrap();
        assert!(parse_container(&encoded[..encoded.len() - 1]).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(parse_container(&trailing).is_err());
    }

    #[test]
    fn parser_rejects_unbounded_counts_before_allocating() {
        let mut data = vec![0u8; HEADER_LEN];
        data[..4].copy_from_slice(MAGIC);
        data[4] = VERSION;
        data[5] = PROFILE_WIRE;
        data[8..12].copy_from_slice(&VOICE_MEMO_MAX_DURATION_MS.to_be_bytes());
        data[12..14].copy_from_slice(&u16::MAX.to_be_bytes());
        data[14..16].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(parse_container(&data).is_err());
    }

    #[test]
    fn encoder_keeps_max_memo_under_reticulum_efficient_resource_limit() {
        const {
            assert!(MAX_CONTAINER_BYTES < rns_protocol::resource::MAX_EFFICIENT_SIZE);
        }
    }

    #[test]
    fn too_short_recording_is_not_sendable() {
        let (frames, waveform) = synthetic_frames(MIN_FRAME_COUNT - 1);
        assert!(finish_draft(frames, waveform).is_err());
    }
}
