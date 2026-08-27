//! Strict, bounded Ogg/Opus framing for LXMF `FIELD_AUDIO` voice messages.
//!
//! The recorder owns Opus encoding. This module only wraps and unwraps those
//! packets, so adopting the standard container cannot change their quality.

use std::io::Cursor;

use lxst_core::{OPUS_ENCODED_PACKET_MAX_BYTES, opus_packet_duration_samples_48k};
use ogg::PacketReader;
use ogg::reading::PageParsingOptions;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};

use super::{VOICE_MEMO_MAX_AUDIO_BYTES, VOICE_MEMO_MAX_DURATION_MS, VoiceMemoResult};

const OPUS_CLOCK_HZ: u64 = 48_000;
const OPUS_HEAD_MAGIC: &[u8; 8] = b"OpusHead";
const OPUS_TAGS_MAGIC: &[u8; 8] = b"OpusTags";
const OPUS_HEAD_LEN: usize = 19;
const MAX_OPUS_HEAD_BYTES: usize = 64;
const OPUS_VERSION: u8 = 1;
const OUTPUT_CHANNELS: u8 = 1;
const OUTPUT_INPUT_RATE_HZ: u32 = 24_000;
const PACKETS_PER_PAGE: usize = 16;
const MAX_PACKET_COUNT: usize = (VOICE_MEMO_MAX_DURATION_MS as usize) * 2 / 5;
const MAX_PAGE_COUNT: usize = MAX_PACKET_COUNT + 2;
const MAX_TAG_BYTES: usize = 64 * 1024;
const MAX_TAG_ENTRY_BYTES: usize = 8 * 1024;
const MAX_TAG_ENTRIES: usize = 64;
const VENDOR: &[u8] = b"Ratspeak";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OggOpusMetadata {
    pub(crate) channels: u8,
    pub(crate) input_sample_rate_hz: u32,
    pub(crate) pre_skip_48k: u16,
    pub(crate) output_gain_q8: i16,
    pub(crate) granule_offset_48k: u64,
    pub(crate) end_trim_48k: u64,
    pub(crate) final_granule_48k: u64,
    pub(crate) playable_samples_48k: u64,
    pub(crate) duration_ms: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedOggOpus {
    pub(crate) metadata: OggOpusMetadata,
    pub(crate) packets: Vec<Vec<u8>>,
}

#[cfg(test)]
pub(crate) fn mux_opus_packets(
    packets: &[Vec<u8>],
    stream_serial: u32,
) -> VoiceMemoResult<Vec<u8>> {
    mux_opus_packets_with_timing(packets, stream_serial, 0, 0, 0, 0)
}

pub(super) fn mux_opus_packets_with_timing(
    packets: &[Vec<u8>],
    stream_serial: u32,
    pre_skip_48k: u16,
    end_trim_48k: u64,
    granule_offset_48k: u64,
    output_gain_q8: i16,
) -> VoiceMemoResult<Vec<u8>> {
    if packets.is_empty() || packets.len() > MAX_PACKET_COUNT {
        return Err("Voice message has an invalid Opus packet count".to_string());
    }

    let mut packet_durations = Vec::with_capacity(packets.len());
    let mut decoded_samples_48k = 0u64;
    for packet in packets {
        let duration = validate_opus_packet(packet)?;
        decoded_samples_48k = decoded_samples_48k
            .checked_add(duration)
            .ok_or_else(|| "Voice message duration overflows".to_string())?;
        packet_durations.push(duration);
    }
    let maximum_samples = u64::from(VOICE_MEMO_MAX_DURATION_MS) * OPUS_CLOCK_HZ / 1_000;
    if decoded_samples_48k > maximum_samples {
        return Err("Voice message duration exceeds the limit".to_string());
    }
    let final_granule_48k = granule_offset_48k
        .checked_add(decoded_samples_48k)
        .ok_or_else(|| "Voice message granule position overflows".to_string())?
        .checked_sub(end_trim_48k)
        .ok_or_else(|| "Voice message end trim is invalid".to_string())?;
    if final_granule_48k <= u64::from(pre_skip_48k) {
        return Err("Voice message trim removes all audio".to_string());
    }

    let mut writer = PacketWriter::new(Vec::new());
    writer
        .write_packet(
            opus_head(pre_skip_48k, output_gain_q8),
            stream_serial,
            PacketWriteEndInfo::EndPage,
            0,
        )
        .map_err(|error| format!("Could not write Opus identification header: {error}"))?;
    writer
        .write_packet(opus_tags(), stream_serial, PacketWriteEndInfo::EndPage, 0)
        .map_err(|error| format!("Could not write Opus comment header: {error}"))?;

    let mut granule_48k = 0u64;
    for (index, (packet, duration)) in packets.iter().zip(packet_durations).enumerate() {
        granule_48k += duration;
        let last = index + 1 == packets.len();
        let end = if last {
            PacketWriteEndInfo::EndStream
        } else if (index + 1).is_multiple_of(PACKETS_PER_PAGE) {
            PacketWriteEndInfo::EndPage
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        let packet_granule = if last {
            final_granule_48k
        } else {
            granule_offset_48k
                .checked_add(granule_48k)
                .ok_or_else(|| "Voice message granule position overflows".to_string())?
        };
        writer
            .write_packet(packet.clone(), stream_serial, end, packet_granule)
            .map_err(|error| format!("Could not write Opus audio packet: {error}"))?;
    }

    let data = writer.into_inner();
    if data.len() > VOICE_MEMO_MAX_AUDIO_BYTES
        || data.len() >= rns_protocol::resource::MAX_EFFICIENT_SIZE
    {
        return Err("Voice message exceeds the Ogg/Opus size limit".to_string());
    }
    Ok(data)
}

pub(crate) fn parse_ogg_opus(data: &[u8]) -> VoiceMemoResult<ParsedOggOpus> {
    let stream_serial = validate_physical_stream(data)?;
    let mut options = PageParsingOptions::default();
    options.verify_checksum = true;
    let mut reader = PacketReader::new_with_page_parse_opts(Cursor::new(data), options);

    let head = reader
        .read_packet_expected()
        .map_err(|error| format!("Could not read Opus identification header: {error}"))?;
    if head.stream_serial() != stream_serial
        || !head.first_in_stream()
        || !head.first_in_page()
        || !head.last_in_page()
        || head.last_in_stream()
        || head.absgp_page() != 0
    {
        return Err("Opus identification header placement is invalid".to_string());
    }
    let header = parse_opus_head(&head.data)?;

    let tags = reader
        .read_packet_expected()
        .map_err(|error| format!("Could not read Opus comment header: {error}"))?;
    if tags.stream_serial() != stream_serial
        || tags.first_in_stream()
        || tags.last_in_stream()
        || !tags.last_in_page()
        || tags.absgp_page() != 0
    {
        return Err("Opus comment header placement is invalid".to_string());
    }
    validate_opus_tags(&tags.data)?;

    let mut packets = Vec::new();
    let mut decoded_samples_48k = 0u64;
    let mut previous_page_granule_48k = None;
    let mut granule_offset_48k = None;
    let mut final_granule_48k = None;
    let mut end_trim_48k = None;

    while let Some(packet) = reader
        .read_packet()
        .map_err(|error| format!("Could not read Opus audio packet: {error}"))?
    {
        if packet.stream_serial() != stream_serial
            || packet.first_in_stream()
            || final_granule_48k.is_some()
            || packets.len() >= MAX_PACKET_COUNT
        {
            return Err("Ogg/Opus logical stream is invalid".to_string());
        }
        let duration = validate_opus_packet(&packet.data)?;
        decoded_samples_48k = decoded_samples_48k
            .checked_add(duration)
            .ok_or_else(|| "Voice message duration overflows".to_string())?;
        if packet.last_in_page() {
            let page_granule = packet.absgp_page();
            if packet.last_in_stream() {
                let offset = granule_offset_48k
                    .unwrap_or_else(|| page_granule.saturating_sub(decoded_samples_48k));
                let expected = offset
                    .checked_add(decoded_samples_48k)
                    .ok_or_else(|| "Ogg/Opus granule position overflows".to_string())?;
                if previous_page_granule_48k.is_some_and(|previous| page_granule < previous)
                    || page_granule > expected
                {
                    return Err("Ogg/Opus final granule position is invalid".to_string());
                }
                granule_offset_48k = Some(offset);
                end_trim_48k = Some(expected - page_granule);
                final_granule_48k = Some(page_granule);
            } else {
                let offset = match granule_offset_48k {
                    Some(offset) => offset,
                    None => page_granule
                        .checked_sub(decoded_samples_48k)
                        .ok_or_else(|| {
                            "Ogg/Opus initial granule position is invalid".to_string()
                        })?,
                };
                let expected = offset
                    .checked_add(decoded_samples_48k)
                    .ok_or_else(|| "Ogg/Opus granule position overflows".to_string())?;
                if page_granule != expected {
                    return Err("Ogg/Opus intermediate granule position is invalid".to_string());
                }
                granule_offset_48k = Some(offset);
            }
            previous_page_granule_48k = Some(page_granule);
        } else if packet.last_in_stream() {
            return Err("Ogg/Opus end-of-stream placement is invalid".to_string());
        }
        packets.push(packet.data);
    }

    let final_granule_48k = final_granule_48k
        .filter(|_| !packets.is_empty())
        .ok_or_else(|| "Ogg/Opus stream has no complete audio".to_string())?;
    let granule_offset_48k = granule_offset_48k.unwrap_or(0);
    let end_trim_48k = end_trim_48k.unwrap_or(0);
    let playable_samples_48k = decoded_samples_48k
        .checked_sub(u64::from(header.pre_skip_48k))
        .and_then(|samples| samples.checked_sub(end_trim_48k))
        .filter(|samples| *samples > 0)
        .ok_or_else(|| "Ogg/Opus trimming removes all audio".to_string())?;
    let maximum_samples = u64::from(VOICE_MEMO_MAX_DURATION_MS) * OPUS_CLOCK_HZ / 1_000;
    if playable_samples_48k > maximum_samples {
        return Err("Voice message duration exceeds the limit".to_string());
    }
    let duration_ms = u32::try_from(
        playable_samples_48k
            .saturating_mul(1_000)
            .div_ceil(OPUS_CLOCK_HZ),
    )
    .map_err(|_| "Voice message duration overflows".to_string())?;

    Ok(ParsedOggOpus {
        metadata: OggOpusMetadata {
            channels: header.channels,
            input_sample_rate_hz: header.input_sample_rate_hz,
            pre_skip_48k: header.pre_skip_48k,
            output_gain_q8: header.output_gain_q8,
            granule_offset_48k,
            end_trim_48k,
            final_granule_48k,
            playable_samples_48k,
            duration_ms,
        },
        packets,
    })
}

#[derive(Clone, Copy)]
struct OpusHead {
    channels: u8,
    input_sample_rate_hz: u32,
    pre_skip_48k: u16,
    output_gain_q8: i16,
}

fn opus_head(pre_skip_48k: u16, output_gain_q8: i16) -> Vec<u8> {
    let mut data = Vec::with_capacity(OPUS_HEAD_LEN);
    data.extend_from_slice(OPUS_HEAD_MAGIC);
    data.push(OPUS_VERSION);
    data.push(OUTPUT_CHANNELS);
    data.extend_from_slice(&pre_skip_48k.to_le_bytes());
    data.extend_from_slice(&OUTPUT_INPUT_RATE_HZ.to_le_bytes());
    data.extend_from_slice(&output_gain_q8.to_le_bytes());
    data.push(0);
    data
}

fn parse_opus_head(data: &[u8]) -> VoiceMemoResult<OpusHead> {
    if data.len() < OPUS_HEAD_LEN
        || &data[..8] != OPUS_HEAD_MAGIC
        || !(OPUS_VERSION..=15).contains(&data[8])
        || data[9] != OUTPUT_CHANNELS
        || data[18] != 0
    {
        return Err("Opus identification header is not supported".to_string());
    }
    if data[8] == OPUS_VERSION && data.len() != OPUS_HEAD_LEN {
        return Err("Opus version 1 identification header has trailing data".to_string());
    }
    Ok(OpusHead {
        channels: data[9],
        pre_skip_48k: u16::from_le_bytes([data[10], data[11]]),
        input_sample_rate_hz: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        output_gain_q8: i16::from_le_bytes([data[16], data[17]]),
    })
}

fn opus_tags() -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + 4 + VENDOR.len() + 4);
    data.extend_from_slice(OPUS_TAGS_MAGIC);
    data.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    data.extend_from_slice(VENDOR);
    data.extend_from_slice(&0u32.to_le_bytes());
    data
}

