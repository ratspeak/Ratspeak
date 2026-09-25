//! Exercise the serialized IPC boundary, including the Base64 padding emitted
//! by WebView FileReader. Direct AppState staging tests bypass that boundary.

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use ratspeak_tauri::{commands::messaging::*, config::DashboardConfig, db, state::AppState};
use serde_json::{Value, json};
use tauri::test::{MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets};

const CHUNK_BYTES: usize = 256 * 1024;
const LEGACY_BYTES: usize = 1_000_000;

struct Fixture {
    window: tauri::WebviewWindow<MockRuntime>,
    _app: tauri::App<MockRuntime>,
    state: Arc<AppState>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = db::init_pool(dir.path()).unwrap();
        db::init_schema(&pool).unwrap();
        let state = Arc::new(AppState::new(
            DashboardConfig::from_env_and_defaults(dir.path().to_path_buf()),
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        ));
        let app = mock_builder()
            .manage(Arc::clone(&state))
            .invoke_handler(tauri::generate_handler![
                begin_attachment_stage,
                append_attachment_stage,
                cancel_attachment_stage,
                inspect_image_attachment_stage,
                prepare_image_attachment_stage,
                mark_image_attachment_stage_as_file,
                send_lxmf_with_attachment,
            ])
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        Self {
            window,
            _app: app,
            state,
            _dir: dir,
        }
    }

    fn invoke(&self, command: &str, body: Value) -> Result<Value, Value> {
        get_ipc_response(
            &self.window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(any(windows, target_os = "android")) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|response| response.deserialize().unwrap())
    }

    fn begin(&self, size: usize, image: bool) -> String {
        let result = self
            .invoke(
                "begin_attachment_stage",
                json!({"args": {
                    "file_name": if image { "photo.bmp" } else { "data.bin" },
                    "mime": if image { "image/bmp" } else { "application/octet-stream" },
                    "declared_size": size,
                    "is_image": image,
                }}),
            )
            .unwrap();
        assert_eq!(result["chunk_bytes"], CHUNK_BYTES);
        result["token"].as_str().unwrap().to_owned()
    }

    fn append(&self, token: &str, offset: usize, encoded: &str) -> Result<Value, Value> {
        self.invoke(
            "append_attachment_stage",
            json!({"args": {
                "token": token, "offset": offset, "data_base64": encoded,
            }}),
        )
    }

    fn upload(&self, bytes: &[u8], image: bool) -> String {
        let token = self.begin(bytes.len(), image);
        let mut offset = 0;
        for chunk in bytes.chunks(CHUNK_BYTES) {
            let result = self.append(&token, offset, &B64.encode(chunk)).unwrap();
            offset += chunk.len();
            assert_eq!(result["written"], offset);
        }
        token
    }

    fn cancel(&self, token: &str) {
        assert_eq!(
            self.invoke("cancel_attachment_stage", json!({"token": token}))
                .unwrap()["cancelled"],
            true
        );
    }

    fn assert_completed(&self, token: &str, expected: &[u8]) {
        let staged = self.state.take_completed_attachment_staging(token).unwrap();
        assert_eq!(staged.declared_size, expected.len());
        assert_eq!(std::fs::read(&staged.path).unwrap(), expected);
        let path = staged.path.clone();
        drop(staged);
        assert!(
            !path.exists(),
            "completed staging must release its private file"
        );
    }
}

fn bytes(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| (index.wrapping_mul(31) % 251) as u8)
        .collect()
}

#[test]
fn full_chunks_and_all_padding_lengths_round_trip_through_ipc() {
    let fixture = Fixture::new();
    for size in [
        1,
        2,
        3,
        CHUNK_BYTES - 2,
        CHUNK_BYTES - 1,
        CHUNK_BYTES,
        CHUNK_BYTES * 2,
        CHUNK_BYTES * 2 + 1,
        CHUNK_BYTES * 2 + 2,
        CHUNK_BYTES * 2 + 3,
        LEGACY_BYTES,
        LEGACY_BYTES + 1,
        6_000_000,
    ] {
        let source = bytes(size);
        let token = fixture.upload(&source, false);
        fixture.assert_completed(&token, &source);
    }
}

