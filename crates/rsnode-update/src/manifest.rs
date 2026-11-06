//! The 128-byte `RSOT` manifest that leads a `.rsota` bundle, followed by its image.
//!
//! Layout, big-endian: `"RSOT" version(1) platform(1) mcu(1) board(1) sequence(4) image_len(4)
//! major(2) minor(2) patch(2) zero(2) image_sha256(32) zero(8)`, then a 64-byte Ed25519
//! signature over those first 64 bytes. Parsing checks shape only: the node verifies the
//! signature against its release key, the target, the sequence floor and the image digest.

pub const OTA_MANIFEST_LEN: usize = 128;
pub const OTA_SIGNED_HEADER_LEN: usize = 64;
pub const OTA_SIGNATURE_LEN: usize = 64;

const MANIFEST_MAGIC: [u8; 4] = *b"RSOT";
const MANIFEST_VERSION: u8 = 1;

/// The board an image is built for, as `platform mcu board` bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoardTarget {
    pub platform: u8,
    pub mcu: u8,
    pub board: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct FirmwareVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestError {
    InvalidLength,
    /// Wrong magic or version, or nonzero reserved bytes.
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OtaManifest {
    pub target: BoardTarget,
    pub sequence: u32,
    pub image_len: u32,
    pub firmware_version: FirmwareVersion,
    pub image_sha256: [u8; 32],
    pub signature: [u8; OTA_SIGNATURE_LEN],
}

impl OtaManifest {
    /// Decode the 128-byte version-1 record, checking magic and zero reserved bytes.
    /// Verify the signature before trusting any field.
    pub fn decode(bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() != OTA_MANIFEST_LEN {
            return Err(ManifestError::InvalidLength);
        }
        if bytes[..4] != MANIFEST_MAGIC || bytes[4] != MANIFEST_VERSION {
            return Err(ManifestError::Invalid);
        }
        if bytes[22..24] != [0; 2] || bytes[56..OTA_SIGNED_HEADER_LEN] != [0; 8] {
            return Err(ManifestError::Invalid);
        }

        let mut image_sha256 = [0u8; 32];
        image_sha256.copy_from_slice(&bytes[24..56]);
        let mut signature = [0u8; OTA_SIGNATURE_LEN];
        signature.copy_from_slice(&bytes[OTA_SIGNED_HEADER_LEN..OTA_MANIFEST_LEN]);
        Ok(Self {
            target: BoardTarget {
                platform: bytes[5],
                mcu: bytes[6],
                board: bytes[7],
            },
            sequence: crate::be32(&bytes[8..]),
            image_len: crate::be32(&bytes[12..]),
            firmware_version: FirmwareVersion {
                major: crate::be16(&bytes[16..]),
                minor: crate::be16(&bytes[18..]),
                patch: crate::be16(&bytes[20..]),
            },
            image_sha256,
            signature,
        })
    }

    /// Serialize the current fields and signature without validating or signing them.
    pub fn encode(&self) -> [u8; OTA_MANIFEST_LEN] {
        let mut out = self.signed_header();
        let mut manifest = [0u8; OTA_MANIFEST_LEN];
        manifest[..OTA_SIGNED_HEADER_LEN].copy_from_slice(&out);
        manifest[OTA_SIGNED_HEADER_LEN..].copy_from_slice(&self.signature);
        out.fill(0);
        manifest
    }

    /// Serialize the canonical 64 bytes covered by the signature, with zero padding.
    pub fn signed_header(&self) -> [u8; OTA_SIGNED_HEADER_LEN] {
        let mut out = [0u8; OTA_SIGNED_HEADER_LEN];
        out[..4].copy_from_slice(&MANIFEST_MAGIC);
        out[4] = MANIFEST_VERSION;
        out[5] = self.target.platform;
        out[6] = self.target.mcu;
        out[7] = self.target.board;
        out[8..12].copy_from_slice(&self.sequence.to_be_bytes());
        out[12..16].copy_from_slice(&self.image_len.to_be_bytes());
        out[16..18].copy_from_slice(&self.firmware_version.major.to_be_bytes());
        out[18..20].copy_from_slice(&self.firmware_version.minor.to_be_bytes());
        out[20..22].copy_from_slice(&self.firmware_version.patch.to_be_bytes());
        out[24..56].copy_from_slice(&self.image_sha256);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> OtaManifest {
        OtaManifest {
            target: BoardTarget {
                platform: 0x80,
                mcu: 0x81,
                board: 0x3A,
            },
            sequence: 9,
            image_len: 4_096,
            firmware_version: FirmwareVersion {
                major: 1,
                minor: 2,
                patch: 3,
            },
            image_sha256: [0x11; 32],
            signature: [0x22; OTA_SIGNATURE_LEN],
        }
    }

    #[test]
    fn manifests_round_trip_and_reject_bad_shape() {
        let encoded = manifest().encode();
        assert_eq!(OtaManifest::decode(&encoded), Ok(manifest()));
        assert_eq!(
            encoded[..OTA_SIGNED_HEADER_LEN],
            manifest().signed_header()[..]
        );
        assert_eq!(
            OtaManifest::decode(&[0; OTA_MANIFEST_LEN - 1]),
            Err(ManifestError::InvalidLength)
        );
        for (index, byte) in [(0, b'X'), (4, 2), (22, 1), (23, 1), (56, 1), (63, 1)] {
            let mut bytes = encoded;
            bytes[index] = byte;
            assert_eq!(OtaManifest::decode(&bytes), Err(ManifestError::Invalid));
        }
    }
}
