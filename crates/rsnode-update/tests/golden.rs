//! Byte-exact vectors from the rsNode 1.0.0 codec. A change here breaks deployed nodes.

use rsnode_update::delta::{DeltaApplier, DeltaIo};
use rsnode_update::frame::{
    COMMAND_HEADER_LEN, COMMAND_MAX_LEN, Command, CommandResult, DeltaBase, LINK_COMMAND_VERSION,
    OTA_BEGIN_DELTA_LEN, OTA_CHUNK_DATA_MAX, Request, decode_offset, decode_request,
    decode_response, encode_offset, encode_request, encode_response_header,
};
use rsnode_update::manifest::{BoardTarget, FirmwareVersion, OTA_MANIFEST_LEN, OtaManifest};
use rsnode_update::status::{
    LastUpdateStatus, ManageStatus, OwnerRole, ReleaseVersion, RunningImage, SessionPath,
    StatusRadio, TransferStatus, UpdateSource, WifiStatus,
};

fn hex(parts: &[&str]) -> Vec<u8> {
    let text: String = parts.concat();
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

const TARGET: BoardTarget = BoardTarget {
    platform: 0x80,
    mcu: 0x81,
    board: 0x3A,
};

fn encoded(request: Request<'_>) -> Vec<u8> {
    let mut out = [0u8; COMMAND_MAX_LEN];
    let len = encode_request(&request, &mut out).unwrap();
    assert_eq!(decode_request(&out[..len]), Ok(request));
    out[..len].to_vec()
}

#[test]
fn limits() {
    assert_eq!(
        (
            LINK_COMMAND_VERSION,
            COMMAND_HEADER_LEN,
            COMMAND_MAX_LEN,
            OTA_CHUNK_DATA_MAX,
            OTA_BEGIN_DELTA_LEN,
            OTA_MANIFEST_LEN,
        ),
        (1, 6, 431, 420, 168, 128)
    );
}

#[test]
fn transfer_requests() {
    let id = 0x0102_0304;
    let request = |command| Request { id, command };
    assert_eq!(encoded(request(Command::Status)), hex(&["010102030401"]));
    assert_eq!(
        encoded(request(Command::OtaChunk {
            ack: true,
            offset: 4_096,
            data: &[0xC7, 0x00, 0xFF],
        })),
        hex(&["010102030409", "01", "00001000", "c700ff"])
    );
    assert_eq!(
        encoded(request(Command::OtaChunk {
            ack: false,
            offset: 0x0A0B_0C0D,
            data: &[0x42],
        })),
        hex(&["010102030409", "00", "0a0b0c0d", "42"])
    );
    assert_eq!(encoded(request(Command::OtaFinish)), hex(&["01010203040a"]));
    assert_eq!(encoded(request(Command::OtaAbort)), hex(&["01010203040b"]));

    let manifest = [0x5A; OTA_MANIFEST_LEN];
    let full = encoded(Request {
        id: 7,
        command: Command::OtaBegin {
            manifest: &manifest,
            delta: None,
        },
    });
    assert_eq!(full[..6], hex(&["010000000708"])[..]);
    assert_eq!(full[6..], manifest[..]);
    let delta = encoded(Request {
        id: 7,
        command: Command::OtaBegin {
            manifest: &manifest,
            delta: Some(DeltaBase {
                base_len: 1_800_000,
                base_sha256: [3; 32],
                patch_len: 180_000,
            }),
        },
    });
    assert_eq!(delta[..6 + OTA_MANIFEST_LEN], full[..]);
    assert_eq!(
        delta[6 + OTA_MANIFEST_LEN..],
        hex(&[
            "001b7740",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "0002bf20",
        ])[..]
    );
}

#[test]
fn replies() {
    let mut out = [0u8; 8];
    let len = encode_response_header(9, CommandResult::Gap, &mut out).unwrap();
    assert_eq!(out[..len], hex(&["01000000090b"])[..]);
    assert_eq!(
        decode_response(&out[..len]),
        Some((9, CommandResult::Gap, &[][..]))
    );
    let len = encode_offset(8_192, Some(420), &mut out).unwrap();
    assert_eq!(out[..len], hex(&["00002000", "01a4"])[..]);
    assert_eq!(decode_offset(&out[..len]), Some((8_192, Some(420))));
    let len = encode_offset(7, None, &mut out).unwrap();
    assert_eq!(out[..len], hex(&["00000007"])[..]);
    let results = [
        CommandResult::Ok,
        CommandResult::Denied,
        CommandResult::Invalid,
        CommandResult::Full,
        CommandResult::NotFound,
        CommandResult::Busy,
        CommandResult::Unsupported,
        CommandResult::Storage,
        CommandResult::Rejected,
        CommandResult::Refused,
        CommandResult::Unavailable,
        CommandResult::Gap,
    ];
    for (byte, result) in results.into_iter().enumerate() {
        assert_eq!(result as u8, byte as u8);
    }
}

#[test]
fn status() {
    let status = ManageStatus {
        board: Some(TARGET),
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
            tx_power_dbm: -3,
            duty_tenths_percent: 100,
            max_transmission_ms: 4_000,
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
            hops: 2,
        }),
        image: Some(RunningImage {
            len: 1_843_200,
            sha256: [0x9C; 32],
        }),
        release: ReleaseVersion::new("1.0.0-beta.20261101.1"),
    };
    let expected = hex(&[
        "010380813a",
        "0206000100020003",
        "0308abababababababab",
        "0409000000290040000006",
        "05040000002a",
        "06020202",
        "071933d3e6080003d0900905fd006400000fa0000004d200057e40",
        "080103",
        "090400015180",
        "0a160000002901010f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
        "0b09000031380002bf2001",
        "0c060101c0a80128",
        "0d020102",
        "0e24001c2000",
        "9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c",
        "0f15312e302e302d626574612e32303236313130312e31",
    ]);
    let mut out = [0u8; 400];
    let len = status.encode(&mut out).unwrap();
    assert_eq!(out[..len], expected[..]);
    assert_eq!(ManageStatus::decode(&expected), Some(status));

    let empty = hex(&[
        "0409000000000000000000",
        "06020000",
        "080100",
        "090400000000",
    ]);
    let len = ManageStatus::default().encode(&mut out).unwrap();
    assert_eq!(out[..len], empty[..]);
    assert_eq!(ManageStatus::decode(&[]), Some(ManageStatus::default()));
}