#[test]
fn oversized_and_malformed_chunks_never_advance_staging() {
    let fixture = Fixture::new();
    let token = fixture.begin(CHUNK_BYTES, false);
    // +1 and +2 have the SAME encoded length as a permitted full chunk.
    // The exact decoded limit must still reject them after the encoded guard.
    for size in [CHUNK_BYTES + 1, CHUNK_BYTES + 2, CHUNK_BYTES + 3] {
        let error = fixture
            .append(&token, 0, &B64.encode(bytes(size)))
            .unwrap_err();
        assert_eq!(error["message"], "Attachment chunk is too large");
    }
    for invalid in [
        "", "!", "YQ", "YQ=", "YQ===", "YR==", "YWJ=", "Y=Q=", "YQ==\n", "____",
    ] {
        let error = fixture.append(&token, 0, invalid).unwrap_err();
        assert_eq!(error["message"], "Invalid attachment chunk", "{invalid:?}");
    }
    let encoded_limit = B64.encode(bytes(CHUNK_BYTES)).len();
    assert_eq!(
        fixture
            .append(&token, 0, &"!".repeat(encoded_limit + 1))
            .unwrap_err()["message"],
        "Attachment chunk is too large",
        "oversized input must be rejected before attempting Base64 decoding"
    );
    assert_eq!(
        fixture
            .append(&token, 0, &"!".repeat(encoded_limit))
            .unwrap_err()["message"],
        "Invalid attachment chunk"
    );
    assert!(
        fixture
            .state
            .take_completed_attachment_staging(&token)
            .is_none()
    );
    let source = bytes(CHUNK_BYTES);
    assert_eq!(
        fixture.append(&token, 0, &B64.encode(&source)).unwrap()["written"],
        CHUNK_BYTES
    );
    fixture.assert_completed(&token, &source);
}

#[test]
fn chunk_offsets_declared_size_and_completion_remain_strict() {
    let fixture = Fixture::new();
    let token = fixture.begin(CHUNK_BYTES + 1, false);
    let chunk = bytes(CHUNK_BYTES);
    fixture.append(&token, 0, &B64.encode(&chunk)).unwrap();
    assert!(
        fixture
            .state
            .take_completed_attachment_staging(&token)
            .is_none()
    );
    for (offset, data) in [
        (0, "YQ=="),
        (CHUNK_BYTES - 1, "YQ=="),
        (CHUNK_BYTES + 1, "YQ=="),
        (usize::MAX, "YQ=="),
        (CHUNK_BYTES, "YWI="),
    ] {
        assert_eq!(
            fixture.append(&token, offset, data).unwrap_err()["message"],
            "Invalid attachment chunk"
        );
    }
    fixture.append(&token, CHUNK_BYTES, "YQ==").unwrap();
    let mut expected = chunk;
    expected.push(b'a');
    fixture.assert_completed(&token, &expected);
    assert_eq!(
        fixture.append(&token, 0, "YQ==").unwrap_err()["code"],
        "not_found"
    );
}