fn validate_opus_tags(data: &[u8]) -> VoiceMemoResult<()> {
    if data.len() < 16 || data.len() > MAX_TAG_BYTES || &data[..8] != OPUS_TAGS_MAGIC {
        return Err("Opus comment header is invalid".to_string());
    }
    let mut cursor = 8usize;
    let vendor_len = read_u32(data, &mut cursor)? as usize;
    if vendor_len > MAX_TAG_ENTRY_BYTES {
        return Err("Opus comment vendor is too large".to_string());
    }
    cursor = cursor
        .checked_add(vendor_len)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| "Opus comment vendor is truncated".to_string())?;
    let comment_count = read_u32(data, &mut cursor)? as usize;
    if comment_count > MAX_TAG_ENTRIES {
        return Err("Opus comment count exceeds the limit".to_string());
    }
    for _ in 0..comment_count {
        let length = read_u32(data, &mut cursor)? as usize;
        if length > MAX_TAG_ENTRY_BYTES {
            return Err("Opus comment is too large".to_string());
        }
        cursor = cursor
            .checked_add(length)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| "Opus comment is truncated".to_string())?;
    }
    // RFC 7845 permits bounded padding or future binary extensions after the
    // complete Vorbis-style comment list. Ratspeak does not interpret them.
    Ok(())
}

