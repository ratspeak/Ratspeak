//! The node's `status` reply: one-byte tag and length fields, unknown tags skipped.
//!
//! USB status uses the same encoding. Clients read it for update checks, pacing (radio duty and
//! airtime) and delta bases (the running image).

use crate::frame::EncodeError;
use crate::manifest::{BoardTarget, FirmwareVersion};
use crate::{be16, be32};

/// Length of a Reticulum identity hash or LXMF address.
pub const OWNER_HASH_LEN: usize = 16;

/// Endpoint roles: the primary also edits owners/policy/settings and requests restarts;
/// helpers read status/settings and operate updates. The enum itself enforces no permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OwnerRole {
    Primary,
    Helper,
}

impl OwnerRole {
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Primary => 1,
            Self::Helper => 2,
        }
    }

    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Primary),
            2 => Some(Self::Helper),
            _ => None,
        }
    }
}

/// Who started an update the node staged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateSource {
    /// An owner over the management Link.
    Link,
    /// Retired HTTP management-key source. Retained when reading or rewriting older records.
    Root,
    /// USB source marker. A missing record is separate; this value does not prove a signature.
    Usb,
}

impl UpdateSource {
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Link => 1,
            Self::Root => 2,
            Self::Usb => 3,
        }
    }

    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Link),
            2 => Some(Self::Root),
            3 => Some(Self::Usb),
            _ => None,
        }
    }
}

/// Caller-supplied status used for update checks and transfer estimates. It omits the owner list,
/// but `last_update` can include the initiating owner's identity hash. Optional/missing fields
/// and decoded values are not evidence of readiness, authenticity or regulatory compliance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ManageStatus {
    pub board: Option<BoardTarget>,
    pub firmware: Option<FirmwareVersion>,
    /// First 8 bytes of SHA-256 over the compiled-in release key.
    pub release_key_id: Option<[u8; 8]>,
    pub ota_accepted_sequence: u32,
    pub ota_pending_sequence: Option<u32>,
    pub ota_trial_pending: bool,
    pub ota_slot_len: u32,
    pub owner_count: u8,
    /// The requester's own role.
    pub role: Option<OwnerRole>,
    pub nearby_only: bool,
    pub radio: Option<StatusRadio>,
    /// Bit 0 LoRa, bit 1 backbone, bits 2 and 3 the accepted peers.
    pub interfaces_up: u8,
    pub uptime_s: u32,
    pub last_update: Option<LastUpdateStatus>,
    /// A Link transfer in progress, from any owner.
    pub transfer: Option<TransferStatus>,
    /// Present on boards with Wi-Fi.
    pub wifi: Option<WifiStatus>,
    /// The requester's own Link: the interface it arrived on and its hops.
    pub path: Option<SessionPath>,
    /// The running image: what a delta patch must be made against.
    pub image: Option<RunningImage>,
    /// The release the running image was built as.
    pub release: Option<ReleaseVersion>,
}

pub const RELEASE_VERSION_MAX: usize = 24;

/// A release label of 1–24 ASCII graphic bytes when constructed with `new`; no semantic-version
/// parsing is performed. `Default` is empty and cannot round-trip as a status release tag.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReleaseVersion {
    len: u8,
    bytes: [u8; RELEASE_VERSION_MAX],
}