#[test]
fn cancellation_and_identity_changes_reject_late_chunks_and_release_large_lane() {
    let fixture = Fixture::new();
    let token = fixture.begin(2_000_000, false);
    fixture
        .append(&token, 0, &B64.encode(bytes(CHUNK_BYTES)))
        .unwrap();
    fixture.cancel(&token);
    assert_eq!(
        fixture.append(&token, CHUNK_BYTES, "YQ==").unwrap_err()["code"],
        "not_found"
    );
    let replacement = fixture.begin(2_000_000, false);
    fixture.state.bump_identity_session_generation();
    assert_eq!(
        fixture.append(&replacement, 0, "YQ==").unwrap_err()["message"],
        "Invalid attachment chunk"
    );
    fixture.cancel(&replacement);
    let final_token = fixture.begin(2_000_000, false);
    fixture.cancel(&final_token);
    assert_eq!(
        std::fs::read_dir(fixture.state.config.data_dir.join("attachment-staging"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn product_size_limit_and_large_transfer_admission_remain_enforced() {
    let fixture = Fixture::new();
    for size in [0, 128_000_001, usize::MAX] {
        let error = fixture.invoke("begin_attachment_stage", json!({"args": {
            "file_name": "data.bin", "mime": "application/octet-stream", "declared_size": size,
        }})).unwrap_err();
        assert_eq!(error["code"], "attachment_too_large");
    }
    // Beginning a stage reserves its declared size but does not allocate or
    // write that many bytes. The product ceiling remains inclusive.
    let token = fixture.begin(128_000_000, false);
    assert_eq!(fixture.invoke("begin_attachment_stage", json!({"args": {
        "file_name": "other.bin", "mime": "application/octet-stream", "declared_size": 2_000_000,
    }})).unwrap_err()["code"], "conflict");
    fixture.cancel(&token);
    let replacement = fixture.begin(2_000_000, false);
    fixture.cancel(&replacement);
}

// An ordinary uncompressed 24-bit BMP, large enough to cross multiple IPC
// chunks and the 1 MB image-choice threshold.
fn photo_bmp() -> Vec<u8> {
    let (width, height) = (1200u32, 900u32);
    let pixel_bytes = width * height * 3; // width is already row-aligned
    let mut bmp = Vec::with_capacity((54 + pixel_bytes) as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54 + pixel_bytes).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    bmp.resize((54 + pixel_bytes) as usize, 100);
    bmp
}

#[test]
fn multi_chunk_photo_reaches_inspection_and_every_compression_profile() {
    let fixture = Fixture::new();
    let source = photo_bmp();
    for (profile, max_edge, ceiling) in [
        ("small", 960, 250_000),
        ("medium", 1600, 750_000),
        ("large", 2560, 2_000_000),
        ("actual", 1200, 128_000_000),
    ] {
        let token = fixture.upload(&source, true);
        assert!(
            fixture
                .state
                .take_completed_attachment_staging(&token)
                .is_none(),
            "source photo must be prepared before sending"
        );
        let inspection = fixture
            .invoke(
                "inspect_image_attachment_stage",
                json!({"args": {"token": token}}),
            )
            .unwrap();
        assert_eq!(inspection["disposition"], "still");
        assert_eq!(inspection["should_prompt"], true);
        assert_eq!(inspection["source_bytes"], source.len());
        let prepared = fixture
            .invoke(
                "prepare_image_attachment_stage",
                json!({"args": {
                    "token": token, "profile": profile,
                }}),
            )
            .unwrap();
        assert_eq!(prepared["profile"], profile);
        assert_eq!(prepared["mime"], "image/jpeg");
        assert!(prepared["width"].as_u64().unwrap() <= max_edge);
        assert!(prepared["height"].as_u64().unwrap() <= max_edge);
        let size = prepared["size"].as_u64().unwrap() as usize;
        assert!(size > 0 && size <= ceiling && size < source.len());
        let staged = fixture
            .state
            .take_completed_attachment_staging(&token)
            .unwrap();
        assert_eq!(staged.declared_size, size);
        assert_eq!(std::fs::metadata(&staged.path).unwrap().len(), size as u64);
        assert!(staged.is_image);
    }
}

#[test]
fn automatic_photo_preparation_bounds_reencoded_bytes_but_explicit_actual_does_not() {
    use image::{DynamicImage, ImageBuffer, Rgb, codecs::jpeg::JpegEncoder};

    let photo = DynamicImage::ImageRgb8(ImageBuffer::from_fn(1600, 1200, |x, y| {
        Rgb([
            ((x * 17 + y * 3) % 255) as u8,
            ((x * 7 + y * 13) % 255) as u8,
            ((x * 5 + y * 19) % 255) as u8,
        ])
    }));
    let mut source = Vec::new();
    JpegEncoder::new_with_quality(&mut source, 40)
        .encode_image(&photo)
        .unwrap();
    assert!(source.len() < LEGACY_BYTES);
    let fixture = Fixture::new();
    for automatic in [true, false] {
        let token = fixture.upload(&source, true);
        let inspection = fixture
            .invoke(
                "inspect_image_attachment_stage",
                json!({"args": {"token": token}}),
            )
            .unwrap();
        assert_eq!(inspection["should_prompt"], false);
        let prepared = fixture
            .invoke(
                "prepare_image_attachment_stage",
                json!({"args": {
                    "token": token, "profile": "actual", "automatic": automatic,
                }}),
            )
            .unwrap();
        let size = prepared["size"].as_u64().unwrap() as usize;
        if automatic {
            assert_eq!(prepared["profile"], "medium");
            assert!(size <= 750_000);
        } else {
            assert_eq!(prepared["profile"], "actual");
            assert!(
                size > LEGACY_BYTES,
                "fixture must exercise re-encoding inflation"
            );
        }
        let staged = fixture
            .state
            .take_completed_attachment_staging(&token)
            .unwrap();
        assert!(staged.is_image);
        assert_eq!(staged.declared_size, size);
        let encoded = std::fs::read(&staged.path).unwrap();
        assert_eq!(encoded.len(), size);
        let decoded = image::load_from_memory(&encoded).unwrap();
        assert_eq!(decoded.width(), prepared["width"].as_u64().unwrap() as u32);
        assert_eq!(
            decoded.height(),
            prepared["height"].as_u64().unwrap() as u32
        );
        if automatic {
            assert!(decoded.width() <= 1600 && decoded.height() <= 1200);
        } else {
            assert_eq!((decoded.width(), decoded.height()), (1600, 1200));
        }
    }
}

#[test]
fn automatic_photo_preparation_keeps_actual_when_it_fits_and_rejects_invalid_choices() {
    let fixture = Fixture::new();
    let token = fixture.upload(&photo_bmp(), true);
    let error = fixture
        .invoke(
            "prepare_image_attachment_stage",
            json!({"args": {
                "token": token, "profile": "medium", "automatic": true,
            }}),
        )
        .unwrap_err();
    assert_eq!(error["code"], "bad_request");
    // Rejection must leave the source usable for a valid preparation.
    let prepared = fixture
        .invoke(
            "prepare_image_attachment_stage",
            json!({"args": {
                "token": token, "profile": "actual", "automatic": true,
            }}),
        )
        .unwrap();
    assert_eq!(prepared["profile"], "actual");
    assert_eq!(prepared["width"], 1200);
    assert_eq!(prepared["height"], 900);
    assert!(prepared["size"].as_u64().unwrap() <= 750_000);
    fixture.cancel(&token);
}

#[test]
fn unsupported_multi_chunk_image_can_still_be_sent_as_a_file() {
    let fixture = Fixture::new();
    let source = bytes(CHUNK_BYTES + 1);
    let token = fixture.upload(&source, true);
    let inspection = fixture
        .invoke(
            "inspect_image_attachment_stage",
            json!({"args": {"token": token}}),
        )
        .unwrap();
    assert_eq!(inspection["disposition"], "unsupported");
    fixture
        .invoke(
            "mark_image_attachment_stage_as_file",
            json!({"args": {"token": token}}),
        )
        .unwrap();
    fixture.assert_completed(&token, &source);
}

#[test]
fn legacy_file_and_image_limits_allow_exactly_one_mb_but_no_extra_bytes() {
    let fixture = Fixture::new();
    for field in ["file_data", "image_data"] {
        for size in (LEGACY_BYTES - 2)..=(LEGACY_BYTES + 3) {
            let mut args = json!({"dest_hash": "ab".repeat(16), "delivery_method": "direct"});
            args[field] = json!(B64.encode(bytes(size)));
            let error = fixture
                .invoke("send_lxmf_with_attachment", json!({"args": args}))
                .unwrap_err();
            // There is deliberately no network manager: accepted input reaches
            // the queue and reports its absence, rather than a decoding error.
            assert_eq!(
                error["code"],
                if size <= LEGACY_BYTES {
                    "lxmf_not_initialized"
                } else {
                    "attachment_too_large"
                },
                "{field}, {size}: {error}"
            );
        }
    }
    for (input, code) in [
        ("", "attachment_missing"),
        ("YR==", "attachment_invalid"),
        ("YQ", "attachment_invalid"),
    ] {
        assert_eq!(
            fixture
                .invoke(
                    "send_lxmf_with_attachment",
                    json!({"args": {
                        "dest_hash": "ab".repeat(16), "file_data": input,
                    }})
                )
                .unwrap_err()["code"],
            code
        );
    }
    // All rejected/unqueued sends must release their pre-decode memory lease.
    let leases: Vec<_> = (0..8)
        .map(|_| {
            fixture
                .state
                .reserve_attachment_transfer(1_000_000)
                .unwrap()
        })
        .collect();
    assert_eq!(
        fixture
            .invoke(
                "send_lxmf_with_attachment",
                json!({"args": {
                    "dest_hash": "ab".repeat(16), "file_data": B64.encode(bytes(LEGACY_BYTES)),
                }})
            )
            .unwrap_err()["code"],
        "attachment_memory_pressure"
    );
    drop(leases);
    assert_eq!(
        fixture
            .invoke(
                "send_lxmf_with_attachment",
                json!({"args": {
                    "dest_hash": "ab".repeat(16), "file_data": B64.encode(bytes(LEGACY_BYTES)),
                }})
            )
            .unwrap_err()["code"],
        "lxmf_not_initialized"
    );
}