#[test]
fn manifest() {
    let mut signature = [0u8; 64];
    for (index, byte) in signature.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let manifest = OtaManifest {
        target: TARGET,
        sequence: 0x0102_0304,
        image_len: 1_843_200,
        firmware_version: FirmwareVersion {
            major: 1,
            minor: 0,
            patch: 7,
        },
        image_sha256: [0x9C; 32],
        signature,
    };
    let expected = hex(&[
        "52534f5401",
        "80813a",
        "01020304",
        "001c2000",
        "000100000007",
        "0000",
        "9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c9c",
        "0000000000000000",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
    ]);
    assert_eq!(manifest.encode()[..], expected[..]);
    assert_eq!(manifest.signed_header()[..], expected[..64]);
    assert_eq!(OtaManifest::decode(&expected), Ok(manifest));
}

/// A manifest signed by rsNode's release packager (`scripts/package_ota.py`).
#[test]
fn release_packager_manifest() {
    let encoded = hex(&[
        "52534f540180813a000000090000000900010002000300009001597fc46093893f",
        "9bd86e2a1eb73698e82565273fd5643765534df06af2ca0000000000000000df12",
        "098461f4e1e8b75a7a88b1f9bc88cec657d51c2800b2c38217e2d0ab4b547097",
        "e1c7121fe31202122000ed817750a71727b6d4a05651810e443bedaf6c08",
    ]);
    let manifest = OtaManifest::decode(&encoded).unwrap();
    assert_eq!(manifest.target, TARGET);
    assert_eq!((manifest.sequence, manifest.image_len), (9, 9));
    assert_eq!(
        manifest.firmware_version,
        FirmwareVersion {
            major: 1,
            minor: 2,
            patch: 3
        }
    );
    assert_eq!(manifest.encode()[..], encoded[..]);
}

/// A patch made by the 1.0.0 `rsnode-client` encoder.
const PATCH: &str = "5253445001000000d3000000210000000000000080d9200002595c0e0000368141ee573a75bec9659086fa0007302825d2c369b600";

fn delta_base() -> Vec<u8> {
    (0..200u32).map(|index| (index * 7 % 251) as u8).collect()
}

fn delta_target() -> Vec<u8> {
    let mut target = delta_base();
    target[40] ^= 0x55;
    target[41] = target[41].wrapping_add(3);
    target.splice(100..100, b"rsNode!".iter().copied());
    target.extend_from_slice(b"tail");
    target
}

struct VecIo<'a> {
    base: &'a [u8],
    out: Vec<u8>,
}

impl DeltaIo for VecIo<'_> {
    type Error = ();

    fn read_base(&mut self, offset: u32, buffer: &mut [u8]) -> Result<(), ()> {
        let at = offset as usize;
        buffer.copy_from_slice(self.base.get(at..at + buffer.len()).ok_or(())?);
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), ()> {
        self.out.extend_from_slice(bytes);
        Ok(())
    }
}

#[test]
fn delta_patch() {
    let (base, target, patch) = (delta_base(), delta_target(), hex(&[PATCH]));
    for step in [1, 7, patch.len()] {
        let mut applier = DeltaApplier::new(target.len() as u32, base.len() as u32);
        let mut io = VecIo {
            base: &base,
            out: Vec::new(),
        };
        for piece in patch.chunks(step) {
            applier.push(piece, &mut io).unwrap();
        }
        assert!(applier.complete());
        assert_eq!(io.out, target);
    }
    #[cfg(feature = "alloc")]
    assert_eq!(rsnode_update::delta::create(&base, &target), patch);
}