fn read_u32(data: &[u8], cursor: &mut usize) -> VoiceMemoResult<u32> {
    let end = cursor
        .checked_add(4)
        .filter(|end| *end <= data.len())
        .ok_or_else(|| "Opus comment header is truncated".to_string())?;
    let value = u32::from_le_bytes(data[*cursor..end].try_into().expect("four-byte slice"));
    *cursor = end;
    Ok(value)
}

fn validate_opus_packet(packet: &[u8]) -> VoiceMemoResult<u64> {
    if packet.is_empty() || packet.len() > OPUS_ENCODED_PACKET_MAX_BYTES {
        return Err("Voice message contains an invalid Opus packet size".to_string());
    }
    let duration = opus_packet_duration_samples_48k(packet)
        .map_err(|error| format!("Voice message contains an invalid Opus packet: {error}"))?;
    let duration = u64::try_from(duration)
        .map_err(|_| "Voice message Opus packet duration overflows".to_string())?;
    Ok(duration)
}

fn validate_physical_stream(data: &[u8]) -> VoiceMemoResult<u32> {
    if data.len() < 27 || data.len() > VOICE_MEMO_MAX_AUDIO_BYTES {
        return Err("Voice message Ogg size is invalid".to_string());
    }
    let mut cursor = 0usize;
    let mut serial = None;
    let mut expected_sequence = 0u32;
    let mut page_count = 0usize;
    let mut saw_eos = false;
    let mut partial_packet_bytes = 0usize;
    let mut completed_packets = 0usize;

    while cursor < data.len() {
        if saw_eos || page_count >= MAX_PAGE_COUNT {
            return Err("Voice message contains multiple or excessive Ogg streams".to_string());
        }
        let header_end = cursor
            .checked_add(27)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| "Voice message Ogg page header is truncated".to_string())?;
        let header = &data[cursor..header_end];
        if &header[..4] != b"OggS" || header[4] != 0 || header[5] & !0x07 != 0 {
            return Err("Voice message Ogg page header is invalid".to_string());
        }
        let flags = header[5];
        if (flags & 0x01 != 0) != (partial_packet_bytes != 0) {
            return Err("Voice message Ogg continuation flag is invalid".to_string());
        }
        let page_serial = u32::from_le_bytes(header[14..18].try_into().expect("serial slice"));
        let sequence = u32::from_le_bytes(header[18..22].try_into().expect("sequence slice"));
        if page_count == 0 {
            if flags & 0x02 == 0 || sequence != 0 {
                return Err("Voice message Ogg stream does not begin correctly".to_string());
            }
            serial = Some(page_serial);
        } else if flags & 0x02 != 0 || serial != Some(page_serial) {
            return Err("Voice message contains chained or multiplexed Ogg streams".to_string());
        }
        if sequence != expected_sequence {
            return Err("Voice message Ogg page sequence is invalid".to_string());
        }
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or_else(|| "Voice message Ogg page sequence overflows".to_string())?;

        let segment_count = usize::from(header[26]);
        let segments_end = header_end
            .checked_add(segment_count)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| "Voice message Ogg lacing table is truncated".to_string())?;
        let segments = &data[header_end..segments_end];
        let mut completed_on_page = false;
        for length in segments {
            partial_packet_bytes = partial_packet_bytes
                .checked_add(usize::from(*length))
                .ok_or_else(|| "Voice message Ogg packet size overflows".to_string())?;
            let packet_limit = match completed_packets {
                0 => MAX_OPUS_HEAD_BYTES,
                1 => MAX_TAG_BYTES,
                _ => OPUS_ENCODED_PACKET_MAX_BYTES,
            };
            if partial_packet_bytes > packet_limit {
                return Err("Voice message Ogg packet exceeds its size limit".to_string());
            }
            if *length < 255 {
                partial_packet_bytes = 0;
                completed_packets += 1;
                completed_on_page = true;
                if completed_packets > MAX_PACKET_COUNT + 2 {
                    return Err("Voice message Ogg packet count exceeds the limit".to_string());
                }
            }
        }
        let body_len = segments
            .iter()
            .try_fold(0usize, |total, length| {
                total.checked_add(usize::from(*length))
            })
            .ok_or_else(|| "Voice message Ogg page size overflows".to_string())?;
        let page_granule = u64::from_le_bytes(header[6..14].try_into().expect("granule slice"));
        if !completed_on_page && page_granule != u64::MAX {
            return Err("Voice message Ogg continuation page granule is invalid".to_string());
        }
        if completed_on_page && page_granule == u64::MAX {
            return Err("Voice message Ogg completed-packet granule is invalid".to_string());
        }
        cursor = segments_end
            .checked_add(body_len)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| "Voice message Ogg page body is truncated".to_string())?;
        saw_eos = flags & 0x04 != 0;
        page_count += 1;
    }
    if !saw_eos || partial_packet_bytes != 0 || completed_packets < 3 {
        return Err("Voice message Ogg stream has no end marker".to_string());
    }
    serial.ok_or_else(|| "Voice message Ogg stream is empty".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lxst_core::{OpusEncoderState, Profile, RawAudioFrame};

    fn encoded_packets(count: usize) -> Vec<Vec<u8>> {
        let profile = Profile::QualityMedium;
        let mut encoder = OpusEncoderState::new(profile).unwrap();
        (0..count)
            .map(|packet_index| {
                let samples: Vec<f32> = (0..profile.sample_frames_per_packet())
                    .map(|sample| {
                        let phase = ((packet_index * profile.sample_frames_per_packet() + sample)
                            as f32)
                            * 440.0
                            * std::f32::consts::TAU
                            / profile.sample_rate_hz() as f32;
                        phase.sin() * 0.2
                    })
                    .collect();
                let raw = RawAudioFrame::new(profile.channels(), samples).unwrap();
                encoder.encode_frame(&raw).unwrap().payload
            })
            .collect()
    }

    fn stream_with_headers(head: Vec<u8>, tags: Vec<u8>, audio_granule: u64) -> Vec<u8> {
        let serial = 0x5151;
        let audio = encoded_packets(1).remove(0);
        let mut writer = PacketWriter::new(Vec::new());
        writer
            .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        writer
            .write_packet(audio, serial, PacketWriteEndInfo::EndStream, audio_granule)
            .unwrap();
        writer.into_inner()
    }

    #[test]
    fn ogg_wrap_preserves_every_existing_opus_packet_byte_for_byte() {
        let packets = encoded_packets(33);
        let packet_hashes = packets
            .iter()
            .map(|packet| rns_crypto::sha::sha256(packet))
            .collect::<Vec<_>>();
        let ogg = mux_opus_packets(&packets, 0x5253_564d).unwrap();
        let parsed = parse_ogg_opus(&ogg).unwrap();

        assert_eq!(parsed.packets, packets);
        assert_eq!(
            parsed
                .packets
                .iter()
                .map(|packet| rns_crypto::sha::sha256(packet))
                .collect::<Vec<_>>(),
            packet_hashes
        );
        assert_eq!(parsed.metadata.channels, 1);
        assert_eq!(parsed.metadata.input_sample_rate_hz, 24_000);
        assert_eq!(parsed.metadata.pre_skip_48k, 0);
        assert_eq!(parsed.metadata.output_gain_q8, 0);
        assert_eq!(parsed.metadata.granule_offset_48k, 0);
        assert_eq!(parsed.metadata.end_trim_48k, 0);
        assert_eq!(parsed.metadata.final_granule_48k, 33 * 2_880);
        assert_eq!(parsed.metadata.duration_ms, 33 * 60);
    }

    #[test]
    fn maximum_recording_ogg_size_matches_the_compile_time_resource_bound() {
        let mut packet = encoded_packets(1).remove(0);
        packet.resize(60, 0);
        assert_eq!(validate_opus_packet(&packet).unwrap(), 2_880);
        let packets = vec![packet; 5_000];

        let ogg = mux_opus_packets(&packets, 0x5253_4d58).unwrap();

        assert_eq!(
            ogg.len(),
            crate::voice_memo::VOICE_MEMO_MAX_GENERATED_OGG_BYTES
        );
        assert!(ogg.len() < rns_protocol::resource::MAX_EFFICIENT_SIZE);
    }

    #[test]
    fn parser_honors_nonzero_pre_skip_and_final_granule_trim() {
        let packets = encoded_packets(3);
        let ogg = mux_opus_packets_with_timing(&packets, 7, 312, 480, 0, 0).unwrap();
        let parsed = parse_ogg_opus(&ogg).unwrap();

        assert_eq!(parsed.metadata.pre_skip_48k, 312);
        assert_eq!(parsed.metadata.final_granule_48k, 8_160);
        assert_eq!(parsed.metadata.end_trim_48k, 480);
        assert_eq!(parsed.metadata.playable_samples_48k, 7_848);
        assert_eq!(parsed.metadata.duration_ms, 164);
    }

    #[test]
    fn parser_tracks_initial_granule_offset_without_inflating_duration() {
        let packets = encoded_packets(17);
        let ogg = mux_opus_packets_with_timing(&packets, 8, 312, 120, 12_000, 0).unwrap();
        let parsed = parse_ogg_opus(&ogg).unwrap();

        assert_eq!(parsed.metadata.granule_offset_48k, 12_000);
        assert_eq!(parsed.metadata.end_trim_48k, 120);
        assert_eq!(parsed.metadata.final_granule_48k, 60_840);
        assert_eq!(parsed.metadata.playable_samples_48k, 48_528);
        assert_eq!(parsed.metadata.duration_ms, 1_011);
    }

    #[test]
    fn parser_accepts_standards_permitted_trim_across_final_page_packets() {
        let packets = encoded_packets(20);
        let ogg = mux_opus_packets_with_timing(&packets, 9, 0, 4_000, 0, 0).unwrap();
        let parsed = parse_ogg_opus(&ogg).unwrap();

        assert_eq!(parsed.metadata.end_trim_48k, 4_000);
        assert_eq!(parsed.metadata.playable_samples_48k, 53_600);
    }

    #[test]
    fn parser_accepts_a_well_formed_packet_continued_across_pages() {
        let serial = 10;
        let mut writer = PacketWriter::new(Vec::new());
        writer
            .write_packet(opus_head(0, 0), serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        writer
            .write_packet(opus_tags(), serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        for index in 0..254u64 {
            writer
                .write_packet(
                    vec![0xF8, 0],
                    serial,
                    PacketWriteEndInfo::NormalPacket,
                    (index + 1) * 960,
                )
                .unwrap();
        }
        let mut continued = vec![0u8; 256];
        continued[0] = 0xF8;
        writer
            .write_packet(
                continued.clone(),
                serial,
                PacketWriteEndInfo::EndStream,
                255 * 960,
            )
            .unwrap();

        let parsed = parse_ogg_opus(&writer.into_inner()).unwrap();
        assert_eq!(parsed.packets.len(), 255);
        assert_eq!(parsed.packets.last(), Some(&continued));
        assert_eq!(parsed.metadata.playable_samples_48k, 255 * 960);
    }

    #[test]
    fn parser_accepts_bounded_opus_tags_extension_data() {
        for extension in [vec![0, 0, 0], vec![0x52, 0x53, 0x01]] {
            let mut tags = opus_tags();
            tags.extend_from_slice(&extension);
            let stream = stream_with_headers(opus_head(0, 0), tags, 2_880);
            assert!(parse_ogg_opus(&stream).is_ok());
        }
    }

    #[test]
    fn parser_requires_exact_version_one_head_but_allows_bounded_future_extension() {
        let mut version_one = opus_head(0, 0);
        version_one.push(0);
        assert!(parse_ogg_opus(&stream_with_headers(version_one, opus_tags(), 2_880)).is_err());

        let mut compatible_future = opus_head(0, 0);
        compatible_future[8] = 2;
        compatible_future.extend_from_slice(&[0x52, 0x53]);
        assert!(
            parse_ogg_opus(&stream_with_headers(compatible_future, opus_tags(), 2_880)).is_ok()
        );
    }

    #[test]
    fn parser_rejects_minus_one_granule_when_audio_completes_on_page() {
        let stream = stream_with_headers(opus_head(0, 0), opus_tags(), u64::MAX);
        assert!(parse_ogg_opus(&stream).is_err());
    }

    #[test]
    fn parser_rejects_crc_corruption_truncation_and_trailing_stream_data() {
        let packets = encoded_packets(2);
        let ogg = mux_opus_packets(&packets, 11).unwrap();

        let mut corrupt = ogg.clone();
        *corrupt.last_mut().unwrap() ^= 0x80;
        assert!(parse_ogg_opus(&corrupt).is_err());
        assert!(parse_ogg_opus(&ogg[..ogg.len() - 1]).is_err());

        let mut trailing = ogg;
        trailing.push(0);
        assert!(parse_ogg_opus(&trailing).is_err());
    }

    #[test]
    fn parser_rejects_page_sequence_and_stream_identity_changes() {
        let packets = encoded_packets(17);
        let ogg = mux_opus_packets(&packets, 19).unwrap();
        let offsets = page_offsets(&ogg);
        assert!(offsets.len() >= 4);

        let mut sequence_gap = ogg.clone();
        sequence_gap[offsets[2] + 18..offsets[2] + 22].copy_from_slice(&99u32.to_le_bytes());
        assert!(parse_ogg_opus(&sequence_gap).is_err());

        let mut other_stream = ogg;
        other_stream[offsets[2] + 14..offsets[2] + 18].copy_from_slice(&20u32.to_le_bytes());
        assert!(parse_ogg_opus(&other_stream).is_err());
    }

    fn page_offsets(data: &[u8]) -> Vec<usize> {
        let mut offsets = Vec::new();
        let mut cursor = 0usize;
        while cursor < data.len() {
            offsets.push(cursor);
            let segment_count = usize::from(data[cursor + 26]);
            let segments = &data[cursor + 27..cursor + 27 + segment_count];
            cursor += 27
                + segment_count
                + segments
                    .iter()
                    .map(|value| usize::from(*value))
                    .sum::<usize>();
        }
        offsets
    }
}