impl ReleaseVersion {
    pub fn new(text: &str) -> Option<Self> {
        let source = text.as_bytes();
        if source.is_empty()
            || source.len() > RELEASE_VERSION_MAX
            || !source.iter().all(u8::is_ascii_graphic)
        {
            return None;
        }
        let mut bytes = [0u8; RELEASE_VERSION_MAX];
        bytes[..source.len()].copy_from_slice(source);
        Some(Self {
            len: source.len() as u8,
            bytes,
        })
    }

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunningImage {
    pub len: u32,
    pub sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LastUpdateStatus {
    pub sequence: u32,
    pub source: UpdateSource,
    /// Identity hash of the owner who started it; zero otherwise.
    pub owner: [u8; OWNER_HASH_LEN],
    /// Board-reported match between the running image and the stored eight-byte digest prefix.
    pub running: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferStatus {
    pub next_offset: u32,
    pub total: u32,
    pub delta: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WifiStatus {
    /// 0 off, 1 station, 2 access point.
    pub mode: u8,
    pub service_ready: bool,
    /// Active Wi-Fi service address (station or access point), or zero when unavailable.
    pub ip: [u8; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SessionPath {
    pub interface_id: u8,
    pub hops: u8,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct StatusRadio {
    pub frequency_hz: u32,
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power_dbm: i8,
    /// Reported hourly duty-cycle setting in tenths of a percent; zero means no limit reported.
    pub duty_tenths_percent: u16,
    /// Reported per-transmission limit in milliseconds; zero means no limit reported.
    pub max_transmission_ms: u32,
    pub hour_used_ms: u32,
    pub hour_budget_ms: u32,
}

const TAG_BOARD: u8 = 0x01;
const TAG_FIRMWARE: u8 = 0x02;
const TAG_RELEASE_KEY: u8 = 0x03;
const TAG_OTA: u8 = 0x04;
const TAG_OTA_PENDING: u8 = 0x05;
const TAG_OWNERS: u8 = 0x06;
const TAG_RADIO: u8 = 0x07;
const TAG_INTERFACES: u8 = 0x08;
const TAG_UPTIME: u8 = 0x09;
const TAG_LAST_UPDATE: u8 = 0x0A;
const TAG_TRANSFER: u8 = 0x0B;
const TAG_WIFI: u8 = 0x0C;
const TAG_PATH: u8 = 0x0D;
const TAG_IMAGE: u8 = 0x0E;
const TAG_RELEASE: u8 = 0x0F;
const WIFI_READY: u8 = 0x01;

// Bit 0x01 (management key) and Wi-Fi bits 0x02/0x04 (window, HTTP OTA) are retired: sent as 0.
const FLAG_NEARBY_ONLY: u8 = 0x02;
const FLAG_TRIAL: u8 = 0x04;

impl ManageStatus {
    /// Encode supplied fields as TLVs without semantic validation. A short output can retain
    /// earlier complete TLVs. Do not supply an empty default `ReleaseVersion` for a release tag.
    pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
        let mut writer = TlvWriter { out, len: 0 };
        if let Some(board) = self.board {
            writer.put(TAG_BOARD, &[board.platform, board.mcu, board.board])?;
        }
        if let Some(version) = self.firmware {
            let mut value = [0u8; 6];
            value[..2].copy_from_slice(&version.major.to_be_bytes());
            value[2..4].copy_from_slice(&version.minor.to_be_bytes());
            value[4..].copy_from_slice(&version.patch.to_be_bytes());
            writer.put(TAG_FIRMWARE, &value)?;
        }
        if let Some(key) = self.release_key_id {
            writer.put(TAG_RELEASE_KEY, &key)?;
        }
        let mut flags = 0;
        if self.nearby_only {
            flags |= FLAG_NEARBY_ONLY;
        }
        if self.ota_trial_pending {
            flags |= FLAG_TRIAL;
        }
        let mut ota = [0u8; 9];
        ota[..4].copy_from_slice(&self.ota_accepted_sequence.to_be_bytes());
        ota[4..8].copy_from_slice(&self.ota_slot_len.to_be_bytes());
        ota[8] = flags;
        writer.put(TAG_OTA, &ota)?;
        if let Some(pending) = self.ota_pending_sequence {
            writer.put(TAG_OTA_PENDING, &pending.to_be_bytes())?;
        }
        writer.put(
            TAG_OWNERS,
            &[self.owner_count, self.role.map_or(0, OwnerRole::to_byte)],
        )?;
        if let Some(radio) = self.radio {
            let mut value = [0u8; 25];
            value[..4].copy_from_slice(&radio.frequency_hz.to_be_bytes());
            value[4..8].copy_from_slice(&radio.bandwidth_hz.to_be_bytes());
            value[8] = radio.spreading_factor;
            value[9] = radio.coding_rate;
            value[10] = radio.tx_power_dbm as u8;
            value[11..13].copy_from_slice(&radio.duty_tenths_percent.to_be_bytes());
            value[13..17].copy_from_slice(&radio.max_transmission_ms.to_be_bytes());
            value[17..21].copy_from_slice(&radio.hour_used_ms.to_be_bytes());
            value[21..].copy_from_slice(&radio.hour_budget_ms.to_be_bytes());
            writer.put(TAG_RADIO, &value)?;
        }
        writer.put(TAG_INTERFACES, &[self.interfaces_up])?;
        writer.put(TAG_UPTIME, &self.uptime_s.to_be_bytes())?;
        if let Some(update) = self.last_update {
            let mut value = [0u8; 22];
            value[..4].copy_from_slice(&update.sequence.to_be_bytes());
            value[4] = update.source.to_byte();
            value[5] = u8::from(update.running);
            value[6..].copy_from_slice(&update.owner);
            writer.put(TAG_LAST_UPDATE, &value)?;
        }
        if let Some(transfer) = self.transfer {
            let mut value = [0u8; 9];
            value[..4].copy_from_slice(&transfer.next_offset.to_be_bytes());
            value[4..8].copy_from_slice(&transfer.total.to_be_bytes());
            value[8] = u8::from(transfer.delta);
            writer.put(TAG_TRANSFER, &value)?;
        }
        if let Some(wifi) = self.wifi {
            let mut flags = 0;
            if wifi.service_ready {
                flags |= WIFI_READY;
            }
            let mut value = [0u8; 6];
            value[0] = wifi.mode;
            value[1] = flags;
            value[2..].copy_from_slice(&wifi.ip);
            writer.put(TAG_WIFI, &value)?;
        }
        if let Some(path) = self.path {
            writer.put(TAG_PATH, &[path.interface_id, path.hops])?;
        }
        if let Some(image) = self.image {
            let mut value = [0u8; 36];
            value[..4].copy_from_slice(&image.len.to_be_bytes());
            value[4..].copy_from_slice(&image.sha256);
            writer.put(TAG_IMAGE, &value)?;
        }
        if let Some(release) = &self.release {
            writer.put(TAG_RELEASE, release.as_str().as_bytes())?;
        }
        Ok(writer.len)
    }

    /// Decode TLV framing and known field widths; skip unknown tags. Missing fields default and
    /// later duplicate tags replace earlier values. Empty input is a valid default status.
    /// Invalid release text or update-source bytes fail; radio, counts, modes, flags and sequence
    /// relationships are otherwise not validated. Authenticate and assess required fields outside.
    pub fn decode(body: &[u8]) -> Option<Self> {
        let mut status = Self::default();
        let mut rest = body;
        while !rest.is_empty() {
            let [tag, len, tail @ ..] = rest else {
                return None;
            };
            let value = tail.get(..usize::from(*len))?;
            rest = &tail[usize::from(*len)..];
            match (*tag, value.len()) {
                (TAG_BOARD, 3) => {
                    status.board = Some(BoardTarget {
                        platform: value[0],
                        mcu: value[1],
                        board: value[2],
                    });
                }
                (TAG_FIRMWARE, 6) => {
                    status.firmware = Some(FirmwareVersion {
                        major: be16(&value[..2]),
                        minor: be16(&value[2..4]),
                        patch: be16(&value[4..]),
                    });
                }
                (TAG_RELEASE_KEY, 8) => status.release_key_id = value.try_into().ok(),
                (TAG_OTA, 9) => {
                    status.ota_accepted_sequence = be32(&value[..4]);
                    status.ota_slot_len = be32(&value[4..8]);
                    status.nearby_only = value[8] & FLAG_NEARBY_ONLY != 0;
                    status.ota_trial_pending = value[8] & FLAG_TRIAL != 0;
                }
                (TAG_OTA_PENDING, 4) => status.ota_pending_sequence = Some(be32(value)),
                (TAG_OWNERS, 2) => {
                    status.owner_count = value[0];
                    status.role = OwnerRole::from_byte(value[1]);
                }
                (TAG_RADIO, 25) => {
                    status.radio = Some(StatusRadio {
                        frequency_hz: be32(&value[..4]),
                        bandwidth_hz: be32(&value[4..8]),
                        spreading_factor: value[8],
                        coding_rate: value[9],
                        tx_power_dbm: value[10] as i8,
                        duty_tenths_percent: be16(&value[11..13]),
                        max_transmission_ms: be32(&value[13..17]),
                        hour_used_ms: be32(&value[17..21]),
                        hour_budget_ms: be32(&value[21..]),
                    });
                }
                (TAG_INTERFACES, 1) => status.interfaces_up = value[0],
                (TAG_UPTIME, 4) => status.uptime_s = be32(value),
                (TAG_LAST_UPDATE, 22) => {
                    status.last_update = Some(LastUpdateStatus {
                        sequence: be32(&value[..4]),
                        source: UpdateSource::from_byte(value[4])?,
                        running: value[5] != 0,
                        owner: value[6..].try_into().ok()?,
                    });
                }
                (TAG_TRANSFER, 9) => {
                    status.transfer = Some(TransferStatus {
                        next_offset: be32(&value[..4]),
                        total: be32(&value[4..8]),
                        delta: value[8] != 0,
                    });
                }
                (TAG_WIFI, 6) => {
                    status.wifi = Some(WifiStatus {
                        mode: value[0],
                        service_ready: value[1] & WIFI_READY != 0,
                        ip: value[2..].try_into().ok()?,
                    });
                }
                (TAG_PATH, 2) => {
                    status.path = Some(SessionPath {
                        interface_id: value[0],
                        hops: value[1],
                    });
                }
                (TAG_IMAGE, 36) => {
                    status.image = Some(RunningImage {
                        len: be32(&value[..4]),
                        sha256: value[4..].try_into().ok()?,
                    });
                }
                (TAG_RELEASE, _) => {
                    status.release = Some(ReleaseVersion::new(core::str::from_utf8(value).ok()?)?);
                }
                (
                    TAG_BOARD | TAG_FIRMWARE | TAG_RELEASE_KEY | TAG_OTA | TAG_OTA_PENDING
                    | TAG_OWNERS | TAG_RADIO | TAG_INTERFACES | TAG_UPTIME | TAG_LAST_UPDATE
                    | TAG_TRANSFER | TAG_WIFI | TAG_PATH | TAG_IMAGE,
                    _,
                ) => return None,
                _ => {}
            }
        }
        Some(status)
    }
}

struct TlvWriter<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl TlvWriter<'_> {
    fn put(&mut self, tag: u8, value: &[u8]) -> Result<(), EncodeError> {
        let end = self.len + 2 + value.len();
        let slot = self
            .out
            .get_mut(self.len..end)
            .ok_or(EncodeError::BufferTooSmall)?;
        slot[0] = tag;
        slot[1] = value.len() as u8;
        slot[2..].copy_from_slice(value);
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_and_skips_unknown_tags() {
        let status = ManageStatus {
            board: Some(BoardTarget {
                platform: 0x80,
                mcu: 0x81,
                board: 0x3A,
            }),
            firmware: Some(FirmwareVersion {
                major: 1,
                minor: 2,
                patch: 3,
            }),
            release_key_id: Some([0xAB; 8]),
            ota_accepted_sequence: 41,
            ota_pending_sequence: Some(42),
            ota_trial_pending: true,
            ota_slot_len: 0x40_0000,
            owner_count: 2,
            role: Some(OwnerRole::Helper),
            nearby_only: true,
            radio: Some(StatusRadio {
                frequency_hz: 869_525_000,
                bandwidth_hz: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power_dbm: 14,
                duty_tenths_percent: 100,
                max_transmission_ms: 0,
                hour_used_ms: 1_234,
                hour_budget_ms: 360_000,
            }),
            interfaces_up: 0b0011,
            uptime_s: 86_400,
            last_update: Some(LastUpdateStatus {
                sequence: 41,
                source: UpdateSource::Link,
                owner: [0x0F; 16],
                running: true,
            }),
            transfer: Some(TransferStatus {
                next_offset: 12_600,
                total: 180_000,
                delta: true,
            }),
            wifi: Some(WifiStatus {
                mode: 1,
                service_ready: true,
                ip: [192, 168, 1, 40],
            }),
            path: Some(SessionPath {
                interface_id: 1,
                hops: 1,
            }),
            image: Some(RunningImage {
                len: 1_843_200,
                sha256: [0x9C; 32],
            }),
            release: ReleaseVersion::new("1.0.0-beta.20261101.1"),
        };
        let mut out = [0u8; 320];
        let len = status.encode(&mut out).unwrap();
        assert!(len < crate::frame::COMMAND_MAX_LEN - crate::frame::COMMAND_HEADER_LEN);
        assert_eq!(ManageStatus::decode(&out[..len]), Some(status));
        // A future tag is skipped; a known tag with the wrong size is refused.
        out[len..len + 3].copy_from_slice(&[0xEE, 1, 0]);
        assert_eq!(ManageStatus::decode(&out[..len + 3]), Some(status));
        assert!(ManageStatus::decode(&[TAG_UPTIME, 2, 0, 0]).is_none());
        assert!(ManageStatus::decode(&[TAG_UPTIME, 4, 0]).is_none());
        assert!(ManageStatus::decode(&[TAG_RELEASE, 0]).is_none());
        assert!(ManageStatus::decode(&[TAG_RELEASE, 2, b'1', b' ']).is_none());
        assert!(ReleaseVersion::new(&"9".repeat(RELEASE_VERSION_MAX + 1)).is_none());
        // Retired flag bits from older nodes are ignored.
        let mut legacy = [0u8; 32];
        let len = ManageStatus {
            wifi: Some(WifiStatus::default()),
            ..ManageStatus::default()
        }
        .encode(&mut legacy)
        .unwrap();
        let ota = legacy.iter().position(|&b| b == TAG_OTA).unwrap();
        legacy[ota + 2 + 8] |= 0x01;
        let wifi = legacy.iter().rposition(|&b| b == TAG_WIFI).unwrap();
        legacy[wifi + 3] |= 0x06;
        let decoded = ManageStatus::decode(&legacy[..len]).unwrap();
        assert!(!decoded.nearby_only && !decoded.ota_trial_pending);
        assert_eq!(decoded.wifi, Some(WifiStatus::default()));
        let empty = ManageStatus::default();
        let len = empty.encode(&mut out).unwrap();
        assert_eq!(ManageStatus::decode(&out[..len]), Some(empty));
        assert_eq!(
            status.encode(&mut [0u8; 64]),
            Err(EncodeError::BufferTooSmall)
        );
    }

    #[test]
    fn roles_and_sources_keep_their_bytes() {
        for role in [OwnerRole::Primary, OwnerRole::Helper] {
            assert_eq!(OwnerRole::from_byte(role.to_byte()), Some(role));
        }
        for source in [UpdateSource::Link, UpdateSource::Root, UpdateSource::Usb] {
            assert_eq!(UpdateSource::from_byte(source.to_byte()), Some(source));
        }
        assert_eq!(OwnerRole::from_byte(0), None);
        assert_eq!(UpdateSource::from_byte(4), None);
    }
}
