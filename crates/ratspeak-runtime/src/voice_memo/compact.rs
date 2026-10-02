//! Standard LXMF 700C: headerless four-byte native frames, not LXST packets.
//! Decode sequentially even when seeking, preserving the predictor history.
use super::{VoiceMemoMetadata, VoiceMemoResult};
use lxst_codec2::{Codec, Mode};
use lxst_core::RawAudioFrame;

pub const MODE: u8 = 0x03;
pub const FILENAME: &str = "Voice message.c2raw";
pub const MIME: &str = "application/octet-stream";
pub const FRAME_MS: u32 = 40;
pub const FRAME_BYTES: usize = 4;
pub const FRAME_SAMPLES: usize = 320;
pub const SAMPLE_RATE: u32 = 8_000;
pub const RECORD_MS: u32 = 15_000;
pub const RECORD_BYTES: usize = 1_500;
pub const PLAYBACK_BYTES: usize = 3_000;

pub fn codec() -> Box<Codec> {
    let mut storage = Box::<Codec>::new_uninit();
    Codec::initialise(&mut storage, Mode::Rate700C);
    // SAFETY: initialise writes every field directly into its final allocation.
    unsafe { storage.assume_init() }
}

pub fn inspect(data: &[u8]) -> VoiceMemoResult<VoiceMemoMetadata> {
    if data.is_empty() || data.len() > PLAYBACK_BYTES || !data.len().is_multiple_of(FRAME_BYTES) {
        return Err("Compact voice message is empty, incomplete, or longer than 30 seconds".into());
    }
    Ok(VoiceMemoMetadata {
        duration_ms: (data.len() / FRAME_BYTES) as u32 * FRAME_MS,
        waveform: Vec::new(),
    })
}

pub struct Source {
    codec: Box<Codec>,
    bytes: Vec<u8>,
    offset: usize,
    discard_samples: usize,
}

impl Source {
    pub fn new(bytes: Vec<u8>, position_ms: u32) -> VoiceMemoResult<Self> {
        let metadata = inspect(&bytes)?;
        Ok(Self {
            codec: codec(),
            bytes,
            offset: 0,
            discard_samples: position_ms.min(metadata.duration_ms) as usize * 8,
        })
    }

    pub fn next_decoded(&mut self) -> VoiceMemoResult<Option<RawAudioFrame>> {
        while self.offset < self.bytes.len() {
            let mut pcm = [0i16; FRAME_SAMPLES];
            self.codec
                .decode(
                    &self.bytes[self.offset..self.offset + FRAME_BYTES],
                    &mut pcm,
                )
                .map_err(|_| "Compact voice message could not be decoded".to_string())?;
            self.offset += FRAME_BYTES;
            let discard = self.discard_samples.min(FRAME_SAMPLES);
            self.discard_samples -= discard;
            if discard == FRAME_SAMPLES {
                continue;
            }
            let samples: Vec<f32> = pcm[discard..]
                .iter()
                .map(|v| f32::from(*v) / 32768.0)
                .collect();
            return RawAudioFrame::new(1, samples)
                .map(Some)
                .map_err(|e| e.to_string());
        }
        Ok(None)
    }
}

pub fn encode(codec: &mut Codec, frame: &RawAudioFrame) -> VoiceMemoResult<Vec<u8>> {
    if frame.channels != 1 || frame.samples.len() != FRAME_SAMPLES {
        return Err("Compact recorder received an invalid audio frame".into());
    }
    let mut pcm = [0i16; FRAME_SAMPLES];
    for (out, value) in pcm.iter_mut().zip(&frame.samples) {
        if !value.is_finite() {
            return Err("Compact recorder received invalid samples".into());
        }
        *out = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    }
    let mut bytes = vec![0; FRAME_BYTES];
    codec
        .encode(&pcm, &mut bytes)
        .map_err(|_| "Compact voice encoding failed".to_string())?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_memo::{MemoFormat, decode_voice_memo, decode_voice_memo_format};

    #[test]
    fn native_shape_limits_and_typed_container() {
        for length in [0, 1, 3, 5, 2_999, 3_004] {
            assert!(inspect(&vec![0; length]).is_err());
        }
        for length in [4, RECORD_BYTES, PLAYBACK_BYTES] {
            assert_eq!(
                inspect(&vec![0; length]).unwrap().duration_ms,
                length as u32 * 10
            );
        }
        assert!(MemoFormat::from_mode(0).is_err());
        assert!(decode_voice_memo(&[0; 4]).is_err()); // never infer raw audio from an Opus failure
    }

    #[test]
    fn native_frames_decode_and_seek_with_predictor_history() {
        let mut encoder = codec();
        let mut bytes = Vec::new();
        for n in 0..25 {
            let samples: Vec<f32> = (0..FRAME_SAMPLES)
                .map(|i| (((n * FRAME_SAMPLES + i) as f32 * 0.173).sin()) * 0.2)
                .collect();
            bytes.extend(encode(&mut encoder, &RawAudioFrame::new(1, samples).unwrap()).unwrap());
        }
        assert_eq!(bytes.len(), 100); // four native bytes, no .c2/LXST header
        let playback = decode_voice_memo_format(&bytes, MemoFormat::Compact).unwrap();
        assert_eq!(
            (
                playback.duration_ms,
                playback.sample_rate_hz,
                playback.channels
            ),
            (1_000, 8_000, 1)
        );
        assert_eq!(playback.wav_data.len(), 44 + 8_000 * 2);
        let mut all = Source::new(bytes.clone(), 0).unwrap();
        let mut decoded = Vec::new();
        while let Some(frame) = all.next_decoded().unwrap() {
            decoded.extend(frame.samples);
        }
        let mut seek = Source::new(bytes.clone(), 137).unwrap();
        let mut tail = Vec::new();
        while let Some(frame) = seek.next_decoded().unwrap() {
            tail.extend(frame.samples);
        }
        assert_eq!(tail, decoded[137 * 8..]);
        assert!(
            Source::new(bytes, 1_000)
                .unwrap()
                .next_decoded()
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn recorder_uses_native_frames_and_stops_at_fifteen_seconds() {
        use crate::voice_memo::{
            MemoEncoder, drain_ready_capture, encode_captured_frame, finish_compact_draft,
        };
        let mut encoder = MemoEncoder::new(MemoFormat::Compact).unwrap();
        let mut frames = Vec::new();
        let mut waveform = Vec::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(380);
        for _ in 0..380 {
            tx.send(RawAudioFrame::new(1, vec![0.1; FRAME_SAMPLES]).unwrap())
                .await
                .unwrap();
        }
        drain_ready_capture(&mut rx, &mut encoder, &mut frames, &mut waveform).unwrap();
        let draft = finish_compact_draft(frames, waveform).unwrap();
        assert_eq!(
            (draft.data.len(), draft.duration_ms, draft.format),
            (RECORD_BYTES, RECORD_MS, MemoFormat::Compact)
        );
        assert_eq!(rx.len(), 5);
        assert!(finish_compact_draft(Vec::new(), Vec::new()).is_err());
        assert!(
            encode_captured_frame(
                &mut encoder,
                RawAudioFrame::new(1, vec![0.0; 319]).unwrap(),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_err()
        );
    }
}
