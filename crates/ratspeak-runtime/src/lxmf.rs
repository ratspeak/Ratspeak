//! LXMF manager: identity, message send/receive, contacts.
//! `&DbPool` functions are sync; wrap in `db::spawn_db` from async.

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{Value, json};

use lxmf_core::admission::{AdmissionEpochs, DownloadedPropagationMessage, SuppressionLedger};
use lxmf_core::constants::{
    DELIVERY_RETRY_WAIT, ENCRYPTED_PACKET_MAX_CONTENT, MAX_DELIVERY_ATTEMPTS, MAX_PATHLESS_TRIES,
    PATH_REQUEST_WAIT, STRUCT_OVERHEAD, TIMESTAMP_SIZE,
};
use lxmf_core::delivery_ratchet::{
    DeliveryAnnounceKind, DeliveryRatchetError, DeliveryRatchetState,
};
use lxmf_core::handlers::CompressionSupport;
use lxmf_core::link_delivery::{
    BackchannelSendCommand, BackchannelSendError, BackchannelSendReceipt, DeliveryResult,
    DeliveryState, DirectLinkStartKind, LxmfDeliveryEvent, LxmfDeliveryEventKind,
    LxmfDeliveryEventMethod, is_retryable_link_delivery_failure,
};
use lxmf_core::message::{LxMessageDraft, MessageError, MessageMetadata, SignedLxMessage};
use lxmf_core::message_api::{DeliveryMethod, DeliveryRepresentation, LxMessage};
use lxmf_core::propagation_send::{
    AdvertisedNodePolicy, PropagationEnvelope, propagation_readiness,
};
use lxmf_core::router::{
    DirectDeliveryPlan, DirectDeliveryPlanInput, DirectReusableLinkState, DirectRouteSnapshot,
    LxmRouter, OutboundAction, RouterConfig, RouterStateSnapshot, plan_direct_delivery,
};
use rns_identity::destination::Destination;
use rns_identity::identity::Identity;
use rns_identity::persistence::{
    DurableFileReceipt, durable_create_dir_all, durable_reconcile, durable_remove,
    durable_rename_directory, durable_write, read_file_bounded, validate_durable_storage_directory,
};
use rns_identity::ratchet::ReceivedRatchet;

use rns_transport::messages::{PathTableRpcEntry, TransportMessage};
use tokio::sync::{mpsc, oneshot};

use crate::db;
use crate::state::DbPool;
use ratspeak_core::{LXMF_DELIVERY_APP_NAME as LXMF_APP_NAME, LXMF_PROPAGATION_APP_NAME};

const MAX_LXMF_RESOURCE_BYTES: usize = rns_protocol::resource::MAX_RESOURCE_SIZE;
pub const MAX_AUDIO_FIELD_BYTES: usize = 1_000_000;
pub const AUDIO_MESSAGE_FILE_NAME: &str = "Voice message.opus";
pub const COMPACT_AUDIO_FILE_NAME: &str = "Voice message.c2raw";
const AUTO_PROPAGATION_CHECK_INTERVAL_SECS: f64 = 5.0 * 60.0;
const BACKCHANNEL_COMMAND_BUFFER: usize = 64;
mod packet_delivery;
mod preparation;
mod recovery;
mod timing;
const DIRECT_BACKCHANNEL_IDENTIFY_GRACE: Duration = Duration::from_secs(3);
type DirectInboundResourceAcceptHandler =
    Arc<dyn Fn([u8; 16], &rns_protocol::resource_adv::ResourceAdvertisement) -> bool + Send + Sync>;
type DirectInboundResourceConcludedHandler = Arc<dyn Fn([u8; 16], [u8; 32]) + Send + Sync>;
type DirectInboundResourceCompletionHandler =
    Arc<dyn Fn([u8; 16], [u8; 32], Vec<u8>) + Send + Sync>;
// Wire tag for the original (v1) Ratspeak chat extension. Kept for inbound
// decoding of messages from peers that haven't upgraded.
pub const RATSPEAK_CHAT_CUSTOM_TYPE_V1: &[u8] = b"ratspeak.chat.v1";
// v2 wire tag — same envelope, but hash-bearing fields (target, target_sender)
// are encoded as 32 raw bytes instead of 64-char hex strings (~32 B saved
// per reference). Emitted whenever the target hex-decodes cleanly; v1 still
// emitted as a fallback when the id isn't a 32-byte hash (UUID case).
pub const RATSPEAK_CHAT_CUSTOM_TYPE_V2: &[u8] = b"ratspeak.chat.v2";
pub const LEGACY_RATSPEAK_REACTION_TYPE: &[u8] = b"ratspeak.reaction";

// Back-compat alias for callers (tests, source-contract guards) that still
// reference the old single-version constant.
pub const RATSPEAK_CHAT_CUSTOM_TYPE: &[u8] = RATSPEAK_CHAT_CUSTOM_TYPE_V1;
// TRANSITION: replies/reactions now emit the standard LXMF 1.0.1 fields
// (FIELD_REPLY_TO/FIELD_REPLY_QUOTE/FIELD_REACTION) as primary; the legacy
// 0xFB/0xFC envelope is dual-emitted because shipped v1.0.x clients only
// decode the envelope. Flip to false / remove in a future release once the
// fleet reads the standard fields.
pub const EMIT_LEGACY_REPLY_REACTION_ENVELOPE: bool = true;
pub const RATSPEAK_ANNOUNCE_EXTENSION_VERSION: u8 = 1;
pub const RATSPEAK_CAP_CLIENT: u64 = 0x01;
pub const RATSPEAK_CAP_GAMES: u64 = 0x02;
pub const RATSPEAK_CAP_CHAT: u64 = 0x04;
pub const RATSPEAK_DEFAULT_CAPABILITIES: u64 =
    RATSPEAK_CAP_CLIENT | RATSPEAK_CAP_GAMES | RATSPEAK_CAP_CHAT;

#[derive(Debug)]
pub(crate) enum CoordinatedDeliveryAnnounceError {
    Coalesced,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatspeakChatExtension {
    Reaction {
        target: String,
        emoji: String,
        action: String,
    },
    Reply {
        target: String,
        preview: String,
        target_sender: Option<String>,
    },
}

fn encode_msgpack_value(value: &rmpv::Value) -> Vec<u8> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, value).expect("writing msgpack to Vec cannot fail");
    buf
}

/// FIELD_RENDERER is a native MessagePack integer on the wire, never bin.
fn renderer_custom_field(renderer: u8) -> (u8, Vec<u8>) {
    (
        lxmf_core::constants::FIELD_RENDERER,
        encode_msgpack_value(&rmpv::Value::from(renderer)),
    )
}

/// Inbound FIELD_THREAD as lowercase hex; Main (`""`) unless it is 1..=64 bytes.
pub(crate) fn message_thread(msg: &LxMessage) -> String {
    msg.get_field(lxmf_core::constants::FIELD_THREAD)
        .and_then(|field| field.as_binary())
        .filter(|bytes| (1..=64).contains(&bytes.len()))
        .map(hex::encode)
        .unwrap_or_default()
}

/// Outbound FIELD_THREAD (bin); none for Main. Callers pass validated hex.
fn thread_custom_field(thread: &str) -> Option<(u8, Vec<u8>)> {
    if thread.is_empty() {
        return None;
    }
    hex::decode(thread)
        .ok()
        .map(|bytes| (lxmf_core::constants::FIELD_THREAD, bytes))
}

/// Inbound FIELD_RENDERER; only an unsigned MessagePack integer in 0..=255 counts.
pub(crate) fn message_renderer(msg: &LxMessage) -> Option<u8> {
    renderer_from_encoded(
        msg.get_field(lxmf_core::constants::FIELD_RENDERER)?
            .encoded(),
    )
}

fn renderer_from_encoded(mut encoded: &[u8]) -> Option<u8> {
    // Integer markers only, so no other value shape is ever decoded.
    if !matches!(encoded.first()?, 0x00..=0x7f | 0xcc..=0xcf | 0xd0..=0xd3) {
        return None;
    }
    let value = rmpv::decode::read_value(&mut encoded).ok()?;
    if !encoded.is_empty() {
        return None;
    }
    u8::try_from(value.as_u64()?).ok()
}

pub fn get_ratspeak_announce_app_data(
    display_name: Option<&str>,
    stamp_cost: Option<u8>,
    status: Option<&str>,
    announce_ratspeak_usage: bool,
) -> Vec<u8> {
    use rmpv::Value;

    if !announce_ratspeak_usage {
        return lxmf_core::handlers::get_announce_app_data(display_name, stamp_cost);
    }

    let name_val = match display_name {
        Some(name) => Value::Binary(name.as_bytes().to_vec()),
        None => Value::Nil,
    };
    let cost_val = match stamp_cost {
        Some(cost) if cost > 0 && cost < 255 => Value::from(cost as u64),
        _ => Value::Nil,
    };
    let features = Value::Array(vec![Value::from(
        lxmf_core::constants::SF_COMPRESSION as u64,
    )]);
    let ratspeak = Value::Array(vec![
        Value::from(RATSPEAK_ANNOUNCE_EXTENSION_VERSION as u64),
        Value::from(RATSPEAK_DEFAULT_CAPABILITIES),
        Value::Map(vec![(
            Value::String("s".into()),
            Value::String(
                crate::helpers::sanitize_announced_status(status.unwrap_or(""))
                    .unwrap_or_default()
                    .into(),
            ),
        )]),
    ]);

    encode_msgpack_value(&Value::Array(vec![name_val, cost_val, features, ratspeak]))
}

fn msgpack_text_value(value: &rmpv::Value) -> Option<String> {
    match value {
        rmpv::Value::String(s) => s.as_str().map(ToOwned::to_owned),
        rmpv::Value::Binary(bytes) => String::from_utf8(bytes.clone()).ok(),
        _ => None,
    }
}

pub fn ratspeak_capability_services_from_app_data(data: &[u8]) -> Vec<&'static str> {
    let Ok(value) = rmpv::decode::read_value(&mut &data[..]) else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let Some(ext) = arr.get(3).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    if ext.first().and_then(|v| v.as_u64()) != Some(RATSPEAK_ANNOUNCE_EXTENSION_VERSION as u64) {
        return Vec::new();
    }
    let Some(bits) = ext.get(1).and_then(|v| v.as_u64()) else {
        return Vec::new();
    };
    if bits & RATSPEAK_CAP_CLIENT == 0 {
        return Vec::new();
    }

    let mut services = vec![db::PEER_SERVICE_RATSPEAK_CLIENT];
    if bits & RATSPEAK_CAP_GAMES != 0 {
        services.push(db::PEER_SERVICE_RATSPEAK_GAMES);
    }
    if bits & RATSPEAK_CAP_CHAT != 0 {
        services.push(db::PEER_SERVICE_RATSPEAK_CHAT);
    }
    services
}

pub fn ratspeak_status_from_app_data(data: &[u8]) -> Option<String> {
    let Ok(value) = rmpv::decode::read_value(&mut &data[..]) else {
        return None;
    };
    let arr = value.as_array()?;
    let ext = arr.get(3)?.as_array()?;
    if ext.first().and_then(|v| v.as_u64()) != Some(RATSPEAK_ANNOUNCE_EXTENSION_VERSION as u64) {
        return None;
    }
    let rmpv::Value::Map(profile) = ext.get(2)? else {
        return None;
    };
    let status = profile.iter().find_map(|(key, value)| {
        if msgpack_text_value(key).as_deref() == Some("s") {
            msgpack_text_value(value)
        } else {
            None
        }
    })?;
    crate::helpers::sanitize_announced_status(&status).ok()
}

fn chat_map_entry(key: &str, value: rmpv::Value) -> (rmpv::Value, rmpv::Value) {
    (rmpv::Value::String(key.into()), value)
}

fn chat_custom_type(msg: &LxMessage) -> Option<&[u8]> {
    msg.fields()
        .get(lxmf_core::constants::FIELD_CUSTOM_TYPE)?
        .as_binary()
}

// Try to interpret an id string as a 64-char hex-encoded 32-byte hash. Used
// to pick the v2 (raw bytes) vs v1 (string) wire shape — UUIDs and other
// non-hash ids fall through to the v1 fallback path.
fn id_as_32_bytes(id: &str) -> Option<[u8; 32]> {
    let decoded = hex::decode(id).ok()?;
    decoded.try_into().ok()
}

fn encode_chat_extension_data_v1(ext: &RatspeakChatExtension) -> Option<Vec<u8>> {
    // `v` + `type` previously written here are redundant with the 0xFB
    // custom-type namespace tag and ignored by every decoder. Omitted to
    // save ~25 bytes per message.
    let mut entries: Vec<(rmpv::Value, rmpv::Value)> = Vec::new();
    match ext {
        RatspeakChatExtension::Reaction {
            target,
            emoji,
            action,
        } => {
            entries.push(chat_map_entry(
                "kind",
                rmpv::Value::String("reaction".into()),
            ));
            entries.push(chat_map_entry(
                "target",
                rmpv::Value::String(target.as_str().into()),
            ));
            entries.push(chat_map_entry(
                "emoji",
                rmpv::Value::String(emoji.as_str().into()),
            ));
            entries.push(chat_map_entry(
                "action",
                rmpv::Value::String(action.as_str().into()),
            ));
        }
        RatspeakChatExtension::Reply {
            target,
            preview,
            target_sender,
        } => {
            entries.push(chat_map_entry("kind", rmpv::Value::String("reply".into())));
            entries.push(chat_map_entry(
                "target",
                rmpv::Value::String(target.as_str().into()),
            ));
            entries.push(chat_map_entry(
                "preview",
                rmpv::Value::String(preview.as_str().into()),
            ));
            if let Some(sender) = target_sender {
                entries.push(chat_map_entry(
                    "target_sender",
                    rmpv::Value::String(sender.as_str().into()),
                ));
            }
        }
    }

    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &rmpv::Value::Map(entries)).ok()?;
    Some(bytes)
}

fn encode_chat_extension_data_v2(ext: &RatspeakChatExtension) -> Option<Vec<u8>> {
    let mut entries: Vec<(rmpv::Value, rmpv::Value)> = Vec::new();
    match ext {
        RatspeakChatExtension::Reaction {
            target,
            emoji,
            action,
        } => {
            let target_bytes = id_as_32_bytes(target)?;
            entries.push(chat_map_entry(
                "kind",
                rmpv::Value::String("reaction".into()),
            ));
            entries.push(chat_map_entry(
                "target",
                rmpv::Value::Binary(target_bytes.to_vec()),
            ));
            entries.push(chat_map_entry(
                "emoji",
                rmpv::Value::String(emoji.as_str().into()),
            ));
            entries.push(chat_map_entry(
                "action",
                rmpv::Value::String(action.as_str().into()),
            ));
        }
        RatspeakChatExtension::Reply {
            target,
            preview,
            target_sender,
        } => {
            let target_bytes = id_as_32_bytes(target)?;
            // If target_sender is provided, it must also be a hash — otherwise
            // fall through to v1 (we don't want a half-binary, half-string
            // shape on the wire).
            let target_sender_bytes = match target_sender {
                Some(sender) => Some(id_as_32_bytes(sender)?),
                None => None,
            };
            entries.push(chat_map_entry("kind", rmpv::Value::String("reply".into())));
            entries.push(chat_map_entry(
                "target",
                rmpv::Value::Binary(target_bytes.to_vec()),
            ));
            entries.push(chat_map_entry(
                "preview",
                rmpv::Value::String(preview.as_str().into()),
            ));
            if let Some(sender_bytes) = target_sender_bytes {
                entries.push(chat_map_entry(
                    "target_sender",
                    rmpv::Value::Binary(sender_bytes.to_vec()),
                ));
            }
        }
    }

    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &rmpv::Value::Map(entries)).ok()?;
    Some(bytes)
}

// FIELD_REACTION 0x40 = {REACTION_TO: bin(32-byte hash), REACTION_CONTENT:
// bin(UTF-8)} — a native msgpack dict, byte-exact with Python's packb.
fn encode_standard_reaction_field(target: &[u8; 32], emoji: &str) -> Vec<u8> {
    encode_msgpack_value(&rmpv::Value::Map(vec![
        (
            rmpv::Value::from(lxmf_core::constants::REACTION_TO as u64),
            rmpv::Value::Binary(target.to_vec()),
        ),
        (
            rmpv::Value::from(lxmf_core::constants::REACTION_CONTENT as u64),
            rmpv::Value::Binary(emoji.as_bytes().to_vec()),
        ),
    ]))
}

// Standard LXMF 1.0.1 fields: FIELD_REPLY_TO 0x30 = bin(full LXMessage hash),
// FIELD_REPLY_QUOTE 0x31 = bin(quoted content UTF-8), FIELD_REACTION 0x40 =
// dict (see above). Empty when the target id isn't a 32-byte hash (UUID
// stand-in case) or the reaction is a removal — the standard field has no
// remove semantics, so removals stay envelope-only rather than showing
// standard peers a phantom add.
fn standard_chat_fields(ext: &RatspeakChatExtension) -> Vec<(u8, Vec<u8>)> {
    let mut fields = Vec::new();
    match ext {
        RatspeakChatExtension::Reaction {
            target,
            emoji,
            action,
        } => {
            if let Some(hash) = id_as_32_bytes(target).filter(|_| action == "add") {
                fields.push((
                    lxmf_core::constants::FIELD_REACTION,
                    encode_standard_reaction_field(&hash, emoji),
                ));
            }
        }
        RatspeakChatExtension::Reply {
            target, preview, ..
        } => {
            if let Some(hash) = id_as_32_bytes(target) {
                fields.push((lxmf_core::constants::FIELD_REPLY_TO, hash.to_vec()));
                if !preview.is_empty() {
                    fields.push((
                        lxmf_core::constants::FIELD_REPLY_QUOTE,
                        preview.as_bytes().to_vec(),
                    ));
                }
            }
        }
    }
    fields
}

pub fn ratspeak_chat_custom_fields(ext: &RatspeakChatExtension) -> Option<Vec<(u8, Vec<u8>)>> {
    let mut fields = standard_chat_fields(ext);
    if EMIT_LEGACY_REPLY_REACTION_ENVELOPE {
        // Envelope prefers v2 (~32 B saved per hash reference). Fall back to
        // v1 when the ids in `ext` aren't 32-byte hashes — covers the
        // Historical outbound rows sometimes used UUIDs before receiving a message hash.
        if let Some(payload) = encode_chat_extension_data_v2(ext) {
            fields.push((
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE_V2.to_vec(),
            ));
            fields.push((lxmf_core::constants::FIELD_CUSTOM_DATA, payload));
        } else if let Some(payload) = encode_chat_extension_data_v1(ext) {
            fields.push((
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE_V1.to_vec(),
            ));
            fields.push((lxmf_core::constants::FIELD_CUSTOM_DATA, payload));
        }
    }
    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

// Accept the spec'd bin(32) hash plus the 64-char-ASCII-hex variant some
// clients put on the wire; normalizes to the lowercase hex id space the rest
// of the codebase (and the DB) keys on.
fn standard_hash_hex(bytes: &[u8]) -> Option<String> {
    if bytes.len() == 32 {
        return Some(hex::encode(bytes));
    }
    std::str::from_utf8(bytes)
        .ok()
        .filter(|value| bytes.len() == 64 && hex::decode(value).is_ok_and(|hash| hash.len() == 32))
        .map(str::to_ascii_lowercase)
}

// Selected schema fields are borrowed until their final UI projection. Unknown
// values are structurally scanned, never expanded into an owned MessagePack tree.
fn chat_field_text(value: lxmf_core::message::FieldValueRef<'_>) -> Option<&str> {
    value
        .as_text()
        .or_else(|| std::str::from_utf8(value.as_binary()?).ok())
}

fn decode_legacy_reaction_json(bytes: &[u8]) -> Result<RatspeakChatExtension, serde_json::Error> {
    // Stream this old flat schema. Unknown values are ignored without owned
    // trees; the small key count and input caps bound duplicate-key bookkeeping.
    struct LegacyReaction;
    impl<'de> serde::de::Visitor<'de> for LegacyReaction {
        type Value = RatspeakChatExtension;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a bounded reaction object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut keys = Vec::<String>::with_capacity(64);
            let (mut target, mut emoji, mut action) = (None, None, None);
            while let Some(key) = map.next_key::<String>()? {
                if keys.len() == 64 || keys.contains(&key) {
                    return Err(serde::de::Error::custom(
                        "duplicate or excessive reaction keys",
                    ));
                }
                match key.as_str() {
                    "message_id" => target = Some(map.next_value::<String>()?),
                    "emoji" => emoji = Some(map.next_value::<String>()?),
                    "action" => action = Some(map.next_value::<String>()?),
                    _ => {
                        let _ = map.next_value::<serde::de::IgnoredAny>()?;
                    }
                }
                keys.push(key);
            }
            Ok(RatspeakChatExtension::Reaction {
                target: target
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| serde::de::Error::custom("missing reaction target"))?,
                emoji: emoji
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| serde::de::Error::custom("missing reaction emoji"))?,
                action: action.unwrap_or_else(|| "add".into()),
            })
        }
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let reaction = serde::Deserializer::deserialize_map(&mut decoder, LegacyReaction)?;
    decoder.end()?;
    Ok(reaction)
}

/// Validates recognized chat schemas before durable propagation admission. The
/// compatibility wrapper below still presents malformed extensions as absent
/// to existing direct-message consumers, which do not grant remote purge rights.
pub(crate) fn decode_ratspeak_chat_extension_checked(
    msg: &LxMessage,
) -> Result<Option<RatspeakChatExtension>, lxmf_core::admission::SuppressionReason> {
    use lxmf_core::admission::SuppressionReason::{Malformed, TooLarge};
    use lxmf_core::message::{FieldMapKey, FieldValueRef};
    if let Some(data) = msg.get_field(lxmf_core::constants::FIELD_REACTION) {
        let map = data.unique_map().map_err(|_| Malformed)?;
        let target = map
            .get(FieldMapKey::Integer(
                lxmf_core::constants::REACTION_TO.into(),
            ))
            .ok_or(Malformed)?;
        let target = standard_hash_hex(
            target
                .as_binary()
                .or_else(|| target.as_text().map(str::as_bytes))
                .ok_or(Malformed)?,
        )
        .ok_or(Malformed)?;
        let emoji = chat_field_text(
            map.get(FieldMapKey::Integer(
                lxmf_core::constants::REACTION_CONTENT.into(),
            ))
            .ok_or(Malformed)?,
        )
        .filter(|text| !text.is_empty())
        .ok_or(Malformed)?
        .to_owned();
        return Ok(Some(RatspeakChatExtension::Reaction {
            target,
            emoji,
            action: "add".into(),
        }));
    }
    if let Some(target) = msg.get_field(lxmf_core::constants::FIELD_REPLY_TO) {
        let target = standard_hash_hex(target.as_binary().ok_or(Malformed)?).ok_or(Malformed)?;
        let preview = match msg.get_field(lxmf_core::constants::FIELD_REPLY_QUOTE) {
            Some(value) => std::str::from_utf8(value.as_binary().ok_or(Malformed)?)
                .map_err(|_| Malformed)?
                .to_owned(),
            None => String::new(),
        };
        return Ok(Some(RatspeakChatExtension::Reply {
            target,
            preview,
            target_sender: None,
        }));
    }
    let Some(custom_type) = chat_custom_type(msg) else {
        return Ok(None);
    };
    if custom_type == LEGACY_RATSPEAK_REACTION_TYPE {
        // This historical JSON shape only contains short reaction metadata.
        // Its separate input and key-count caps bound the streaming decoder.
        const MAX_LEGACY_REACTION_JSON_BYTES: usize = 4096;
        if msg.content_bytes().len() > MAX_LEGACY_REACTION_JSON_BYTES {
            return Err(TooLarge);
        }
        return decode_legacy_reaction_json(msg.content_bytes())
            .map(Some)
            .map_err(|_| Malformed);
    }
    let v2 = custom_type == RATSPEAK_CHAT_CUSTOM_TYPE_V2;
    if !v2 && custom_type != RATSPEAK_CHAT_CUSTOM_TYPE_V1 {
        return Ok(None);
    }
    let data = msg
        .get_field(lxmf_core::constants::FIELD_CUSTOM_DATA)
        .and_then(|value| value.as_binary())
        .ok_or(Malformed)?;
    let map = FieldValueRef::from_encoded(data)
        .and_then(|value| value.unique_map())
        .map_err(|_| Malformed)?;
    let text = |key| {
        map.get(FieldMapKey::Text(key))
            .and_then(|value| value.as_text())
    };
    let target = |key| {
        let value = map.get(FieldMapKey::Text(key)).ok_or(Malformed)?;
        if v2 {
            value
                .as_binary()
                .filter(|bytes| bytes.len() == 32)
                .map(hex::encode)
                .ok_or(Malformed)
        } else {
            value
                .as_text()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .ok_or(Malformed)
        }
    };
    let optional_text = |key| match map.get(FieldMapKey::Text(key)) {
        Some(value) => value.as_text().map(str::to_owned).ok_or(Malformed),
        None => Ok(String::new()),
    };
    match text("kind").ok_or(Malformed)? {
        "reaction" => {
            let target = target("target")?;
            let emoji = text("emoji")
                .filter(|text| !text.is_empty())
                .ok_or(Malformed)?
                .to_owned();
            let action = match map.get(FieldMapKey::Text("action")) {
                Some(value) => value.as_text().ok_or(Malformed)?.to_owned(),
                None => "add".into(),
            };
            Ok(Some(RatspeakChatExtension::Reaction {
                target,
                emoji,
                action,
            }))
        }
        "reply" => {
            let target_sender = map
                .get(FieldMapKey::Text("target_sender"))
                .map(|_| target("target_sender"))
                .transpose()?;
            Ok(Some(RatspeakChatExtension::Reply {
                target: target("target")?,
                preview: optional_text("preview")?,
                target_sender,
            }))
        }
        _ => Ok(None),
    }
}

pub fn decode_ratspeak_chat_extension(msg: &LxMessage) -> Option<RatspeakChatExtension> {
    decode_ratspeak_chat_extension_checked(msg).ok().flatten()
}

fn direct_link_start_step(kind: DirectLinkStartKind) -> &'static str {
    match kind {
        DirectLinkStartKind::NewDirect => "link_establishing",
        DirectLinkStartKind::ReusedActiveDirect => "reusing_direct_link",
        DirectLinkStartKind::QueuedOnDirect => "sending_via_link",
    }
}

struct DirectLinkDeliveryStartInput {
    message: LxMessage,
    dest_hash: [u8; 16],
    hops: u8,
    now: f64,
    msg_hash: Option<[u8; 32]>,
    is_ephemeral: bool,
    router_owned: bool,
}

fn direct_route_snapshot_from_entry(
    dest_hash: [u8; 16],
    entry: &PathTableRpcEntry,
) -> DirectRouteSnapshot {
    DirectRouteSnapshot {
        destination_hash: dest_hash,
        hops: entry.hops.max(1),
        interface_name: Some(entry.interface.clone()),
        learned_at: Some(entry.timestamp),
        expires_at: Some(entry.expires),
    }
}

pub type PropagationHealth = (
    Vec<[u8; 16]>,
    Vec<([u8; 16], String)>,
    Vec<[u8; 16]>,
    Vec<([u8; 16], String)>,
);

#[derive(Debug, Clone, PartialEq)]
pub struct LxmfDeliveryProgressUpdate {
    pub msg_id: String,
    pub kind: LxmfDeliveryProgressKind,
    pub event_method: LxmfDeliveryProgressMethod,
    pub delivery_representation: LxmfDeliveryProgressRepresentation,
    pub step: &'static str,
    pub method: &'static str,
    pub progress: Option<f64>,
    pub link_id: Option<String>,
    pub dest_hash: String,
    pub attempts: u32,
    pub representation: &'static str,
    pub queued_deliveries: usize,
    pub in_flight_deliveries: usize,
    pub reason: Option<String>,
}

/// Typed delivery evidence retained beside the legacy product display string.
/// Activity consumes this enum directly and never parses `step` or `reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LxmfDeliveryProgressKind {
    LinkEstablishing,
    LinkEstablished,
    DirectLinkPending,
    DirectLinkReused,
    BackchannelLinkReused,
    TransferStarted,
    TransferProgress,
    AwaitingProof,
    Delivered,
    Rejected,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LxmfDeliveryProgressMethod {
    Direct,
    PropagationDeposit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LxmfDeliveryProgressRepresentation {
    Unknown,
    Packet,
    Resource,
    Paper,
}

/// Result of a locally accepted outbound LXMF submission. The method is read
/// from the normalized message that was actually persisted and handed to the
/// router, not from the pre-normalization preference decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LxmfQueuedMessage {
    pub message_id: String,
    pub method: DeliveryMethod,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LxmfDeliveryFailureUpdate {
    pub msg_id: String,
    pub code: &'static str,
    pub actual_bytes: usize,
    pub limit_bytes: usize,
}

/// Curated local failure surface. Detailed build/sign/pack failures remain in
/// local diagnostics and never cross into Activity as arbitrary prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LxmfSubmissionFailure {
    PreparationFailed,
    StorageFailed,
    ResourceLimitExceeded {
        actual_bytes: usize,
        limit_bytes: usize,
    },
}

/// Stable string identifier for the chosen `DeliveryMethod`. Persisted in the
/// `messages.delivery_method` column and surfaced to the frontend so the UI can
/// render proof-aware state icons.
pub fn delivery_method_name(method: DeliveryMethod) -> &'static str {
    match method {
        DeliveryMethod::Opportunistic => "opportunistic",
        DeliveryMethod::Direct => "direct",
        DeliveryMethod::Propagated => "propagated",
        DeliveryMethod::Paper => "paper",
    }
}

trait OutboundMessageView: std::ops::DerefMut<Target = MessageMetadata> {
    fn destination_hash(&self) -> [u8; 16];
    fn message_id(&self) -> [u8; 32];
    fn packed_len(&self) -> Result<usize, MessageError>;
}
impl OutboundMessageView for LxMessage {
    fn destination_hash(&self) -> [u8; 16] {
        self.destination_hash()
    }
    fn message_id(&self) -> [u8; 32] {
        self.message_id()
    }
    fn packed_len(&self) -> Result<usize, MessageError> {
        self.packed_len()
    }
}
impl OutboundMessageView for SignedLxMessage {
    fn destination_hash(&self) -> [u8; 16] {
        self.destination_hash()
    }
    fn message_id(&self) -> [u8; 32] {
        self.message_id()
    }
    fn packed_len(&self) -> Result<usize, MessageError> {
        finalized_packed_len(self)
    }
}

/// The wire length `msg` has once finalized with its recipient stamp, which
/// `finalize` derives from the same signed inputs. As in Python's `pack()`, the
/// stamp is one more bin8 element: marker, length, and a ticket's 16-byte hash
/// or a 32-byte proof of work. The payload's fixarray marker stays one byte.
fn finalized_packed_len(msg: &SignedLxMessage) -> Result<usize, MessageError> {
    let stamp = if msg.outbound_ticket().is_some() {
        2 + lxmf_core::constants::TICKET_LENGTH
    } else if msg.stamp_cost().is_some() {
        2 + lxmf_core::constants::STAMP_SIZE
    } else {
        0
    };
    msg.packed_len()?
        .checked_add(stamp)
        .ok_or_else(|| MessageError::PackFailed("wire size overflow".into()))
}

fn message_within_resource_limit(msg: &impl OutboundMessageView) -> bool {
    match msg.packed_len() {
        Ok(packed) if packed <= MAX_LXMF_RESOURCE_BYTES => true,
        Ok(packed) => {
            tracing::warn!(
                packed_len = packed,
                max_len = MAX_LXMF_RESOURCE_BYTES,
                "LXMF message exceeds RNS resource limit"
            );
            false
        }
        Err(_) => {
            tracing::warn!(
                reason = "pack_failed",
                "LXMF message failed to pack before send"
            );
            false
        }
    }
}

fn validate_attachment_envelope_size(actual_bytes: usize) -> Result<(), LxmfSubmissionFailure> {
    if actual_bytes <= MAX_LXMF_RESOURCE_BYTES {
        Ok(())
    } else {
        Err(LxmfSubmissionFailure::ResourceLimitExceeded {
            actual_bytes,
            limit_bytes: MAX_LXMF_RESOURCE_BYTES,
        })
    }
}

fn validate_outbound_audio(
    audio_bytes: &[u8],
    audio_mode: u8,
) -> Result<(), LxmfSubmissionFailure> {
    #[cfg(feature = "lxst-voice")]
    {
        let format = crate::voice_memo::MemoFormat::from_mode(audio_mode)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        if format == crate::voice_memo::MemoFormat::Compact
            && audio_bytes.len() > crate::voice_memo::compact::RECORD_BYTES
        {
            return Err(LxmfSubmissionFailure::PreparationFailed);
        }
        crate::voice_memo::inspect_voice_memo_format(audio_bytes, format)
            .map(|_| ())
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)
    }
    #[cfg(not(feature = "lxst-voice"))]
    {
        let _ = (audio_bytes, audio_mode);
        Err(LxmfSubmissionFailure::PreparationFailed)
    }
}

fn validate_audio_message_size(actual_bytes: usize) -> Result<(), LxmfSubmissionFailure> {
    let limit_bytes = rns_protocol::resource::MAX_EFFICIENT_SIZE;
    if actual_bytes >= limit_bytes {
        return Err(LxmfSubmissionFailure::ResourceLimitExceeded {
            actual_bytes,
            limit_bytes,
        });
    }
    Ok(())
}

fn normalize_protocol_delivery_method(msg: &mut impl OutboundMessageView) {
    if msg.method == DeliveryMethod::Opportunistic
        && let Ok(packed_len) = msg.packed_len()
    {
        let content_size = packed_len.saturating_sub(96 + TIMESTAMP_SIZE + STRUCT_OVERHEAD);
        if content_size > ENCRYPTED_PACKET_MAX_CONTENT {
            msg.method = DeliveryMethod::Direct;
        }
    }
}

/// Frontend/user preference for a send. `Auto` applies Ratspeak's UX policy;
/// the others force a protocol method when available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliveryPreference {
    #[default]
    Auto,
    Opportunistic,
    Direct,
    Propagated,
}

/// Fully-specified LXMF attachment send request from the app command layer.
pub struct AttachmentMessageRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub file_name: &'a str,
    pub file_bytes: &'a [u8],
    /// Private app-owned staging file to atomically adopt into message
    /// storage, avoiding a second full-size filesystem copy.
    pub staged_path: Option<&'a Path>,
    pub is_image: bool,
    pub image_mime: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
    pub renderer: Option<u8>,
    pub thread: &'a str,
}

/// Fully-specified first-class LXMF audio send request with an explicit wire mode.
///
/// Staging-token ownership remains in the command layer. When `staged_path`
/// is present, the runtime atomically adopts that already-validated private
/// file into identity-scoped message storage before queueing the LXM.
pub struct AudioMessageRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub audio_bytes: &'a [u8],
    pub audio_mode: u8,
    pub staged_path: Option<&'a Path>,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
    pub thread: &'a str,
}

impl DeliveryPreference {
    pub fn parse(value: Option<&str>) -> Self {
        match value.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
            "opportunistic" => Self::Opportunistic,
            "direct" => Self::Direct,
            "propagated" => Self::Propagated,
            _ => Self::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Opportunistic => "opportunistic",
            Self::Direct => "direct",
            Self::Propagated => "propagated",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryProfile {
    /// Chat-like payloads. Auto reuses Links, otherwise prefers ratcheted
    /// Opportunistic delivery when the packed message fits a packet.
    Message,
    /// Payloads that usually need proof-backed link/resource delivery.
    Attachment,
    /// LRGP game actions start with proof-backed live Link delivery.
    Lrgp,
}

impl DeliveryProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Attachment => "attachment",
            Self::Lrgp => "lrgp",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "message" => Some(Self::Message),
            "attachment" => Some(Self::Attachment),
            "lrgp" => Some(Self::Lrgp),
            _ => None,
        }
    }
}

async fn forward_backchannel_receipt(
    mut runtime_rx: oneshot::Receiver<
        Result<
            rns_runtime::link_manager::LinkPayloadSendReceipt,
            rns_runtime::link_manager::LinkSendError,
        >,
    >,
    mut owner_tx: oneshot::Sender<Result<BackchannelSendReceipt, BackchannelSendError>>,
    abandon_tx: mpsc::Sender<BackchannelSendReceipt>,
) {
    let result = tokio::select! {
        biased;
        result = &mut runtime_rx => Some(result),
        _ = owner_tx.closed() => None,
        _ = tokio::time::sleep(Duration::from_secs(10)) => None,
    };
    let Some(result) = result else {
        // close is a publication barrier: a prior successful send remains
        // readable; every later runtime send fails and performs exact cleanup.
        runtime_rx.close();
        if let Ok(Ok(receipt)) = runtime_rx.try_recv() {
            let _ = abandon_tx
                .send(backchannel_receipt_from_runtime(receipt))
                .await;
        }
        let _ = owner_tx.send(Err(BackchannelSendError::TransportUnavailable));
        return;
    };
    let result = match result {
        Ok(Ok(receipt)) => Ok(backchannel_receipt_from_runtime(receipt)),
        Ok(Err(error)) => Err(backchannel_error_from_runtime(error)),
        Err(_) => Err(BackchannelSendError::TransportUnavailable),
    };
    if let Err(Ok(receipt)) = owner_tx.send(result) {
        let _ = abandon_tx.send(receipt).await;
    }
}

fn backchannel_receipt_from_runtime(
    receipt: rns_runtime::link_manager::LinkPayloadSendReceipt,
) -> BackchannelSendReceipt {
    match receipt {
        rns_runtime::link_manager::LinkPayloadSendReceipt::Packet(receipt) => {
            BackchannelSendReceipt::Packet {
                link_id: receipt.link_id,
                packet_hash: receipt.packet_hash,
            }
        }
        rns_runtime::link_manager::LinkPayloadSendReceipt::Resource(receipt) => {
            BackchannelSendReceipt::Resource {
                link_id: receipt.link_id,
                resource_hash: receipt.resource_hash,
            }
        }
    }
}

fn backchannel_error_from_runtime(
    err: rns_runtime::link_manager::LinkSendError,
) -> BackchannelSendError {
    match err {
        rns_runtime::link_manager::LinkSendError::LinkNotFound => {
            BackchannelSendError::LinkNotFound
        }
        rns_runtime::link_manager::LinkSendError::LinkNotActive => {
            BackchannelSendError::LinkNotActive
        }
        rns_runtime::link_manager::LinkSendError::NoSessionKeys => {
            BackchannelSendError::NoSessionKeys
        }
        rns_runtime::link_manager::LinkSendError::IdentityUnavailable
        | rns_runtime::link_manager::LinkSendError::IdentificationUnavailable => {
            // These commands are not expected for an already-identified LXMF
            // backchannel. If the runtime reports either anyway, retire the
            // cached Link and let core retry normal Direct discovery instead
            // of terminally losing the message.
            BackchannelSendError::LinkNotActive
        }
        rns_runtime::link_manager::LinkSendError::TransportUnavailable => {
            BackchannelSendError::TransportUnavailable
        }
        rns_runtime::link_manager::LinkSendError::ResourceStartFailed => {
            BackchannelSendError::ResourceStartFailed
        }
    }
}

pub struct MessageSendRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
    pub profile: DeliveryProfile,
    pub renderer: Option<u8>,
    pub thread: &'a str,
}

pub struct ReplyMessageSendRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub content: &'a str,
    pub title: &'a str,
    pub reply_to_id: &'a str,
    pub reply_to_preview: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
    pub profile: DeliveryProfile,
    pub renderer: Option<u8>,
    pub thread: &'a str,
}

struct MessageWithMethodRequest<'a> {
    dest_hash_hex: &'a str,
    content: &'a str,
    title: &'a str,
    db_pool: &'a DbPool,
    identity_id: &'a str,
    delivery_method: DeliveryMethod,
    preference: DeliveryPreference,
    profile: DeliveryProfile,
    reply_to_id: Option<&'a str>,
    reply_to_preview: Option<&'a str>,
    renderer: Option<u8>,
    thread: &'a str,
}

pub struct ReactionSendRequest<'a> {
    pub dest_hash_hex: &'a str,
    pub message_id: &'a str,
    pub emoji: &'a str,
    pub action: &'a str,
    pub db_pool: &'a DbPool,
    pub identity_id: &'a str,
    pub preference: DeliveryPreference,
}

pub fn lxmf_compression_support_db_value(support: CompressionSupport) -> Option<&'static str> {
    match support {
        CompressionSupport::Unknown => None,
        CompressionSupport::Unsupported => Some(db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED),
        CompressionSupport::Supported => Some(db::LXMF_COMPRESSION_SUPPORT_SUPPORTED),
    }
}

pub fn lxmf_compression_support_db_value_from_app_data(app_data: &[u8]) -> Option<&'static str> {
    lxmf_compression_support_db_value(
        lxmf_core::handlers::compression_support_state_from_app_data(app_data),
    )
}

struct PendingDirectLinkIdentification {
    link_id: [u8; 16],
    observed_at: Instant,
}

struct PendingOpportunisticDelivery {
    message: LxMessage,
    retry_at: f64,
    packet_hash: [u8; 32],
}

/// Ordered terminal notifications emitted by the responder-side Reticulum
/// LinkManager. Production derives this compact stream from the manager's
/// lossless accounting channel so proof, Resource conclusion and Link closure
/// retain actor order without cloning completed Resource payloads.
#[derive(Debug, Clone)]
pub(crate) enum BackchannelLinkEvent {
    PacketWait {
        link_id: [u8; 16],
        packet_hash: [u8; 32],
        started_at: Instant,
        timeout: Duration,
        awaiting_admission: bool,
        cancellation:
            Option<rns_transport::link_endpoint_dispatch::LinkEndpointDispatchCancellation>,
    },
    ResourceWait {
        link_id: [u8; 16],
        resource_hash: [u8; 32],
        started_at: Instant,
        timeout: Duration,
    },
    PacketProof(rns_runtime::link_manager::LinkPacketProof),
    ResourceConclusion {
        link_id: [u8; 16],
        resource_hash: [u8; 32],
        conclusion: rns_runtime::link_manager::LinkResourceConclusion,
    },
    LinkClosed {
        link_id: [u8; 16],
    },
}

/// Captured briefly under manager ownership; decrypt in the bounded blocking
/// admission worker. Secret snapshots are zeroized when the owner exits.
pub struct PropagationDecryptContext {
    identity: Identity,
    destination: [u8; 16],
    ratchets: zeroize::Zeroizing<Vec<[u8; 32]>>,
}
impl PropagationDecryptContext {
    pub fn decrypt(&self, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 17 || data[..16] != self.destination {
            return None;
        }
        let refs = self.ratchets.iter().collect::<Vec<_>>();
        let plaintext = self
            .identity
            .decrypt(
                &data[16..],
                (!refs.is_empty()).then_some(refs.as_slice()),
                false,
            )
            .ok()?;
        let mut full = Vec::with_capacity(16 + plaintext.len());
        full.extend_from_slice(&self.destination);
        full.extend_from_slice(&plaintext);
        Some(full)
    }
}

#[derive(Clone)]
pub struct DownloadSuppressionSnapshot {
    pub identity: [u8; 16],
    pub node: [u8; 16],
    pub ledger: SuppressionLedger,
    pub epochs: AdmissionEpochs,
    revision: u64,
    client_revision: u64,
}
struct InstalledDownloadState {
    node: [u8; 16],
    ledger: SuppressionLedger,
    epochs: AdmissionEpochs,
    haves: Vec<lxmf_core::admission::DurableHave>,
}

enum PreparedPropagation {
    Pending,
    Ready(Vec<u8>),
    Failed(MessageError),
}
struct PendingPropagationEnvelope {
    binding: [u8; 32],
    cost: u8,
    result: oneshot::Receiver<Result<PropagationEnvelope, MessageError>>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for PendingPropagationEnvelope {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
fn propagation_preparation_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}

pub struct LxmfManager {
    pub identity: Identity,
    /// True when `identity` is backed by a hardware token (PIV). Gates features
    /// such as propagation-node hosting and disables private-key export.
    pub is_hardware: bool,
    pub identity_hash: String,
    pub lxmf_hash: String,
    pub lxmf_dest_hash: [u8; 16],
    pub propagation_dest_hash: [u8; 16],
    pub router: LxmRouter,
    pub data_dir: PathBuf,
    pub lxmf_storage_dir: PathBuf,
    pub display_name: String,
    pub status: String,
    pub announce_ratspeak_usage: bool,
    delivery_ratchets: DeliveryRatchetState,
    announce_cache_started: Instant,
    pub received_ratchets: HashMap<String, ReceivedRatchet>,
    /// Remote ratchets whose current value has not yet been acknowledged by
    /// the serialized persistence owner. Persistence failures leave entries
    /// here so a later delta/periodic pass retries them without replaying the
    /// complete received-ratchet store.
    dirty_received_ratchets: HashSet<String>,
    pub known_identities: HashMap<String, [u8; 64]>,
    known_identities_revision: u64,
    known_identities_persisted_revision: u64,
    peer_lxmf_compression_support: HashMap<[u8; 16], CompressionSupport>,
    route_hops: HashMap<[u8; 16], u8>,
    route_entries: HashMap<[u8; 16], PathTableRpcEntry>,
    path_recovery: Option<rns_transport::path_recovery::PathRecoveryHandle>,
    shared_recovery_owner: Option<rns_runtime::reticulum::ReticulumHandle>,
    shared_recovery_slots: std::sync::Arc<tokio::sync::Semaphore>,
    pending_path_recoveries: HashMap<[u8; 16], recovery::PendingPathRecovery>,
    route_snapshot_started: Instant,
    path_recovery_refresh_needed: bool,
    delivery_timing: timing::DeliveryTimingCache,
    /// Held so identity-switch can re-register with the transport actor.
    pub delivery_tx:
        Option<tokio::sync::mpsc::Sender<rns_transport::link_messages::DestinationEvent>>,
    pub link_delivery: Option<lxmf_core::link_delivery::LinkDeliveryManager>,
    endpoint_dispatch: Option<rns_transport::link_endpoint_dispatch::LinkEndpointDispatchHandle>,
    lxmf_link_command_tx: Option<mpsc::Sender<rns_runtime::link_manager::LinkManagerCommand>>,
    lxmf_direct_link_packet_tx: Option<mpsc::UnboundedSender<(Vec<u8>, [u8; 16])>>,
    direct_inbound_resource_accept_handler: Option<DirectInboundResourceAcceptHandler>,
    direct_inbound_resource_concluded_handler: Option<DirectInboundResourceConcludedHandler>,
    direct_inbound_resource_completion_handler: Option<DirectInboundResourceCompletionHandler>,
    pending_direct_link_identifications: HashMap<[u8; 16], PendingDirectLinkIdentification>,
    lxmf_backchannel_command_rx: Option<mpsc::Receiver<BackchannelSendCommand>>,
    backchannel_abandon_tx: mpsc::Sender<BackchannelSendReceipt>,
    backchannel_abandon_rx: mpsc::Receiver<BackchannelSendReceipt>,
    pending_backchannel_abandon: Option<BackchannelSendReceipt>,
    backchannel_bridge_slots: Arc<tokio::sync::Semaphore>,
    lxmf_link_identified_rx: Option<mpsc::Receiver<([u8; 16], [u8; 16])>>,
    lxmf_backchannel_event_rx: Option<mpsc::UnboundedReceiver<BackchannelLinkEvent>>,
    pending_backchannel_resource_cancellations:
        std::collections::VecDeque<lxmf_core::link_delivery::BackchannelResourceCancelRequest>,
    opportunistic_proof_tx:
        Option<mpsc::UnboundedSender<rns_transport::link_messages::DestinationEvent>>,
    opportunistic_in_flight: HashMap<[u8; 32], PendingOpportunisticDelivery>,
    opportunistic_proofs: HashMap<[u8; 32], packet_delivery::ProofOwner>,
    packet_delivery_rtts: Vec<(String, Duration)>,
    pub propagation_sync: Option<lxmf_core::propagation_sync::PropagationSyncTask>,
    pub propagation_client: Option<lxmf_core::propagation_client::PropagationClient>,
    delivery_limit_kb: f64,
    last_propagation_check: f64,
    pub client_propagation_enabled: bool,
    pub configured_propagation_node: Option<[u8; 16]>,
    propagation_transfer_limits_kb: HashMap<[u8; 16], u64>,
    propagation_node_policies: HashMap<[u8; 16], AdvertisedNodePolicy>,
    propagation_envelopes: HashMap<[u8; 32], PropagationEnvelope>,
    pending_propagation_envelopes: HashMap<[u8; 32], PendingPropagationEnvelope>,
    propagation_preparation_tasks: Vec<tokio::task::JoinHandle<()>>,
    last_ratchet_clean: f64,
    last_router_cull: f64,
    pub received_ratchets_dir: PathBuf,
    pending_expired_received_ratchets: Vec<String>,
    /// Outbound message hashes routed via propagation. `LinkDeliveryManager`
    /// reports `Complete` for both propagation deposits and large-message
    /// direct sends; this map lets `tick()` map completion to the right state
    /// (`propagated` for deposit, `delivered` for direct) and attribute relay
    /// health back to the selected propagation node.
    in_flight_propagation: std::collections::HashMap<[u8; 32], [u8; 16]>,
    completed_propagation_deposits: Vec<[u8; 16]>,
    failed_propagation_deposits: Vec<([u8; 16], String)>,
    completed_propagation_syncs: Vec<[u8; 16]>,
    failed_propagation_syncs: Vec<([u8; 16], String)>,
    downloaded_propagation_messages: Vec<DownloadedPropagationMessage>,
    download_state: Option<InstalledDownloadState>,
    download_suppression_revision: u64,
    last_propagation_admission: lxmf_core::propagation_client::PropagationAdmissionCounts,
    delivery_progress_updates: Vec<LxmfDeliveryProgressUpdate>,
    delivery_failure_updates: Vec<LxmfDeliveryFailureUpdate>,
    ephemeral_outbound: HashSet<[u8; 32]>,
    last_reported_steps: HashMap<String, ReportedStep>,
    /// Auto sends that began over a live LXMF method and may be retried once
    /// through the configured Offline Inbox after live delivery fails.
    auto_live_fallback: HashSet<[u8; 32]>,
    live_preparation: preparation::LivePreparation,
    /// Outbox sends from the last run or a Retry, queued by the tick.
    resumable: Vec<ResumableSend>,
    /// Failed sends whose earlier packets can still be proven; they fail only
    /// once the last receipt expires.
    deferred_failures: HashSet<[u8; 32]>,
}

/// An outbox send waiting to be queued again.
struct ResumableSend {
    message_id: [u8; 32],
    message: crate::outbox::OutboxMessage,
    preference: DeliveryPreference,
    profile: DeliveryProfile,
    auto_compress: bool,
    since: f64,
}

enum Resumed {
    Queued(DeliveryMethod),
    Waiting,
    Failed,
}

/// How long a resumed Offline Inbox send waits for its relay after startup.
const RESUME_RELAY_WAIT_SECS: f64 = 120.0;

/// Immutable known-identity artifact captured while the LXMF manager is
/// locked and persisted only after that protocol lock has been released.
pub struct KnownIdentitiesSnapshot {
    identity_hash: String,
    revision: u64,
    path: PathBuf,
    bytes: Vec<u8>,
    pub count: usize,
}

impl KnownIdentitiesSnapshot {
    pub fn persist(&self) -> std::io::Result<()> {
        lxmf_core::persist::write_file_atomic(&self.path, &self.bytes)
    }
}

/// Immutable periodic/shutdown checkpoint. Received ratchets are deliberately
/// absent: each changed remote ratchet is persisted as a per-destination delta
/// when learned, so a checkpoint never rewrites thousands of unchanged files.
pub struct LxmfCheckpointSnapshot {
    known_identities: KnownIdentitiesSnapshot,
    router_state: RouterStateSnapshot,
    router_state_dir: PathBuf,
    node_policies: HashMap<[u8; 16], AdvertisedNodePolicy>,
}

impl LxmfCheckpointSnapshot {
    pub fn persist(&self) -> std::io::Result<()> {
        self.known_identities.persist()?;
        self.router_state.save_state(&self.router_state_dir)?;
        lxmf_core::persist::save_node_policies(&self.router_state_dir, &self.node_policies)
    }

    pub fn known_identities_count(&self) -> usize {
        self.known_identities.count
    }
}

/// Owned delta for state that changed during an LXMF tick or announce
/// observation. All paths and values are resolved before the manager lock is
/// released; persistence is then independent of live protocol ownership.
pub struct LxmfPersistenceDelta {
    identity_hash: String,
    known_identities: Option<KnownIdentitiesSnapshot>,
    received_ratchets: Vec<(String, PathBuf, ReceivedRatchet)>,
    router_state: Option<(PathBuf, RouterStateSnapshot)>,
    node_policies: Option<(PathBuf, HashMap<[u8; 16], AdvertisedNodePolicy>)>,
}

impl LxmfPersistenceDelta {
    pub fn is_empty(&self) -> bool {
        self.known_identities.is_none()
            && self.received_ratchets.is_empty()
            && self.router_state.is_none()
            && self.node_policies.is_none()
    }

    pub fn persist(&self) -> std::io::Result<()> {
        for (_, path, ratchet) in &self.received_ratchets {
            ratchet.save_durable(path)?;
        }
        if let Some(snapshot) = self.known_identities.as_ref() {
            snapshot.persist()?;
        }
        if let Some((path, snapshot)) = self.router_state.as_ref() {
            snapshot.save_state(path)?;
        }
        if let Some((path, policies)) = &self.node_policies {
            lxmf_core::persist::save_node_policies(path, policies)?;
        }
        Ok(())
    }

    pub fn artifact_count(&self) -> usize {
        usize::from(self.known_identities.is_some())
            + self.received_ratchets.len()
            + usize::from(self.router_state.is_some())
            + usize::from(self.node_policies.is_some())
    }

    fn persisted_ratchets(&self) -> impl Iterator<Item = (&str, ReceivedRatchet)> {
        self.received_ratchets
            .iter()
            .map(|(hash, _, ratchet)| (hash.as_str(), *ratchet))
    }
}

#[derive(Debug, Clone, Copy)]
struct ReportedStep {
    step: &'static str,
    observed_at: f64,
}

const MAX_REPORTED_STEPS: usize = 4_096;
const REPORTED_STEP_TTL_SECS: f64 = 24.0 * 60.0 * 60.0;

fn terminal_delivery_step(step: &str) -> bool {
    matches!(
        step,
        "delivered" | "propagated" | "rejected" | "failed" | "cancelled"
    )
}

/// Loads a hardware (PIV-backed) identity from its `.hwid`; `pin` comes from
/// the unlock UI.
#[cfg(feature = "hardware")]
fn load_hwid_identity(
    hwid_file: &Path,
    hash: &str,
    pin: Option<&str>,
) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    let cfg = rns_ratkey::HwidConfig::from_file(hwid_file)
        .map_err(|e| format!("invalid .hwid for {hash}: {e}"))?;
    let pin = pin.ok_or_else(|| "hardware identity is locked (no PIN provided)".to_string())?;
    tracing::info!(identity = %crate::short_id(hash), "loading hardware (PIV) identity");
    // Keep the RatkeyError Display in the message — the unlock UI parses it for
    // remaining-attempts / blocked.
    Ok(rns_ratkey::load_hardware_identity(&cfg, pin)
        .map_err(|e| format!("hardware identity load failed: {e}"))?)
}

#[cfg(not(feature = "hardware"))]
fn load_hwid_identity(
    _hwid_file: &Path,
    hash: &str,
    _pin: Option<&str>,
) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    Err(
        format!("identity {hash} is hardware-backed but this build lacks the `hardware` feature")
            .into(),
    )
}

pub(crate) fn bounded_plain_identity_from_file(
    path: &Path,
) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    let key = crate::vault::read_plain_identity_key(path)?;
    Ok(Identity::from_private_key(key.as_ref())?)
}

/// Load a passcode-protected software identity from its `identity.enc` vault.
/// `passcode` is the user's passcode (None when still locked).
fn load_encrypted_identity(
    enc_file: &Path,
    hash: &str,
    passcode: Option<&str>,
    job: Option<&crate::vault::VaultJob>,
) -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    let vault = crate::vault::read_vault(enc_file)
        .map_err(|e| format!("invalid identity.enc for {hash}: {e}"))?;
    let passcode =
        passcode.ok_or_else(|| "identity is locked (no passcode provided)".to_string())?;
    let unlocked = match job {
        Some(job) => job.decrypt_identity(passcode, &vault),
        None => crate::vault::decrypt_identity(passcode, &vault),
    }
    .map_err(|e| format!("passcode unlock failed: {e}"))?;
    tracing::info!(identity = %crate::short_id(hash), "loading passcode-protected identity");
    Ok(Identity::from_private_key(unlocked.key())?)
}

fn received_ratchet_hash_from_path(path: &Path) -> Option<&str> {
    path.file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| crate::helpers::is_protocol_hash_16(name))
}

/// Read a local identity's public key without borrowing the live LXMF manager.
/// `data_dir` is the process's `.ratspeak` directory.
pub fn contact_card_public_key_from_profile(data_dir: &Path, hash_hex: &str) -> Option<[u8; 64]> {
    if !crate::helpers::is_protocol_hash_16(hash_hex) {
        return None;
    }

    if let Some(public_key) = hwid_contact_card_public_key(data_dir, hash_hex) {
        return Some(public_key);
    }

    let id_file = data_dir.join("identities").join(hash_hex).join("identity");
    bounded_plain_identity_from_file(&id_file)
        .ok()
        .filter(|identity| hex::encode(identity.hash) == hash_hex)
        .map(|identity| identity.get_public_key())
}

#[cfg(feature = "seed")]
fn hwid_contact_card_public_key(data_dir: &Path, hash_hex: &str) -> Option<[u8; 64]> {
    let hwid_file = data_dir
        .join("identities")
        .join(hash_hex)
        .join("identity.hwid");
    let cfg = rns_ratkey::HwidConfig::from_file(&hwid_file).ok()?;
    if cfg.identity.hash != hash_hex {
        return None;
    }

    let x25519_pub = cfg.x25519_pub_bytes().ok()?;
    let ed25519_pub = cfg.ed25519_pub_bytes().ok()?;
    let mut public_key = [0u8; 64];
    public_key[..32].copy_from_slice(&x25519_pub);
    public_key[32..].copy_from_slice(&ed25519_pub);

    let identity = Identity::from_public_key(&public_key).ok()?;
    (hex::encode(identity.hash) == hash_hex).then_some(public_key)
}

#[cfg(not(feature = "seed"))]
fn hwid_contact_card_public_key(_data_dir: &Path, _hash_hex: &str) -> Option<[u8; 64]> {
    None
}

fn apply_lrgp_fields_to_message(
    message: &mut LxMessageDraft,
    fields: &std::collections::HashMap<u8, rmpv::Value>,
) -> Result<(), String> {
    for (&field_id, value) in fields {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, value)
            .map_err(|error| format!("field {field_id:#x} encode failed: {error}"))?;

        // LRGP allocates native LXMF field values: 0xFB is a MessagePack
        // string and 0xFD is a MessagePack map. `set_field` would wrap these
        // already-encoded values in MessagePack BIN, which our own decoder
        // could unwrap but Python LXMF/LRGP peers correctly see as the wrong
        // wire types. Preserve the native value shape.
        message
            .set_msgpack_field(field_id, bytes)
            .map_err(|error| format!("field {field_id:#x} is invalid: {error}"))?;
    }
    Ok(())
}

const STORAGE_NAMESPACE_WITNESS: &str = ".ratspeak-namespace-witness";
const STORAGE_NAMESPACE_READY: &str = ".ratspeak-namespace-ready";
const STORAGE_NAMESPACE_BYTES: &[u8] = b"ratspeak-private-namespace-v1\n";
const STORAGE_NAMESPACE_RESET_BYTES: &[u8] = b"ratspeak-private-namespace-reset-pending-v1\n";
const IDENTITY_NAMESPACE_BYTES: &[u8] = b"ratspeak-identity-namespace-v1\n";
const DELETE_IDENTITY_ALL: &[u8] = b"ratspeak-identity-delete-all-v1\n";
const DELETE_IDENTITY_CRYPTO: &[u8] = b"ratspeak-identity-delete-crypto-v1\n";

fn require_real_directory(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private storage namespace must contain real directories",
        ));
    }
    validate_durable_storage_directory(path).map_err(std::io::Error::other)
}

fn established_storage_namespace(data_root: &Path) -> std::io::Result<DurableFileReceipt> {
    require_real_directory(data_root)?;
    durable_reconcile(&data_root.join(STORAGE_NAMESPACE_READY), STORAGE_NAMESPACE_BYTES)
        .map_err(|error| std::io::Error::other(format!(
            "private storage namespace is not initialized; call initialize_storage_namespace before opening the database: {error}"
        )))?;
    storage_namespace_witness(data_root)
}

fn storage_namespace_witness(data_root: &Path) -> std::io::Result<DurableFileReceipt> {
    require_real_directory(&data_root.join(".ratspeak"))?;
    require_real_directory(&data_root.join(".ratspeak/identities"))?;
    durable_reconcile(
        &data_root.join(STORAGE_NAMESPACE_WITNESS),
        STORAGE_NAMESPACE_BYTES,
    )
    .map_err(std::io::Error::other)
}

fn data_root_for_identity_namespace(ratspeak_dir: &Path) -> std::io::Result<PathBuf> {
    require_real_directory(ratspeak_dir)?;
    let data_root = ratspeak_dir
        .parent()
        .ok_or_else(|| std::io::Error::other("missing application data root"))?;
    let expected = data_root.join(".ratspeak");
    if std::fs::canonicalize(ratspeak_dir)? != std::fs::canonicalize(&expected)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "identity namespace must be the established application .ratspeak root",
        ));
    }
    Ok(data_root.to_owned())
}

fn identity_namespace_marker(ratspeak_dir: &Path, hash: &str) -> PathBuf {
    ratspeak_dir
        .join("identities")
        .join(format!("{hash}.namespace-ready"))
}

fn deletion_scope(path: &Path) -> std::io::Result<bool> {
    match read_file_bounded(path, 64)?.as_deref() {
        Some(DELETE_IDENTITY_ALL) => Ok(true),
        Some(DELETE_IDENTITY_CRYPTO) => Ok(false),
        _ => Err(std::io::Error::other(
            "invalid identity namespace deletion intent",
        )),
    }
}

fn retire_directory(path: &Path, witness: &DurableFileReceipt) -> std::io::Result<()> {
    let mut name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("missing directory name"))?
        .to_os_string();
    name.push(".namespace-retired");
    let retired = path.with_file_name(name);
    if path.try_exists()? {
        require_real_directory(path)?;
        if retired.try_exists()? {
            return Err(std::io::Error::other(
                "both identity deletion locations exist",
            ));
        }
        // A strict move publishes the intentional removal before erasing its
        // bytes. Restart recovery can finish only this exact recorded removal.
        durable_rename_directory(path, &retired, witness).map_err(std::io::Error::other)?;
    }
    if retired.try_exists()? {
        require_real_directory(&retired)?;
        std::fs::remove_dir_all(retired)?;
    }
    for suffix in [".rns-directory", ".rns-directory-lock"] {
        let mut name = path.file_name().unwrap().to_os_string();
        name.push(suffix);
        durable_remove(&path.with_file_name(name), witness).map_err(std::io::Error::other)?;
    }
    Ok(())
}

fn unlink_namespace_symlink(path: &Path, witness: &DurableFileReceipt) -> std::io::Result<()> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_symlink() {
        return Err(std::io::Error::other(
            "identity reset symlink changed before removal",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if kind.is_symlink_dir() {
            std::fs::remove_dir(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    #[cfg(not(windows))]
    std::fs::remove_file(path)?;
    durable_remove(path, witness).map_err(std::io::Error::other)?;
    Ok(())
}

fn retire_identity_namespace(
    ratspeak_dir: &Path,
    hash: &str,
    cascade: bool,
    witness: &DurableFileReceipt,
) -> std::io::Result<()> {
    let identities = ratspeak_dir.join("identities");
    let directory = identities.join(hash);
    let intent = identities.join(format!("{hash}.namespace-delete"));
    let bytes = if cascade {
        DELETE_IDENTITY_ALL
    } else {
        DELETE_IDENTITY_CRYPTO
    };
    let intent_receipt = if intent.try_exists()? {
        durable_reconcile(&intent, bytes)
    } else {
        durable_write(&intent, bytes)
    }
    .map_err(std::io::Error::other)?;
    if cascade {
        retire_directory(&directory, &intent_receipt)?;
    } else if directory.try_exists()? {
        for name in [
            "ratchets",
            "known_identities",
            "lxmf",
            "reticulum",
            "cache",
            "propagation",
        ] {
            retire_directory(&directory.join(name), &intent_receipt)?;
        }
        for name in ["identity", "identity.enc", "identity.seed"] {
            durable_remove(&directory.join(name), &intent_receipt)
                .map_err(std::io::Error::other)?;
        }
    }
    durable_remove(&identity_namespace_marker(ratspeak_dir, hash), witness)
        .map_err(std::io::Error::other)?;
    durable_remove(&intent, witness).map_err(std::io::Error::other)?;
    Ok(())
}

/// Clear identity profiles after their network/storage workers have stopped.
/// Keep the established app/database and identities roots while SQLite is live.
pub fn clear_identity_storage_namespace(ratspeak_dir: &Path) -> std::io::Result<()> {
    let data_root = data_root_for_identity_namespace(ratspeak_dir)?;
    let ready = data_root.join(STORAGE_NAMESPACE_READY);
    let witness =
        if read_file_bounded(&ready, 64)?.as_deref() == Some(STORAGE_NAMESPACE_RESET_BYTES) {
            storage_namespace_witness(&data_root)?
        } else {
            established_storage_namespace(&data_root)?
        };
    // The outside ready marker becomes a durable bulk-reset intent. Manager
    // creation/imports require its completed value, so a cancelled/failed reset
    // cannot accept a new identity that a later replay would accidentally erase.
    let reset =
        durable_write(&ready, STORAGE_NAMESPACE_RESET_BYTES).map_err(std::io::Error::other)?;
    finish_identity_storage_reset(ratspeak_dir, &witness, &reset)?;
    durable_write(&ready, STORAGE_NAMESPACE_BYTES).map_err(std::io::Error::other)?;
    Ok(())
}

fn finish_identity_storage_reset(
    ratspeak_dir: &Path,
    witness: &DurableFileReceipt,
    reset: &DurableFileReceipt,
) -> std::io::Result<()> {
    let identities = ratspeak_dir.join("identities");
    // Unlink before interpreting hash-named directories or metadata. Even a
    // link named like an identity or deletion record remains only a link inside
    // the authorized reset tree; its destination never enters deletion scope.
    for entry in std::fs::read_dir(&identities)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            unlink_namespace_symlink(&entry.path(), reset)?;
        }
    }
    let mut hashes = HashSet::new();
    for entry in std::fs::read_dir(&identities)? {
        let name = entry?.file_name();
        if let Some(name) = name.to_str() {
            let hash = name.split('.').next().unwrap_or(name);
            if crate::helpers::is_protocol_hash_16(hash) {
                hashes.insert(hash.to_owned());
            }
        }
    }
    for hash in hashes {
        let deletion = identities.join(format!("{hash}.namespace-delete"));
        if deletion.try_exists()? {
            retire_identity_namespace(ratspeak_dir, &hash, deletion_scope(&deletion)?, witness)?;
        }
        retire_identity_namespace(ratspeak_dir, &hash, true, witness)?;
    }
    // Preserve the previous reset's full child wipe, including unknown folders
    // and files. The established root and its outside publication journals stay
    // in place. A symlink is unlinked without following its destination.
    for entry in std::fs::read_dir(&identities)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            unlink_namespace_symlink(&entry.path(), reset)?;
        } else if kind.is_dir() {
            retire_directory(&entry.path(), reset)?;
        } else if kind.is_file() {
            durable_remove(&entry.path(), reset).map_err(std::io::Error::other)?;
        } else {
            return Err(std::io::Error::other(
                "unsupported entry in identity reset namespace",
            ));
        }
    }
    // A restart may resume after the last unlink/remove but before its parent
    // barrier. Renew the exact identities parent without republishing its root.
    durable_remove(&identities.join(".ratspeak-reset-barrier"), reset)
        .map_err(std::io::Error::other)?;
    Ok(())
}

fn established_identity_namespace(ratspeak_dir: &Path, hash: &str) -> std::io::Result<()> {
    durable_reconcile(
        &identity_namespace_marker(ratspeak_dir, hash),
        IDENTITY_NAMESPACE_BYTES,
    )
    .map_err(std::io::Error::other)?;
    let directory = ratspeak_dir.join("identities").join(hash);
    for path in [
        directory.clone(),
        directory.join("lxmf"),
        directory.join("ratchets"),
        directory.join("ratchets/received"),
    ] {
        require_real_directory(&path)?;
    }
    Ok(())
}

/// Establish one identity's private namespace before any users of its files.
/// The app-wide namespace must already be initialized. Existing committed
/// identities are validated without republishing directories containing live
/// file handles. Only initial startup or creation/import may recover an
/// incomplete identity namespace.
pub(crate) fn initialize_identity_storage_namespace(
    ratspeak_dir: &Path,
    hash: &str,
) -> std::io::Result<()> {
    if !crate::helpers::is_protocol_hash_16(hash) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid identity namespace hash",
        ));
    }
    let data_root = data_root_for_identity_namespace(ratspeak_dir)?;
    let witness = established_storage_namespace(&data_root)?;
    let deletion = ratspeak_dir
        .join("identities")
        .join(format!("{hash}.namespace-delete"));
    if deletion.try_exists()? {
        retire_identity_namespace(ratspeak_dir, hash, deletion_scope(&deletion)?, &witness)?;
    }
    let directory = ratspeak_dir.join("identities").join(hash);
    if !identity_namespace_marker(ratspeak_dir, hash).try_exists()?
        && (directory.join("ratchets").try_exists()? || directory.join("lxmf").try_exists()?)
    {
        return Err(std::io::Error::other(
            "existing identity namespace is incomplete; reconcile it before opening database/storage users",
        ));
    }
    prepare_identity_namespace(ratspeak_dir, hash, &witness)
}

fn prepare_identity_namespace(
    ratspeak_dir: &Path,
    hash: &str,
    witness: &DurableFileReceipt,
) -> std::io::Result<()> {
    let deletion = ratspeak_dir
        .join("identities")
        .join(format!("{hash}.namespace-delete"));
    if deletion.try_exists()? {
        retire_identity_namespace(ratspeak_dir, hash, deletion_scope(&deletion)?, witness)?;
    }
    let marker = identity_namespace_marker(ratspeak_dir, hash);
    if marker.try_exists()? {
        return established_identity_namespace(ratspeak_dir, hash);
    }
    let directory = ratspeak_dir.join("identities").join(hash);
    for path in [
        directory.clone(),
        directory.join("lxmf"),
        directory.join("ratchets"),
        directory.join("ratchets/received"),
    ] {
        durable_create_dir_all(&path, witness).map_err(std::io::Error::other)?;
    }
    durable_write(&marker, IDENTITY_NAMESPACE_BYTES).map_err(std::io::Error::other)?;
    Ok(())
}

/// Establish/reconcile the app's private directory tree before configuration,
/// database pools, identity files or network workers are opened. Call once at
/// process startup; live identity rebuilds only validate its committed marker.
/// Takes the profile owner lock, and refuses a newer build's database, before
/// reconciling anything.
pub fn initialize_storage_namespace(data_root: &Path) -> std::io::Result<()> {
    // Platform-provided roots already exist. A fresh explicit/test root may
    // need multiple ancestors, all established from an existing outside anchor.
    if !data_root.try_exists()? {
        let mut anchor = data_root
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        while !anchor.try_exists()? {
            anchor = anchor
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
        }
        let digest = hex::encode(rns_crypto::sha::sha256(
            data_root.as_os_str().as_encoded_bytes(),
        ));
        let witness = durable_write(
            &anchor.join(format!(".ratspeak-root-{digest}.witness")),
            STORAGE_NAMESPACE_BYTES,
        )
        .map_err(std::io::Error::other)?;
        durable_create_dir_all(data_root, &witness).map_err(std::io::Error::other)?;
    }
    require_real_directory(data_root)?;
    // Reconciliation may retire identities; never run it beside another owner,
    // or on a newer build's profile, whose recorded intents may differ.
    crate::owner_lock::acquire_owner_lock(data_root)?;
    let ratspeak_dir = data_root.join(".ratspeak");
    // The version check must not follow a linked `.ratspeak` either.
    match std::fs::symlink_metadata(&ratspeak_dir) {
        Ok(_) => require_real_directory(&ratspeak_dir)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    db::refuse_newer_database(data_root).map_err(std::io::Error::other)?;
    if data_root.join(STORAGE_NAMESPACE_READY).try_exists()? {
        // Completed roots cannot become fresh merely because directories have
        // disappeared. Only an explicit recorded deletion may retire identity
        // descendants; the app/database roots remain established.
        let ready = data_root.join(STORAGE_NAMESPACE_READY);
        if read_file_bounded(&ready, 64)?.as_deref() == Some(STORAGE_NAMESPACE_RESET_BYTES) {
            let witness = storage_namespace_witness(data_root)?;
            let reset = durable_reconcile(&ready, STORAGE_NAMESPACE_RESET_BYTES)
                .map_err(std::io::Error::other)?;
            finish_identity_storage_reset(&ratspeak_dir, &witness, &reset)?;
            durable_write(&ready, STORAGE_NAMESPACE_BYTES).map_err(std::io::Error::other)?;
        } else {
            established_storage_namespace(data_root)?;
        }
    }
    let witness = durable_write(
        &data_root.join(STORAGE_NAMESPACE_WITNESS),
        STORAGE_NAMESPACE_BYTES,
    )
    .map_err(std::io::Error::other)?;
    let identities = ratspeak_dir.join("identities");
    durable_create_dir_all(&ratspeak_dir, &witness).map_err(std::io::Error::other)?;
    // Builds before private folders left it readable by every local user.
    make_private(&ratspeak_dir)?;
    durable_create_dir_all(&identities, &witness).map_err(std::io::Error::other)?;
    for entry in std::fs::read_dir(&identities)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(hash) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".namespace-delete"))
            .filter(|hash| crate::helpers::is_protocol_hash_16(hash))
        {
            retire_identity_namespace(
                &ratspeak_dir,
                hash,
                deletion_scope(&entry.path())?,
                &witness,
            )?;
        }
    }
    for entry in std::fs::read_dir(&identities)? {
        let entry = entry?;
        let name = entry.file_name();
        if let Some(hash) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".namespace-ready"))
            .filter(|hash| crate::helpers::is_protocol_hash_16(hash))
        {
            established_identity_namespace(&ratspeak_dir, hash)?;
        }
        if entry.file_type()?.is_dir()
            && let Some(hash) = name
                .to_str()
                .filter(|hash| crate::helpers::is_protocol_hash_16(hash))
        {
            prepare_identity_namespace(&ratspeak_dir, hash, &witness)?;
        }
    }
    durable_write(
        &data_root.join(STORAGE_NAMESPACE_READY),
        STORAGE_NAMESPACE_BYTES,
    )
    .map_err(std::io::Error::other)?;
    Ok(())
}

/// Why an identity was not imported. Nothing it wrote is left behind.
#[derive(Debug, thiserror::Error)]
pub enum IdentityImportError {
    #[error("Identity already exists")]
    Exists,
    #[error("{0}")]
    InvalidKey(String),
    /// Its recovery phrase could not be stored.
    #[error("recovery phrase not stored: {0}")]
    Phrase(crate::vault::VaultError),
    #[error("{0}")]
    Storage(String),
}

/// Takes back what a failed import wrote. A namespace it created goes whole,
/// through its recorded deletion, which a restart finishes; in one that was
/// there before, only the files it created go.
fn discard_failed_import(ratspeak_dir: &Path, hash: &str, new_namespace: bool, created: &[&Path]) {
    if new_namespace {
        let retired = data_root_for_identity_namespace(ratspeak_dir)
            .and_then(|root| established_storage_namespace(&root))
            .and_then(|witness| retire_identity_namespace(ratspeak_dir, hash, true, &witness));
        match retired {
            Ok(()) => return,
            Err(error) => tracing::warn!(%error, "could not retire a failed import's namespace"),
        }
    }
    for path in created {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, "could not remove a failed import's file"),
        }
    }
}

/// Leaves `dir` to its owner alone, as a fresh profile's folders are. Unix
/// only; elsewhere the platform's own folders already are.
pub fn make_private(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Stores an attachment in an identity's `files` directory, under a fresh name.
pub(crate) fn save_attachment_in(
    files_dir: &Path,
    file_name: &str,
    data: &[u8],
) -> std::io::Result<String> {
    use std::io::Write;

    let stored_name = attachment_storage_name(file_name);
    let path = files_dir.join(&stored_name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    if let Err(error) = file.write_all(data).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(stored_name)
}

impl LxmfManager {
    /// Load/create a manager after [`initialize_storage_namespace`] has completed
    /// before database or configuration users are opened.
    pub fn load_or_create(
        data_dir: &Path,
        preferred_identity_hash: Option<&str>,
        hw_pin: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::load_or_create_with_vault_job(
            data_dir,
            preferred_identity_hash,
            hw_pin.as_deref(),
            None,
        )
    }

    pub(crate) fn load_or_create_with_vault_job(
        data_dir: &Path,
        preferred_identity_hash: Option<&str>,
        hw_pin: Option<&str>,
        job: Option<&crate::vault::VaultJob>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if preferred_identity_hash
            .filter(|hash| !hash.is_empty())
            .is_some_and(|hash| !crate::helpers::is_protocol_hash_16(hash))
        {
            return Err("active identity hash is not a canonical 16-byte protocol hash".into());
        }
        let ratspeak_dir = data_dir.join(".ratspeak");
        established_storage_namespace(data_dir)?;

        let identities_dir = ratspeak_dir.join("identities");

        let legacy_path = ratspeak_dir.join("identity");
        let mut is_hardware = false;
        let identity = if let Some(hash) = preferred_identity_hash.filter(|h| !h.is_empty()) {
            let id_file = identities_dir.join(hash).join("identity");
            let hwid_file = identities_dir.join(hash).join("identity.hwid");
            let enc_file = identities_dir.join(hash).join("identity.enc");
            if hwid_file.exists() {
                is_hardware = true;
                load_hwid_identity(&hwid_file, hash, hw_pin)?
            } else if enc_file.exists() {
                // Passcode-protected software identity; `hw_pin` carries the passcode.
                load_encrypted_identity(&enc_file, hash, hw_pin, job)?
            } else if id_file.exists() {
                tracing::info!(identity = %crate::short_id(hash), "Loading active identity from profile");
                bounded_plain_identity_from_file(&id_file)?
            } else if legacy_path.exists() {
                let id = bounded_plain_identity_from_file(&legacy_path)?;
                let legacy_hash = hex::encode(id.hash);
                if legacy_hash == hash {
                    let id_dir = identities_dir.join(hash);
                    initialize_identity_storage_namespace(&ratspeak_dir, hash)?;
                    id.to_file(&id_dir.join("identity"))?;
                    id
                } else {
                    return Err(format!("active identity file not found for {hash}").into());
                }
            } else {
                return Err(format!("active identity file not found for {hash}").into());
            }
        } else if legacy_path.exists() {
            tracing::info!("Loading identity from legacy profile");
            bounded_plain_identity_from_file(&legacy_path)?
        } else {
            let mut found = None;
            if identities_dir.is_dir()
                && let Ok(entries) = std::fs::read_dir(&identities_dir)
            {
                for entry in entries.flatten() {
                    let id_file = entry.path().join("identity");
                    if id_file.exists() {
                        found = Some(bounded_plain_identity_from_file(&id_file)?);
                        break;
                    }
                }
            }

            match found {
                Some(id) => id,
                None => {
                    tracing::info!("No identity found, generating new one");
                    let id = Identity::new();
                    id.to_file(&legacy_path)?;
                    id
                }
            }
        };

        Self::from_loaded_identity(data_dir, identity, is_hardware)
    }

    /// Rebuild a same-identity network session without saving an unlocked
    /// identity or asking the user to enter its PIN/passcode again.
    pub(crate) fn from_loaded_identity(
        data_dir: &Path,
        identity: Identity,
        is_hardware: bool,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let ratspeak_dir = data_dir.join(".ratspeak");
        let identities_dir = ratspeak_dir.join("identities");
        let legacy_path = ratspeak_dir.join("identity");
        let identity_hash = hex::encode(identity.hash);

        let id_dir = identities_dir.join(&identity_hash);
        let witness = established_storage_namespace(data_dir)?;
        if id_dir.try_exists()? {
            established_identity_namespace(&ratspeak_dir, &identity_hash)?;
        } else {
            prepare_identity_namespace(&ratspeak_dir, &identity_hash, &witness)?;
        }

        let id_file = id_dir.join("identity");
        if !id_file.exists()
            && legacy_path.exists()
            && !is_hardware
            && !id_dir.join("identity.enc").exists()
        {
            std::fs::copy(&legacy_path, &id_file)?;
        }

        let lxmf_storage = id_dir.join("lxmf");

        let lxmf_dest_hash =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hash = hex::encode(lxmf_dest_hash);

        let propagation_dest_hash =
            Destination::hash_from_name_and_identity("lxmf.propagation", Some(&identity.hash));

        tracing::info!(
            identity = %crate::short_id(&identity_hash),
            lxmf = %crate::short_id(&lxmf_hash),
            "Identity loaded"
        );

        let mut router = LxmRouter::new(RouterConfig::default());
        if router.load_state(&lxmf_storage).is_err() {
            tracing::warn!(reason = "load_failed", "failed to load LXMF router state");
        }

        let ratchet_dir = id_dir.join("ratchets");
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let delivery_ratchets = DeliveryRatchetState::load_or_initialize(
            &identity,
            lxmf_dest_hash,
            ratchet_dir.join("ring"),
            ratchet_dir.join("ring.control"),
            wall_now,
        )?;

        // Validate, expire and load each file exactly once. The former
        // clean-then-load sequence decoded every ratchet twice at startup.
        let received_dir = ratchet_dir.join("received");
        let mut received_ratchets = HashMap::new();
        let mut removed = 0usize;
        if let Ok(entries) = std::fs::read_dir(&received_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                match received_ratchet_hash_from_path(&path)
                    .zip(ReceivedRatchet::load(&path).ok())
                    .filter(|(_, ratchet)| !ratchet.is_expired())
                {
                    Some((name, ratchet)) => {
                        received_ratchets.insert(name.to_string(), ratchet);
                    }
                    None => match std::fs::remove_file(&path) {
                        Ok(()) => removed += 1,
                        Err(error) => tracing::warn!(
                            %error,
                            "failed to remove expired or corrupt received ratchet"
                        ),
                    },
                }
            }
        }
        if removed > 0 {
            tracing::info!(removed, "swept expired received-ratchet files at startup");
        }

        // Binary: repeated [dest_hash:16][pubkey:64] records.
        let ki_path = ratchet_dir.join("known_identities");
        let mut known_identities: HashMap<String, [u8; 64]> = HashMap::new();
        if let Some(data) = ki_path
            .exists()
            .then(|| std::fs::read(&ki_path))
            .and_then(Result::ok)
        {
            let mut pos = 0;
            while pos + 80 <= data.len() {
                let mut dh = [0u8; 16];
                dh.copy_from_slice(&data[pos..pos + 16]);
                let mut pk = [0u8; 64];
                pk.copy_from_slice(&data[pos + 16..pos + 80]);
                known_identities.insert(hex::encode(dh), pk);
                pos += 80;
            }
        }

        tracing::info!(
            ratchet_keys = delivery_ratchets.ring().len(),
            received_ratchets = received_ratchets.len(),
            known_identities = known_identities.len(),
            "Crypto state loaded"
        );

        let propagation_node_policies =
            lxmf_core::persist::load_node_policies(&lxmf_storage).unwrap_or_default();
        let propagation_transfer_limits_kb = propagation_node_policies
            .iter()
            .map(|(node, p)| (*node, p.transfer_limit_kb as u64))
            .collect();
        let (backchannel_abandon_tx, backchannel_abandon_rx) = mpsc::channel(256);
        Ok(Self {
            identity,
            is_hardware,
            identity_hash,
            lxmf_hash,
            lxmf_dest_hash,
            propagation_dest_hash,
            router,
            data_dir: ratspeak_dir,
            lxmf_storage_dir: lxmf_storage,
            display_name: String::new(),
            status: String::new(),
            announce_ratspeak_usage: true,
            delivery_ratchets,
            announce_cache_started: Instant::now(),
            received_ratchets,
            dirty_received_ratchets: HashSet::new(),
            known_identities,
            known_identities_revision: 0,
            known_identities_persisted_revision: 0,
            peer_lxmf_compression_support: HashMap::new(),
            route_hops: HashMap::new(),
            route_entries: HashMap::new(),
            path_recovery: None,
            shared_recovery_owner: None,
            shared_recovery_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(8)),
            pending_path_recoveries: HashMap::new(),
            route_snapshot_started: Instant::now(),
            path_recovery_refresh_needed: false,
            delivery_timing: timing::DeliveryTimingCache::default(),
            delivery_tx: None,
            link_delivery: None,
            endpoint_dispatch: None,
            lxmf_link_command_tx: None,
            lxmf_direct_link_packet_tx: None,
            direct_inbound_resource_accept_handler: None,
            direct_inbound_resource_concluded_handler: None,
            direct_inbound_resource_completion_handler: None,
            pending_direct_link_identifications: HashMap::new(),
            lxmf_backchannel_command_rx: None,
            backchannel_abandon_tx,
            backchannel_abandon_rx,
            pending_backchannel_abandon: None,
            backchannel_bridge_slots: Arc::new(tokio::sync::Semaphore::new(256)),
            lxmf_link_identified_rx: None,
            lxmf_backchannel_event_rx: None,
            pending_backchannel_resource_cancellations: std::collections::VecDeque::new(),
            opportunistic_proof_tx: None,
            opportunistic_in_flight: HashMap::new(),
            opportunistic_proofs: HashMap::new(),
            packet_delivery_rtts: Vec::new(),
            propagation_sync: None,
            propagation_client: None,
            delivery_limit_kb: lxmf_core::constants::DELIVERY_LIMIT as f64,
            last_propagation_check: 0.0,
            last_router_cull: 0.0,
            client_propagation_enabled: false,
            configured_propagation_node: None,
            propagation_transfer_limits_kb,
            propagation_node_policies,
            propagation_envelopes: HashMap::new(),
            pending_propagation_envelopes: HashMap::new(),
            propagation_preparation_tasks: Vec::new(),
            last_ratchet_clean: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            received_ratchets_dir: received_dir,
            pending_expired_received_ratchets: Vec::new(),
            in_flight_propagation: std::collections::HashMap::new(),
            completed_propagation_deposits: Vec::new(),
            failed_propagation_deposits: Vec::new(),
            completed_propagation_syncs: Vec::new(),
            failed_propagation_syncs: Vec::new(),
            downloaded_propagation_messages: Vec::new(),
            download_state: None,
            download_suppression_revision: 0,
            last_propagation_admission: Default::default(),
            delivery_progress_updates: Vec::new(),
            delivery_failure_updates: Vec::new(),
            ephemeral_outbound: HashSet::new(),
            last_reported_steps: HashMap::new(),
            auto_live_fallback: HashSet::new(),
            live_preparation: preparation::LivePreparation::default(),
            resumable: Vec::new(),
            deferred_failures: HashSet::new(),
        })
    }

    pub fn create_identity(
        &self,
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        let identity = Identity::new();
        let hash_hex = hex::encode(identity.hash);

        let lxmf_dest =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hex = hex::encode(lxmf_dest);

        let id_dir = self.data_dir.join("identities").join(&hash_hex);
        initialize_identity_storage_namespace(&self.data_dir, &hash_hex)?;
        identity.to_file(&id_dir.join("identity"))?;

        let display_name = if nickname.is_empty() {
            format!("!Ratspeak.org-{}", &lxmf_hex[..6])
        } else {
            nickname.to_string()
        };

        db::save_identity(db_pool, &hash_hex, &lxmf_hex, nickname, &display_name);

        tracing::info!(identity = %crate::short_id(&hash_hex), "Created new identity");
        Ok((hash_hex, lxmf_hex))
    }

    pub(crate) fn set_lxmf_link_control(
        &mut self,
        command_tx: mpsc::Sender<rns_runtime::link_manager::LinkManagerCommand>,
        direct_link_packet_tx: mpsc::UnboundedSender<(Vec<u8>, [u8; 16])>,
        identified_rx: mpsc::Receiver<([u8; 16], [u8; 16])>,
        backchannel_event_rx: mpsc::UnboundedReceiver<BackchannelLinkEvent>,
    ) {
        self.lxmf_link_command_tx = Some(command_tx);
        self.lxmf_direct_link_packet_tx = Some(direct_link_packet_tx);
        self.lxmf_link_identified_rx = Some(identified_rx);
        self.lxmf_backchannel_event_rx = Some(backchannel_event_rx);
        self.lxmf_backchannel_command_rx = None;
        self.ensure_link_delivery_backchannel_sender();
        self.ensure_link_delivery_inbound_sender();
        self.ensure_link_delivery_resource_handlers();
    }

    pub(crate) fn set_direct_inbound_resource_handlers<A, C>(&mut self, accept: A, concluded: C)
    where
        A: Fn([u8; 16], &rns_protocol::resource_adv::ResourceAdvertisement) -> bool
            + Send
            + Sync
            + 'static,
        C: Fn([u8; 16], [u8; 32]) + Send + Sync + 'static,
    {
        self.direct_inbound_resource_accept_handler = Some(Arc::new(accept));
        self.direct_inbound_resource_concluded_handler = Some(Arc::new(concluded));
        self.ensure_link_delivery_resource_handlers();
    }

    pub(crate) fn set_link_endpoint_dispatch_handle(
        &mut self,
        handle: rns_transport::link_endpoint_dispatch::LinkEndpointDispatchHandle,
    ) {
        if let Some(delivery) = &mut self.link_delivery {
            delivery.set_link_endpoint_dispatch_handle(handle.clone());
        }
        self.endpoint_dispatch = Some(handle);
    }

    pub(crate) fn set_direct_inbound_resource_completion_handler<F>(&mut self, handler: F)
    where
        F: Fn([u8; 16], [u8; 32], Vec<u8>) + Send + Sync + 'static,
    {
        self.direct_inbound_resource_completion_handler = Some(Arc::new(handler));
        self.ensure_link_delivery_resource_handlers();
    }

    pub fn set_opportunistic_proof_sender(
        &mut self,
        proof_tx: mpsc::UnboundedSender<rns_transport::link_messages::DestinationEvent>,
    ) {
        self.opportunistic_proof_tx = Some(proof_tx);
    }

    pub fn note_pending_direct_backchannel(&mut self, dest_hash: [u8; 16], link_id: [u8; 16]) {
        self.pending_direct_link_identifications.insert(
            dest_hash,
            PendingDirectLinkIdentification {
                link_id,
                observed_at: Instant::now(),
            },
        );
    }

    /// `key_bytes` must be exactly 64 bytes (X25519 || Ed25519 seed).
    pub fn import_identity(
        &self,
        key_bytes: &[u8],
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        Self::import_identity_to_data_dir(&self.data_dir, key_bytes, nickname, db_pool)
    }

    pub fn import_identity_to_data_dir(
        ratspeak_dir: &Path,
        key_bytes: &[u8],
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self::import_identity_with_phrase(
            ratspeak_dir,
            key_bytes,
            None,
            nickname,
            db_pool,
        )?)
    }

    /// Imports a private identity, with the recovery phrase it derives from
    /// when it has one. The phrase is stored and read back before the key,
    /// and the row comes last: an identity that fails leaves none of them.
    pub fn import_identity_with_phrase(
        ratspeak_dir: &Path,
        key_bytes: &[u8],
        phrase: Option<(&str, &crate::vault::VaultJob)>,
        nickname: &str,
        db_pool: &DbPool,
    ) -> Result<(String, String), IdentityImportError> {
        if key_bytes.len() != 64 {
            return Err(IdentityImportError::InvalidKey(
                "Identity key must be exactly 64 bytes".into(),
            ));
        }

        let identity = Identity::from_private_key(key_bytes)
            .map_err(|e| IdentityImportError::InvalidKey(format!("Invalid identity key: {e}")))?;
        let hash_hex = hex::encode(identity.hash);

        let id_dir = ratspeak_dir.join("identities").join(&hash_hex);
        let key_path = id_dir.join("identity");
        if key_path.exists() {
            return Err(IdentityImportError::Exists);
        }

        let lxmf_dest =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity.hash));
        let lxmf_hex = hex::encode(lxmf_dest);

        let storage =
            |error: &dyn std::fmt::Display| IdentityImportError::Storage(error.to_string());
        let new_namespace = !id_dir.try_exists().map_err(|e| storage(&e))?;
        initialize_identity_storage_namespace(ratspeak_dir, &hash_hex).map_err(|e| storage(&e))?;
        let discard = |created: &[&Path]| {
            discard_failed_import(ratspeak_dir, &hash_hex, new_namespace, created);
        };
        let seed_path = id_dir.join(crate::vault::SEED_FILE);
        let mut created = Vec::new();
        if let Some((phrase, job)) = phrase {
            let seed_existed = seed_path.try_exists().map_err(|e| storage(&e))?;
            if !seed_existed {
                created.push(seed_path.as_path());
            }
            if let Err(error) = job.store_plaintext_seed(&id_dir, phrase) {
                // An uncertain or unverified write may have left this phrase.
                if seed_existed && matches!(error, crate::vault::VaultError::RecoveryRequired(_)) {
                    created.push(seed_path.as_path());
                }
                discard(&created);
                return Err(IdentityImportError::Phrase(error));
            }
        }
        created.push(key_path.as_path());
        if let Err(error) = identity.to_file(&key_path) {
            discard(&created);
            return Err(storage(&error));
        }

        let display_name = if nickname.is_empty() {
            format!("!Ratspeak.org-{}", &lxmf_hex[..6])
        } else {
            nickname.to_string()
        };
        if let Err(error) =
            db::try_save_identity(db_pool, &hash_hex, &lxmf_hex, nickname, &display_name)
        {
            discard(&created);
            return Err(storage(&error));
        }

        tracing::info!(identity = %crate::short_id(&hash_hex), "Imported identity");
        Ok((hash_hex, lxmf_hex))
    }

    pub fn purge_identity_profile(
        data_root: &Path,
        hash_hex: &str,
        cascade: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if !crate::helpers::is_protocol_hash_16(hash_hex) {
            return Err("invalid identity namespace hash".into());
        }
        // The hub keyfile lives outside the identity directory, so it must be
        // removed explicitly and before the early return. Leaving it behind
        // lets a delete-and-reimport resurrect the hub on the same destination
        // hash with an empty registry: every closed room open again, every ban
        // and kline gone, and bookmarked clients reconnecting straight into it.
        let hub_key = data_root
            .join(".ratspeak")
            .join("channel_hub")
            .join(format!("hub_identity_{hash_hex}"));
        if hub_key.exists() {
            std::fs::remove_file(&hub_key)?;
        }

        let witness = established_storage_namespace(data_root)?;
        retire_identity_namespace(&data_root.join(".ratspeak"), hash_hex, cascade, &witness)?;
        Ok(())
    }

    pub fn contact_card_public_key(&self, hash_hex: &str) -> Option<[u8; 64]> {
        if self.identity_hash == hash_hex {
            return Some(self.identity.get_public_key());
        }

        contact_card_public_key_from_profile(&self.data_dir, hash_hex)
    }

    fn peer_lxmf_compression_support(
        &self,
        db_pool: Option<&DbPool>,
        dest_hash_hex: &str,
    ) -> CompressionSupport {
        let dest_bytes = match hex::decode(dest_hash_hex) {
            Ok(bytes) if bytes.len() == 16 => bytes,
            _ => return CompressionSupport::Unknown,
        };
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);
        if let Some(support) = self.peer_lxmf_compression_support.get(&dest).copied() {
            return support;
        }

        db_pool
            .and_then(|pool| db::get_identity_lxmf_compression_support(pool, dest_hash_hex))
            .as_deref()
            .map(|support| match support {
                db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED => CompressionSupport::Unsupported,
                db::LXMF_COMPRESSION_SUPPORT_SUPPORTED => CompressionSupport::Supported,
                _ => CompressionSupport::Unknown,
            })
            .unwrap_or(CompressionSupport::Unknown)
    }

    /// Select the format for a new recording using the existing destination-bound
    /// compression announcement/cache. Explicit no-compression selects compact;
    /// unknown and supported peers retain Standard. This product policy is not a
    /// claim that no-compression is a codec capability. Freeze the returned mode
    /// in the recording; later announcements never reinterpret recorded bytes.
    pub fn voice_message_format(&self, db_pool: &DbPool, dest_hash_hex: &str) -> u8 {
        if self.peer_lxmf_compression_support(Some(db_pool), dest_hash_hex)
            == CompressionSupport::Unsupported
        {
            lxmf_core::constants::AM_CODEC2_700C
        } else {
            lxmf_core::constants::AM_OPUS_OGG
        }
    }

    fn apply_peer_lxmf_compression_support(
        &self,
        msg: &mut MessageMetadata,
        db_pool: Option<&DbPool>,
        dest_hash_hex: &str,
    ) {
        if self.peer_lxmf_compression_support(db_pool, dest_hash_hex)
            == CompressionSupport::Unsupported
        {
            msg.auto_compress = false;
        }
    }

    /// Pick the most truthful `DeliveryMethod` for an outbound send so the
    /// persisted `messages.delivery_method` and the wire method reflect the
    /// user's choice or Ratspeak's Auto policy.
    pub fn pick_delivery_method(
        &self,
        _db_pool: &DbPool,
        dest_hash_hex: &str,
        preference: DeliveryPreference,
        profile: DeliveryProfile,
    ) -> DeliveryMethod {
        self.delivery_method_for(dest_hash_hex, preference, profile)
    }

    fn delivery_method_for(
        &self,
        dest_hash_hex: &str,
        preference: DeliveryPreference,
        profile: DeliveryProfile,
    ) -> DeliveryMethod {
        match preference {
            DeliveryPreference::Opportunistic => DeliveryMethod::Opportunistic,
            DeliveryPreference::Direct => DeliveryMethod::Direct,
            DeliveryPreference::Propagated => DeliveryMethod::Propagated,
            DeliveryPreference::Auto => self.auto_live_method(dest_hash_hex, profile),
        }
    }

    /// `Opportunistic` is the entry-level method; the lxmf-core router escalates
    /// to `Direct` when the packed payload exceeds a single RNS packet.
    /// `Propagated` forces the propagation-node path once the recipient identity
    /// key is known.
    pub fn create_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        delivery_method: DeliveryMethod,
    ) -> Option<SignedLxMessage> {
        self.create_message_with_custom_fields(dest_hash_hex, content, title, delivery_method, &[])
    }

    fn create_message_with_custom_fields(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        delivery_method: DeliveryMethod,
        custom_fields: &[(u8, Vec<u8>)],
    ) -> Option<SignedLxMessage> {
        let dest_bytes = hex::decode(dest_hash_hex).ok()?;
        if dest_bytes.len() != 16 {
            return None;
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let mut msg =
            LxMessageDraft::new(dest, self.lxmf_dest_hash, title, content, delivery_method).ok()?;
        self.apply_peer_lxmf_compression_support(&mut msg, None, dest_hash_hex);

        msg.include_ticket = true;

        for (field_id, bytes) in custom_fields {
            // FIELD_REACTION (dict) and FIELD_RENDERER (int) are native msgpack
            // on the wire; everything else we emit packs as bin.
            if matches!(
                *field_id,
                lxmf_core::constants::FIELD_REACTION | lxmf_core::constants::FIELD_RENDERER
            ) {
                msg.set_msgpack_field(*field_id, bytes.clone()).ok()?;
            } else {
                msg.set_field(*field_id, bytes.clone()).ok()?;
            }
        }

        let intent = self.router.prepare_outbound(&mut msg).ok()?;
        intent.persist(&self.lxmf_storage_dir, &mut msg).ok()?;
        let identity = self.identity.clone();
        msg.sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
            .ok()
    }

    pub fn send_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Option<String> {
        self.send_message_with_preference(MessageSendRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            preference: DeliveryPreference::Auto,
            profile: DeliveryProfile::Message,
            renderer: None,
            thread: "",
        })
    }

    pub fn send_message_with_preference(
        &mut self,
        request: MessageSendRequest<'_>,
    ) -> Option<String> {
        self.send_message_with_preference_report(request)
            .ok()
            .map(|queued| queued.message_id)
    }

    pub fn send_message_with_preference_report(
        &mut self,
        request: MessageSendRequest<'_>,
    ) -> Result<LxmfQueuedMessage, LxmfSubmissionFailure> {
        let preference = request.preference;
        let method = self.pick_delivery_method(
            request.db_pool,
            request.dest_hash_hex,
            preference,
            request.profile,
        );
        self.send_message_with_method_internal(MessageWithMethodRequest {
            dest_hash_hex: request.dest_hash_hex,
            content: request.content,
            title: request.title,
            db_pool: request.db_pool,
            identity_id: request.identity_id,
            delivery_method: method,
            preference,
            profile: request.profile,
            reply_to_id: None,
            reply_to_preview: None,
            renderer: request.renderer,
            thread: request.thread,
        })
        .ok_or(LxmfSubmissionFailure::PreparationFailed)
    }

    pub fn send_reply_with_preference(
        &mut self,
        request: ReplyMessageSendRequest<'_>,
    ) -> Option<String> {
        let preference = request.preference;
        let method = self.pick_delivery_method(
            request.db_pool,
            request.dest_hash_hex,
            preference,
            request.profile,
        );
        self.send_message_with_method_internal(MessageWithMethodRequest {
            dest_hash_hex: request.dest_hash_hex,
            content: request.content,
            title: request.title,
            db_pool: request.db_pool,
            identity_id: request.identity_id,
            delivery_method: method,
            preference,
            profile: request.profile,
            reply_to_id: Some(request.reply_to_id),
            reply_to_preview: Some(request.reply_to_preview),
            renderer: request.renderer,
            thread: request.thread,
        })
        .map(|queued| queued.message_id)
    }

    /// `DeliveryMethod::Propagated` requires `configured_propagation_node`.
    pub fn send_message_with_method(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
        db_pool: &DbPool,
        identity_id: &str,
        delivery_method: DeliveryMethod,
    ) -> Option<String> {
        self.send_message_with_method_internal(MessageWithMethodRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            delivery_method,
            preference: match delivery_method {
                DeliveryMethod::Opportunistic => DeliveryPreference::Opportunistic,
                DeliveryMethod::Direct => DeliveryPreference::Direct,
                DeliveryMethod::Propagated => DeliveryPreference::Propagated,
                DeliveryMethod::Paper => DeliveryPreference::Auto,
            },
            profile: DeliveryProfile::Message,
            reply_to_id: None,
            reply_to_preview: None,
            renderer: None,
            thread: "",
        })
        .map(|queued| queued.message_id)
    }

    fn send_message_with_method_internal(
        &mut self,
        request: MessageWithMethodRequest<'_>,
    ) -> Option<LxmfQueuedMessage> {
        let MessageWithMethodRequest {
            dest_hash_hex,
            content,
            title,
            db_pool,
            identity_id,
            delivery_method,
            preference,
            profile,
            reply_to_id,
            reply_to_preview,
            renderer,
            thread,
        } = request;
        // Paper never goes over the network; see `crate::paper`.
        if delivery_method == DeliveryMethod::Paper {
            return None;
        }
        let reply_to_id = reply_to_id.unwrap_or("");
        let reply_to_preview = reply_to_preview.unwrap_or("");
        let mut custom_fields = if reply_to_id.is_empty() {
            Vec::new()
        } else {
            ratspeak_chat_custom_fields(&RatspeakChatExtension::Reply {
                target: reply_to_id.to_string(),
                preview: reply_to_preview.to_string(),
                target_sender: None,
            })
            .unwrap_or_default()
        };
        custom_fields.extend(renderer.map(renderer_custom_field));
        custom_fields.extend(thread_custom_field(thread));
        let mut msg = self.create_message_with_custom_fields(
            dest_hash_hex,
            content,
            title,
            delivery_method,
            &custom_fields,
        )?;
        self.apply_peer_lxmf_compression_support(&mut msg, Some(db_pool), dest_hash_hex);
        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return None;
        }

        let msg_id = hex::encode(msg.message_id());
        let display_timestamp = db::next_conversation_observed_timestamp(
            db_pool,
            dest_hash_hex,
            identity_id,
            msg.timestamp(),
        );

        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, preference);
        let method = msg.method;
        let message_id = msg.message_id();
        let resend = crate::outbox::OutboxMessage::of(&msg).encode();
        self.router.submit_signed(msg).ok()?;
        let source = self.lxmf_hash.clone();
        let persisted = self.persist_outbound(
            db_pool,
            message_id,
            &db::OutboundMessage {
                msg_id: &msg_id,
                identity_id,
                source: &source,
                destination: dest_hash_hex,
                content,
                title,
                timestamp: display_timestamp,
                delivery_method: delivery_method_name(method),
                reply_to_id,
                reply_to_preview,
                renderer,
                thread,
                resend: db::Resend {
                    preference: preference.as_str(),
                    profile: profile.as_str(),
                    message: &resend,
                },
                ..db::OutboundMessage::default()
            },
        );
        if !persisted {
            return None;
        }
        self.auto_live_fallback.extend(auto_fallback);
        if preference == DeliveryPreference::Auto && profile == DeliveryProfile::Message {
            self.live_preparation.auto_messages.extend(auto_fallback);
        }

        Some(LxmfQueuedMessage {
            message_id: msg_id,
            method,
        })
    }

    /// Write the history row and outbox entry of a message the router just
    /// accepted. The manager lock is still held, so it cannot have left yet;
    /// if the write fails it leaves the queue instead.
    fn persist_outbound(
        &mut self,
        db_pool: &DbPool,
        message_id: [u8; 32],
        message: &db::OutboundMessage<'_>,
    ) -> bool {
        match db::save_outbound_message(db_pool, message) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "outbound message not persisted; withdrawn from the queue");
                self.router.cancel_outbound(&message_id);
                false
            }
        }
    }

    /// Take this identity's unfinished sends from the outbox; the tick queues
    /// them again. Returns the ids of records that cannot be read.
    pub fn resume_outbox(&mut self, rows: Vec<db::OutboxRow>, db_pool: &DbPool) -> Vec<String> {
        let mut unreadable = Vec::new();
        for row in rows {
            match self.resumable_send(
                &row,
                DeliveryPreference::parse(Some(&row.preference)),
                db_pool,
            ) {
                Some(send) => self.resumable.push(send),
                None => unreadable.push(row.message_id),
            }
        }
        unreadable
    }

    /// The size an outbox record rebuilds to, for transfer budgets; `None`
    /// when it cannot be read.
    pub fn outbox_send_size(row: &db::OutboxRow) -> Option<usize> {
        crate::outbox::OutboxMessage::decode(&row.message).map(|message| message.size())
    }

    fn resumable_send(
        &self,
        row: &db::OutboxRow,
        preference: DeliveryPreference,
        db_pool: &DbPool,
    ) -> Option<ResumableSend> {
        let message = crate::outbox::OutboxMessage::decode(&row.message)?;
        let dest_hex = hex::encode(message.destination());
        Some(ResumableSend {
            message_id: hex::decode(&row.message_id).ok()?.try_into().ok()?,
            auto_compress: self.peer_lxmf_compression_support(Some(db_pool), &dest_hex)
                != CompressionSupport::Unsupported,
            message,
            preference,
            profile: DeliveryProfile::parse(&row.profile)?,
            since: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
        })
    }

    /// Send a Not delivered or Cancelled message again, as the identical
    /// message (the recipient discards a copy it already has), by
    /// `preference`. The row is claimed first, so a second Retry cannot queue
    /// another copy; the lock holds the queue until it says `sending`.
    pub fn retry_outbound(
        &mut self,
        db_pool: &DbPool,
        identity_id: &str,
        row: &db::OutboxRow,
        preference: DeliveryPreference,
    ) -> Result<DeliveryMethod, &'static str> {
        const NOT_RETRYABLE: &str = "This message can't be sent again.";
        let send = self
            .resumable_send(row, preference, db_pool)
            .ok_or(NOT_RETRYABLE)?;
        let dest_hex = hex::encode(send.message.destination());
        let planned = self.delivery_method_for(&dest_hex, preference, send.profile);
        let previous = db::restart_outbound_message(
            db_pool,
            identity_id,
            &row.message_id,
            preference.as_str(),
            delivery_method_name(planned),
        )
        .ok()
        .flatten()
        .ok_or(NOT_RETRYABLE)?;
        match self.queue_resumed(&send, send.since) {
            Resumed::Queued(method) => {
                if method != planned {
                    db::update_message_delivery_method(
                        db_pool,
                        &row.message_id,
                        identity_id,
                        delivery_method_name(method),
                    );
                }
                Ok(method)
            }
            Resumed::Waiting | Resumed::Failed => {
                db::update_message_state(db_pool, &row.message_id, identity_id, &previous, None);
                Err("This message couldn't be queued. Try another method.")
            }
        }
    }

    fn queue_resumable(&mut self, now: f64, results: &mut Vec<(String, &'static str)>) {
        for send in std::mem::take(&mut self.resumable) {
            match self.queue_resumed(&send, now) {
                Resumed::Queued(_) => {}
                Resumed::Waiting => self.resumable.push(send),
                Resumed::Failed => results.push((hex::encode(send.message_id), "failed")),
            }
        }
    }

    /// Sign the stored parts again and queue the message as a send would. An
    /// Offline Inbox send waits a while for its relay; a rebuilt message that
    /// is not the one stored fails rather than go out.
    fn queue_resumed(&mut self, send: &ResumableSend, now: f64) -> Resumed {
        let dest_hex = hex::encode(send.message.destination());
        let method = self.delivery_method_for(&dest_hex, send.preference, send.profile);
        if method == DeliveryMethod::Propagated && self.router.outbound_propagation_node.is_none() {
            return if self.client_propagation_enabled && now - send.since < RESUME_RELAY_WAIT_SECS {
                Resumed::Waiting
            } else {
                Resumed::Failed
            };
        }
        let Ok(mut draft) = send.message.draft(method, &self.files_dir()) else {
            return Resumed::Failed;
        };
        draft.auto_compress = send.auto_compress;
        self.apply_peer_lxmf_compression_support(&mut draft, None, &dest_hex);
        let Ok(intent) = self.router.prepare_outbound(&mut draft) else {
            return Resumed::Failed;
        };
        if intent.persist(&self.lxmf_storage_dir, &mut draft).is_err() {
            return Resumed::Failed;
        }
        let identity = self.identity.clone();
        let Ok(mut msg) =
            draft.sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
        else {
            return Resumed::Failed;
        };
        if msg.message_id() != send.message_id {
            tracing::warn!(
                msg_id = %crate::short_id(&hex::encode(send.message_id)),
                "outbox record does not rebuild its message; not sent"
            );
            return Resumed::Failed;
        }
        normalize_protocol_delivery_method(&mut msg);
        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, send.preference);
        let method = msg.method;
        if self.router.submit_signed(msg).is_err() {
            return Resumed::Failed;
        }
        self.deferred_failures.remove(&send.message_id);
        self.auto_live_fallback.extend(auto_fallback);
        if send.preference == DeliveryPreference::Auto && send.profile == DeliveryProfile::Message {
            self.live_preparation.auto_messages.extend(auto_fallback);
        }
        Resumed::Queued(method)
    }

    pub fn send_ephemeral_opportunistic_message(
        &mut self,
        dest_hash_hex: &str,
        content: &str,
        title: &str,
    ) -> bool {
        let mut msg =
            match self.create_message(dest_hash_hex, content, title, DeliveryMethod::Opportunistic)
            {
                Some(msg) => msg,
                None => return false,
            };
        normalize_protocol_delivery_method(&mut msg);
        if msg.method != DeliveryMethod::Opportunistic || !message_within_resource_limit(&msg) {
            return false;
        }
        self.ephemeral_outbound.insert(msg.message_id());
        self.preempt_opportunistic_path(&mut msg);
        self.router.submit_signed(msg).is_ok()
    }

    /// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes]]`.
    /// FIELD_IMAGE 0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
    pub fn send_message_with_attachment_fields(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Option<String> {
        self.send_message_with_attachment_fields_preference(request)
    }

    /// FIELD_FILE_ATTACHMENTS 0x05 = msgpack `[[filename, bytes]]`.
    /// FIELD_IMAGE 0x06 = msgpack `[format, bytes]` (`png`, `webp`, ...).
    pub fn send_message_with_attachment_fields_preference(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Option<String> {
        self.send_message_with_attachment_fields_preference_report(request)
            .ok()
            .map(|queued| queued.message_id)
    }

    pub fn send_message_with_attachment_fields_preference_report(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Result<LxmfQueuedMessage, LxmfSubmissionFailure> {
        self.send_message_with_attachment_fields_preference_internal(request)
    }

    fn send_message_with_attachment_fields_preference_internal(
        &mut self,
        request: AttachmentMessageRequest<'_>,
    ) -> Result<LxmfQueuedMessage, LxmfSubmissionFailure> {
        let AttachmentMessageRequest {
            dest_hash_hex,
            content,
            title,
            file_name,
            file_bytes,
            staged_path,
            is_image,
            image_mime,
            db_pool,
            identity_id,
            preference,
            renderer,
            thread,
        } = request;

        let dest_bytes =
            hex::decode(dest_hash_hex).map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        if dest_bytes.len() != 16 {
            return Err(LxmfSubmissionFailure::PreparationFailed);
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let method = self.pick_delivery_method(
            db_pool,
            dest_hash_hex,
            preference,
            DeliveryProfile::Attachment,
        );
        let mut msg = LxMessageDraft::new(dest, self.lxmf_dest_hash, title, content, method)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        self.apply_peer_lxmf_compression_support(&mut msg, Some(db_pool), dest_hash_hex);

        if is_image {
            let image_format = image_mime
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("png");
            msg.set_image_field(image_format, file_bytes)
                .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        } else {
            msg.set_file_attachment_field(file_name, file_bytes)
                .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        }
        if let Some((field_id, bytes)) = renderer.map(renderer_custom_field) {
            msg.set_msgpack_field(field_id, bytes)
                .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        }
        if let Some((field_id, bytes)) = thread_custom_field(thread) {
            msg.set_field(field_id, bytes)
                .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        }

        msg.include_ticket = true;
        let intent = self
            .router
            .prepare_outbound(&mut msg)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        intent
            .persist(&self.lxmf_storage_dir, &mut msg)
            .map_err(|_| LxmfSubmissionFailure::StorageFailed)?;
        let identity = self.identity.clone();
        let mut msg = msg
            .sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        normalize_protocol_delivery_method(&mut msg);
        let packed_len =
            finalized_packed_len(&msg).map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        if let Err(error) = validate_attachment_envelope_size(packed_len) {
            tracing::warn!(
                packed_len,
                max_len = MAX_LXMF_RESOURCE_BYTES,
                "LXMF attachment message exceeds RNS resource limit"
            );
            return Err(error);
        }

        let msg_id = hex::encode(msg.message_id());
        let display_timestamp = db::next_conversation_observed_timestamp(
            db_pool,
            dest_hash_hex,
            identity_id,
            msg.timestamp(),
        );

        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, preference);
        let method = msg.method;
        // The file is durable before the message is queued, so its row can
        // never reference a missing file; every failure below removes it.
        let stored_name = match staged_path {
            Some(path) => self.adopt_staged_attachment(file_name, path),
            None => self.save_attachment(file_name, file_bytes),
        }
        .map_err(|_| LxmfSubmissionFailure::StorageFailed)?;
        let (attachment_name, attachment_stored_name, image_name, image_stored_name) = if is_image {
            ("", "", file_name, stored_name.as_str())
        } else {
            (file_name, stored_name.as_str(), "", "")
        };

        let message_id = msg.message_id();
        let resend = crate::outbox::OutboxMessage::referencing(
            &msg,
            Some(crate::outbox::StoredFile {
                field: if is_image {
                    lxmf_core::constants::FIELD_IMAGE
                } else {
                    lxmf_core::constants::FIELD_FILE_ATTACHMENTS
                },
                stored_name: &stored_name,
                bytes: file_bytes,
            }),
        )
        .encode();
        if self.router.submit_signed(msg).is_err() {
            let _ = std::fs::remove_file(self.files_dir().join(&stored_name));
            return Err(LxmfSubmissionFailure::PreparationFailed);
        }
        let source = self.lxmf_hash.clone();
        let persisted = self.persist_outbound(
            db_pool,
            message_id,
            &db::OutboundMessage {
                msg_id: &msg_id,
                identity_id,
                source: &source,
                destination: dest_hash_hex,
                content,
                title,
                timestamp: display_timestamp,
                delivery_method: delivery_method_name(method),
                attachment_name,
                attachment_stored_name,
                image_name,
                image_stored_name,
                renderer,
                thread,
                resend: db::Resend {
                    preference: preference.as_str(),
                    profile: DeliveryProfile::Attachment.as_str(),
                    message: &resend,
                },
                ..db::OutboundMessage::default()
            },
        );
        if !persisted {
            let _ = std::fs::remove_file(self.files_dir().join(&stored_name));
            return Err(LxmfSubmissionFailure::StorageFailed);
        }
        self.auto_live_fallback.extend(auto_fallback);
        Ok(LxmfQueuedMessage {
            message_id: msg_id,
            method,
        })
    }

    /// Queue a standards-based LXMF voice message as
    /// `FIELD_AUDIO = [AM_OPUS_OGG, ogg_bytes]`.
    pub fn send_audio_message(&mut self, request: AudioMessageRequest<'_>) -> Option<String> {
        self.send_audio_message_with_preference_report(request)
            .ok()
            .map(|queued| queued.message_id)
    }

    /// Fallible first-class audio submission surface for the app command
    /// layer. This deliberately does not route through generic attachments.
    pub fn send_audio_message_with_preference_report(
        &mut self,
        request: AudioMessageRequest<'_>,
    ) -> Result<LxmfQueuedMessage, LxmfSubmissionFailure> {
        let AudioMessageRequest {
            dest_hash_hex,
            content,
            title,
            audio_bytes,
            audio_mode,
            staged_path,
            db_pool,
            identity_id,
            preference,
            thread,
        } = request;

        validate_outbound_audio(audio_bytes, audio_mode)?;
        // The recorder freezes its selected mode. Discovery may change while
        // reviewing or retrying; it must not relabel or discard existing audio.
        let content = if content.trim().is_empty() {
            "Voice message"
        } else {
            content
        };
        let dest_bytes =
            hex::decode(dest_hash_hex).map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        if dest_bytes.len() != 16 {
            return Err(LxmfSubmissionFailure::PreparationFailed);
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);

        let method = self.pick_delivery_method(
            db_pool,
            dest_hash_hex,
            preference,
            DeliveryProfile::Attachment,
        );
        let mut msg = LxMessageDraft::new(dest, self.lxmf_dest_hash, title, content, method)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        self.apply_peer_lxmf_compression_support(&mut msg, Some(db_pool), dest_hash_hex);
        msg.set_audio_field(audio_mode, audio_bytes)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        if let Some((field_id, bytes)) = thread_custom_field(thread) {
            msg.set_field(field_id, bytes)
                .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        }
        debug_assert!(
            msg.get_field(lxmf_core::constants::FIELD_FILE_ATTACHMENTS)
                .is_none()
        );

        msg.include_ticket = true;
        let intent = self
            .router
            .prepare_outbound(&mut msg)
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        intent
            .persist(&self.lxmf_storage_dir, &mut msg)
            .map_err(|_| LxmfSubmissionFailure::StorageFailed)?;
        let identity = self.identity.clone();
        let mut msg = msg
            .sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
            .map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        normalize_protocol_delivery_method(&mut msg);
        let packed_admission_len =
            finalized_packed_len(&msg).map_err(|_| LxmfSubmissionFailure::PreparationFailed)?;
        validate_audio_message_size(packed_admission_len)?;
        if audio_mode == lxmf_core::constants::AM_CODEC2_700C && packed_admission_len > 3_659 {
            return Err(LxmfSubmissionFailure::ResourceLimitExceeded {
                actual_bytes: packed_admission_len,
                limit_bytes: 3_659,
            });
        }

        let msg_id = hex::encode(msg.message_id());
        let display_timestamp = db::next_conversation_observed_timestamp(
            db_pool,
            dest_hash_hex,
            identity_id,
            msg.timestamp(),
        );

        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, preference);
        let method = msg.method;
        let filename = if audio_mode == lxmf_core::constants::AM_CODEC2_700C {
            COMPACT_AUDIO_FILE_NAME
        } else {
            AUDIO_MESSAGE_FILE_NAME
        };
        let stored_name = match staged_path {
            Some(path) => self.adopt_staged_attachment(filename, path),
            None => self.save_attachment(filename, audio_bytes),
        }
        .map_err(|_| LxmfSubmissionFailure::StorageFailed)?;

        let message_id = msg.message_id();
        let resend = crate::outbox::OutboxMessage::referencing(
            &msg,
            Some(crate::outbox::StoredFile {
                field: lxmf_core::constants::FIELD_AUDIO,
                stored_name: &stored_name,
                bytes: audio_bytes,
            }),
        )
        .encode();
        if self.router.submit_signed(msg).is_err() {
            let _ = std::fs::remove_file(self.files_dir().join(&stored_name));
            return Err(LxmfSubmissionFailure::PreparationFailed);
        }
        let source = self.lxmf_hash.clone();
        let persisted = self.persist_outbound(
            db_pool,
            message_id,
            &db::OutboundMessage {
                msg_id: &msg_id,
                identity_id,
                source: &source,
                destination: dest_hash_hex,
                content,
                title,
                timestamp: display_timestamp,
                delivery_method: delivery_method_name(method),
                audio_mode: Some(audio_mode),
                audio_stored_name: &stored_name,
                thread,
                resend: db::Resend {
                    preference: preference.as_str(),
                    profile: DeliveryProfile::Attachment.as_str(),
                    message: &resend,
                },
                ..db::OutboundMessage::default()
            },
        );
        if !persisted {
            let _ = std::fs::remove_file(self.files_dir().join(&stored_name));
            return Err(LxmfSubmissionFailure::StorageFailed);
        }
        self.auto_live_fallback.extend(auto_fallback);
        Ok(LxmfQueuedMessage {
            message_id: msg_id,
            method,
        })
    }

    /// LRGP send. Auto starts `Direct` (real LXMF Link proof plus the core
    /// retry policy) and may use the same terminal Offline Inbox fallback as
    /// messages and attachments.
    pub fn send_message_with_lrgp_fields(
        &mut self,
        dest_hash_hex: &str,
        fallback_text: &str,
        lrgp_fields: &std::collections::HashMap<u8, rmpv::Value>,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Option<String> {
        self.send_message_with_lrgp_fields_preference(
            dest_hash_hex,
            fallback_text,
            lrgp_fields,
            db_pool,
            identity_id,
            DeliveryPreference::Auto,
        )
    }

    pub fn send_message_with_lrgp_fields_preference(
        &mut self,
        dest_hash_hex: &str,
        fallback_text: &str,
        lrgp_fields: &std::collections::HashMap<u8, rmpv::Value>,
        db_pool: &DbPool,
        identity_id: &str,
        preference: DeliveryPreference,
    ) -> Option<String> {
        let dest_bytes = match hex::decode(dest_hash_hex) {
            Ok(b) => b,
            Err(_) => {
                tracing::warn!(
                    target: "ttt_trace",
                    step = "lxmf_send.hex_fail",
                    input_len = dest_hash_hex.len(),
                    reason = "invalid_hex",
                    "dest_hash_hex not valid hex"
                );
                return None;
            }
        };
        if dest_bytes.len() != 16 {
            tracing::warn!(
                target: "ttt_trace",
                step = "lxmf_send.len_fail",
                len = dest_bytes.len(),
                reason = "invalid_length",
                "dest hash length != 16"
            );
            return None;
        }
        let mut dest = [0u8; 16];
        dest.copy_from_slice(&dest_bytes);
        let canonical_dest = hex::encode(dest);
        let dest_short = crate::short_id(&canonical_dest);
        tracing::info!(
            target: "ttt_trace",
            step = "lxmf_send.enter",
            dest = %dest_short,
            field_count = lrgp_fields.len(),
            "send_message_with_lrgp_fields entered"
        );

        let method =
            self.pick_delivery_method(db_pool, dest_hash_hex, preference, DeliveryProfile::Lrgp);
        let mut msg =
            LxMessageDraft::new(dest, self.lxmf_dest_hash, "", fallback_text, method).ok()?;
        self.apply_peer_lxmf_compression_support(&mut msg, Some(db_pool), dest_hash_hex);

        if let Err(error) = apply_lrgp_fields_to_message(&mut msg, lrgp_fields) {
            tracing::warn!(
                target: "ttt_trace",
                step = "lxmf_send.field_encode_fail",
                error,
                "LRGP fields could not be represented as native LXMF values"
            );
            return None;
        }

        msg.include_ticket = true;
        let intent = self.router.prepare_outbound(&mut msg).ok()?;
        intent.persist(&self.lxmf_storage_dir, &mut msg).ok()?;
        let identity = self.identity.clone();
        let mut msg = msg
            .sign_with(|bytes| identity.sign(bytes).ok_or("identity signing unavailable"))
            .ok()?;
        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return None;
        }

        let msg_id = hex::encode(msg.message_id());

        // LRGP payloads not persisted to messages; game session is the surface.
        let _ = (db_pool, identity_id, fallback_text);

        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, preference);
        self.router.submit_signed(msg).ok()?;
        self.auto_live_fallback.extend(auto_fallback);
        let msg_id_short: String = msg_id.chars().take(8).collect();
        tracing::info!(
            target: "ttt_trace",
            step = "lxmf_send.exit",
            dest = %dest_short,
            msg_id = %msg_id_short,
            "send_message_with_lrgp_fields exit"
        );
        Some(msg_id)
    }

    pub fn get_contacts_list(&self, db_pool: &DbPool, identity_id: &str) -> Vec<Value> {
        crate::contact_list_payload(db::get_all_contacts(db_pool, identity_id))
    }

    pub fn get_conversation(
        &self,
        dest_hash: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Vec<Value> {
        db::get_conversation(db_pool, dest_hash, identity_id, 100)
    }

    pub fn get_propagation_status(&self) -> Value {
        let node_hash = self.configured_propagation_node.map(hex::encode);
        let sync_state = if let Some(ref sync) = self.propagation_sync {
            let state = format!("{:?}", sync.state);
            state
        } else {
            "disabled".to_string()
        };
        let transfer_status = self
            .propagation_client
            .as_ref()
            .map(|client| client.transfer_status());
        let client_state = transfer_status
            .map(|status| format!("{:?}", status.state))
            .unwrap_or_else(|| "none".to_string());
        let connected = self
            .propagation_client
            .as_ref()
            .map(|c| {
                matches!(
                    c.state(),
                    lxmf_core::propagation_client::PropagationClientState::LinkEstablished
                        | lxmf_core::propagation_client::PropagationClientState::ListRequested
                        | lxmf_core::propagation_client::PropagationClientState::GetRequested
                        | lxmf_core::propagation_client::PropagationClientState::Receiving
                        | lxmf_core::propagation_client::PropagationClientState::PurgeRequested
                        | lxmf_core::propagation_client::PropagationClientState::AwaitingAdmission
                        | lxmf_core::propagation_client::PropagationClientState::Partial
                        | lxmf_core::propagation_client::PropagationClientState::Complete
                )
            })
            .unwrap_or(false);
        let admission = self
            .propagation_client
            .as_ref()
            .map(|client| client.admission_status())
            .unwrap_or_default();
        let counts = transfer_status
            .filter(|status| {
                status.state != lxmf_core::propagation_client::PropagationClientState::Idle
            })
            .map(|status| status.admission)
            .unwrap_or(self.last_propagation_admission);
        json!({
            "enabled": self.client_propagation_enabled,
            "node_hash": node_hash,
            "propagation_node": node_hash,
            "sync_state": sync_state,
            "client_state": client_state,
            "connected": connected,
            "transfer_progress": transfer_status.map(|status| status.progress),
            "transfer_size": transfer_status.and_then(|status| status.data_size),
            "transfer_result": transfer_status.and_then(|status| status.result),
            "downloaded_count": counts.downloaded,
            "committed_count": counts.committed,
            "duplicate_count": counts.duplicate,
            "policy_discarded_count": counts.policy_discarded,
            "user_deleted_count": counts.user_deleted,
            "rejected_count": counts.rejected,
            "retryable_count": counts.retryable,
            "message_count": self.router.propagation_store.len(),
            "suppressed_count": admission.suppressed_count,
            "deferred_count": admission.deferred_count,
            "next_retry": admission.next_retry,
            "suppression_records": admission.node_records,
            "suppression_capacity": admission.node_capacity,
            "suppression_full": admission.ledger_full,
            "admission_paused": admission.admission_paused || !self.download_state_installed(),
        })
    }

    pub fn files_dir(&self) -> PathBuf {
        let d = self
            .data_dir
            .join("identities")
            .join(&self.identity_hash)
            .join("files");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn list_received_files(&self) -> Vec<Value> {
        let dir = self.files_dir();
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let meta = std::fs::metadata(&path).ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);

                    // Strip the `<ts>_` storage prefix.
                    let display_name = name
                        .find('_')
                        .map(|pos| name[pos + 1..].to_string())
                        .unwrap_or_else(|| name.clone());

                    files.push(json!({
                        "name": display_name,
                        "stored_name": name,
                        "size": size,
                        "timestamp": modified,
                    }));
                }
            }
        }
        files.sort_by(|a, b| {
            let ta = a.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tb = b.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
            tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
        });
        files
    }

    pub fn get_received_file(&self, stored_name: &str) -> Option<PathBuf> {
        let sanitized = sanitize_stored_file_name(stored_name)?;
        let files_dir = self.files_dir().canonicalize().ok()?;
        let path = files_dir.join(&sanitized).canonicalize().ok()?;
        (path.starts_with(&files_dir) && path.is_file()).then_some(path)
    }

    pub fn save_attachment(&self, file_name: &str, data: &[u8]) -> std::io::Result<String> {
        save_attachment_in(&self.files_dir(), file_name, data)
    }

    fn adopt_staged_attachment(
        &self,
        file_name: &str,
        staged_path: &Path,
    ) -> std::io::Result<String> {
        let stored_name = attachment_storage_name(file_name);
        let files_dir = self.files_dir();
        let path = files_dir.join(&stored_name);
        std::fs::rename(staged_path, &path)?;
        // Windows requires a writable handle for FlushFileBuffers. Opening the
        // adopted file read-only makes the otherwise-successful atomic rename
        // look like a storage failure on that platform.
        if let Err(error) = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.sync_all())
        {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        #[cfg(unix)]
        if let Err(error) = std::fs::File::open(&files_dir).and_then(|dir| dir.sync_all()) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(stored_name)
    }

    pub fn add_contact(
        &self,
        dest_hash: &str,
        display_name: Option<&str>,
        trust: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) {
        db::save_contact(db_pool, dest_hash, display_name, trust, identity_id);
    }

    pub fn remove_contact(&self, dest_hash: &str, db_pool: &DbPool, identity_id: &str) {
        db::delete_contact(db_pool, dest_hash, identity_id);
    }

    pub fn hide_conversation(&self, dest_hash: &str, db_pool: &DbPool, identity_id: &str) {
        db::hide_conversation(db_pool, dest_hash, identity_id);
    }

    pub fn delete_conversation(
        &mut self,
        dest_hash: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> Result<(), db::admission::AdmissionDbError> {
        self.invalidate_download_state();
        let store = db::admission::DurableInboundStore::from_pool(db_pool)?;
        let guard = db::admission::try_reserve_admission()?;
        let artifacts = store.delete_messages_guarded(
            &guard,
            identity_id,
            Some(dest_hash),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )?;
        crate::inbound_admission::cleanup_deleted_artifacts(
            &self.data_dir,
            &store,
            &guard,
            &artifacts,
        )
    }

    pub fn set_propagation_node(
        &mut self,
        node_hash: Option<&str>,
        db_pool: &DbPool,
        identity_id: &str,
    ) -> bool {
        let Some(decoded_node) = Self::decode_propagation_node_hash(node_hash) else {
            return false;
        };
        let node_str = decoded_node.map(hex::encode).unwrap_or_default();

        if db::set_identity_propagation_node(db_pool, identity_id, &node_str).is_err() {
            tracing::warn!(
                reason = "persist_failed",
                "failed to persist propagation node setting"
            );
        }

        self.set_runtime_propagation_node(decoded_node);
        true
    }

    fn decode_propagation_node_hash(node_hash: Option<&str>) -> Option<Option<[u8; 16]>> {
        let node_str = node_hash.unwrap_or("");
        if node_str.is_empty() {
            return Some(None);
        }
        match hex::decode(node_str) {
            Ok(bytes) if bytes.len() == 16 => {
                let mut dest = [0u8; 16];
                dest.copy_from_slice(&bytes);
                Some(Some(dest))
            }
            _ => {
                tracing::warn!(
                    reason = "invalid_hash",
                    "invalid propagation node hash ignored"
                );
                None
            }
        }
    }

    pub fn set_runtime_propagation_node(&mut self, node: Option<[u8; 16]>) {
        if let Some(dest) = node {
            let previous = self.configured_propagation_node;
            self.configured_propagation_node = Some(dest);
            if let Some(prev) = previous.filter(|p| p != &dest) {
                self.router.unpeer(&prev);
            }
            if self.client_propagation_enabled {
                self.activate_client_propagation_node(dest);
            }
        } else {
            if let Some(prev) = self.configured_propagation_node.take() {
                self.router.unpeer(&prev);
            }
            self.router.set_outbound_propagation_node(None);
            self.stop_propagation_sync();
            self.propagation_client = None;
        }
    }

    pub fn enable_propagation(&mut self, enabled: bool, db_pool: &DbPool, identity_id: &str) {
        self.client_propagation_enabled = enabled;
        if !enabled {
            self.router.set_outbound_propagation_node(None);
            self.stop_propagation_sync();
            self.propagation_client = None;
        }
        if let Ok(conn) = db_pool.get() {
            conn.execute(
                "UPDATE identities SET propagation_enabled = ?1 WHERE hash = ?2",
                rusqlite::params![if enabled { 1 } else { 0 }, identity_id],
            )
            .ok();
        }
    }

    fn activate_client_propagation_node(&mut self, dest: [u8; 16]) {
        self.router.set_outbound_propagation_node(Some(dest));
        self.start_propagation_sync(dest);

        // Register as router peer (static_peers blocks LRU rotation).
        self.router
            .peers
            .entry(dest)
            .or_insert_with(|| lxmf_core::peer::LxmPeer::new(dest));
        if !self.router.static_peers.contains(&dest) {
            self.router.static_peers.push(dest);
        }

        if let Some(ref tx) = self.router.transport_tx {
            let mut client = lxmf_core::propagation_client::PropagationClient::new(
                tx.clone(),
                Some(self.identity.get_public_key()),
                self.identity.get_signing_key(),
            );
            client.set_propagation_node(dest);
            client.set_delivery_limit(self.delivery_limit_kb);
            if let Some(state) = self.download_state.as_ref().filter(|s| s.node == dest) {
                client.set_suppression_ledger(state.ledger.clone(), state.epochs);
                client.replace_durable_haves(state.haves.iter().copied());
            } else {
                client.suppression_persistence_failed();
            }
            self.propagation_client = Some(client);
            tracing::info!(
                node = %crate::short_id(&hex::encode(dest)),
                "propagation client created for message download"
            );
        }
    }

    /// Install only a durably persisted, identity-scoped snapshot. The selected
    /// node is captured here so future relay switches cannot inherit its haves.
    pub fn install_download_state(
        &mut self,
        ledger: SuppressionLedger,
        epochs: AdmissionEpochs,
        haves: Vec<lxmf_core::admission::DurableHave>,
    ) {
        let Some(node) = self.configured_propagation_node else {
            return;
        };
        if let Some(client) = self.propagation_client.as_mut() {
            client.set_suppression_ledger(ledger.clone(), epochs);
            client.replace_durable_haves(haves.iter().copied());
        }
        self.download_state = Some(InstalledDownloadState {
            node,
            ledger,
            epochs,
            haves,
        });
        self.download_suppression_revision = self.download_suppression_revision.wrapping_add(1);
    }
    /// Refresh proven haves after a durable message commit without replacing
    /// newer, still-unpersisted suppression decisions from this client.
    pub fn install_download_haves(
        &mut self,
        identity: [u8; 16],
        node: [u8; 16],
        haves: Vec<lxmf_core::admission::DurableHave>,
    ) -> bool {
        if identity != self.identity.hash || Some(node) != self.configured_propagation_node {
            return false;
        }
        let Some(state) = self.download_state.as_mut().filter(|s| s.node == node) else {
            return false;
        };
        if let Some(client) = self.propagation_client.as_mut() {
            client.replace_durable_haves(haves.iter().copied());
        }
        state.haves = haves;
        true
    }
    /// Retain a newly durable disposition for a future client generation. The
    /// adapter calls this only after its commit and current-generation check;
    /// the active client still consumes the exact single-use admission token.
    pub fn remember_committed_download(
        &mut self,
        receipt: &lxmf_core::admission::DurableAdmissionReceipt,
    ) -> bool {
        let scope = receipt.record().scope;
        if scope.identity != self.identity.hash
            || Some(scope.node) != self.configured_propagation_node
        {
            return false;
        }
        let Some(state) = self
            .download_state
            .as_mut()
            .filter(|state| state.node == scope.node)
        else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        state
            .haves
            .retain(|have| have.active(now) && have.transient_id != scope.transient_id);
        let have = lxmf_core::admission::DurableHave::from_record(receipt.record());
        if have.active(now) {
            state.haves.push(have);
        }
        true
    }
    pub fn update_download_epochs(&mut self, identity: [u8; 16], epochs: AdmissionEpochs) -> bool {
        if identity != self.identity.hash {
            return false;
        }
        let Some(state) = self.download_state.as_mut() else {
            return false;
        };
        if state.epochs == epochs {
            return true;
        }
        state.epochs = epochs;
        state.ledger.prune(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            epochs,
        );
        if let Some(client) = self.propagation_client.as_mut() {
            client.update_admission_epochs(epochs);
        }
        self.download_suppression_revision = self.download_suppression_revision.wrapping_add(1);
        true
    }
    pub fn download_suppression_snapshot(&mut self) -> Option<DownloadSuppressionSnapshot> {
        let client = self.propagation_client.as_mut()?;
        if !client.take_suppression_updates().is_empty() {
            self.download_suppression_revision = self.download_suppression_revision.wrapping_add(1);
        }
        if !client.suppression_persistence_pending() {
            return None;
        }
        let state = self.download_state.as_ref()?;
        Some(DownloadSuppressionSnapshot {
            identity: self.identity.hash,
            node: state.node,
            ledger: client.suppression_ledger().clone(),
            epochs: state.epochs,
            revision: self.download_suppression_revision,
            client_revision: client.suppression_revision(),
        })
    }
    pub fn acknowledge_download_suppression(
        &mut self,
        snapshot: &DownloadSuppressionSnapshot,
    ) -> bool {
        if snapshot.identity != self.identity.hash
            || Some(snapshot.node) != self.configured_propagation_node
            || snapshot.revision != self.download_suppression_revision
        {
            return false;
        }
        if let Some(client) = self.propagation_client.as_mut() {
            if !client.take_suppression_updates().is_empty() {
                self.download_suppression_revision =
                    self.download_suppression_revision.wrapping_add(1);
                return false;
            }
            if !client.acknowledge_suppression_persistence(snapshot.client_revision) {
                return false;
            }
        }
        if let Some(state) = self.download_state.as_mut() {
            state.ledger = snapshot.ledger.clone();
            state.epochs = snapshot.epochs;
        }
        true
    }
    /// Fence delayed hydration against same-identity deletion/recovery changes.
    pub fn download_state_revision(&self) -> u64 {
        self.download_suppression_revision
    }
    pub fn download_state_installed(&self) -> bool {
        self.download_state
            .as_ref()
            .is_some_and(|state| Some(state.node) == self.configured_propagation_node)
    }
    /// Deletion/recovery revokes the current transfer before changing effects.
    /// Only a fresh durable adapter hydration can resume selection afterward.
    pub fn invalidate_download_state(&mut self) {
        if let Some(client) = self.propagation_client.as_mut() {
            client.cancel_download();
            client.replace_durable_haves(std::iter::empty());
            client.suppression_persistence_failed();
        }
        self.download_state = None;
        self.download_suppression_revision = self.download_suppression_revision.wrapping_add(1);
    }
    pub fn pause_download_admission(&mut self) {
        if let Some(client) = self.propagation_client.as_mut() {
            client.suppression_persistence_failed();
        }
    }
    pub fn download_key_epoch(&self) -> [u8; 32] {
        let mut keys = self
            .known_identities
            .iter()
            .map(|(dest, key)| (dest.clone(), *key))
            .collect::<Vec<_>>();
        keys.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut bytes = b"LXMF download keys v1".to_vec();
        for (dest, key) in keys {
            bytes.extend_from_slice(dest.as_bytes());
            bytes.extend_from_slice(&key);
        }
        let mut ratchets = self
            .delivery_ratchets
            .ring()
            .private_keys()
            .iter()
            .map(rns_identity::ratchet::ratchet_public_bytes)
            .collect::<Vec<_>>();
        ratchets.sort_unstable();
        for key in ratchets {
            bytes.extend_from_slice(&key);
        }
        rns_crypto::sha::full_hash(&bytes)
    }

    pub fn set_delivery_limit_kb(&mut self, limit_kb: usize) {
        self.delivery_limit_kb = limit_kb as f64;
        if let Some(ref mut link_delivery) = self.link_delivery {
            link_delivery.set_inbound_resource_limit_bytes(
                limit_kb.saturating_mul(1_000).min(MAX_LXMF_RESOURCE_BYTES),
            );
        }
        if let Some(client) = self.propagation_client.as_mut() {
            client.set_delivery_limit(self.delivery_limit_kb);
        }
    }

    pub fn send_reaction(
        &mut self,
        dest_hash_hex: &str,
        message_id: &str,
        emoji: &str,
        action: &str,
        db_pool: &DbPool,
        identity_id: &str,
    ) {
        self.send_reaction_with_preference(ReactionSendRequest {
            dest_hash_hex,
            message_id,
            emoji,
            action,
            db_pool,
            identity_id,
            preference: DeliveryPreference::Auto,
        });
    }

    pub fn send_reaction_with_preference(&mut self, request: ReactionSendRequest<'_>) -> bool {
        if !matches!(hex::decode(request.dest_hash_hex), Ok(bytes) if bytes.len() == 16) {
            return false;
        }
        let action = if request.action == "remove" {
            "remove"
        } else {
            "add"
        };

        let mut custom_fields =
            match ratspeak_chat_custom_fields(&RatspeakChatExtension::Reaction {
                target: request.message_id.to_string(),
                emoji: request.emoji.to_string(),
                action: action.to_string(),
            }) {
                Some(fields) => fields,
                None => return false,
            };
        // A reaction travels in its target's conversation.
        let thread = db::message_thread(request.db_pool, request.message_id, request.identity_id);
        custom_fields.extend(thread_custom_field(&thread));

        let mut msg = match self.create_message_with_custom_fields(
            request.dest_hash_hex,
            "",
            "",
            self.pick_delivery_method(
                request.db_pool,
                request.dest_hash_hex,
                request.preference,
                DeliveryProfile::Message,
            ),
            &custom_fields,
        ) {
            Some(msg) => msg,
            None => return false,
        };
        self.apply_peer_lxmf_compression_support(
            &mut msg,
            Some(request.db_pool),
            request.dest_hash_hex,
        );

        normalize_protocol_delivery_method(&mut msg);
        if !message_within_resource_limit(&msg) {
            return false;
        }

        self.preempt_opportunistic_path(&mut msg);
        let auto_fallback = Self::auto_live_fallback_hash(&msg, request.preference);
        if self.router.submit_signed(msg).is_err() {
            return false;
        }
        self.auto_live_fallback.extend(auto_fallback);

        if action == "remove" {
            db::remove_reaction(
                request.db_pool,
                request.message_id,
                &self.lxmf_hash,
                request.emoji,
                request.identity_id,
            );
        } else {
            db::save_reaction(
                request.db_pool,
                request.message_id,
                &self.lxmf_hash,
                request.emoji,
                request.identity_id,
            );
        }
        true
    }

    #[cfg(test)]
    pub(crate) fn create_announce_packet(&mut self) -> Result<Vec<u8>, String> {
        self.build_delivery_announce_packet(DeliveryAnnounceKind::Broadcast)
    }

    pub(crate) fn create_coordinated_announce_packet(
        &mut self,
    ) -> Result<Vec<u8>, CoordinatedDeliveryAnnounceError> {
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cache_now = self.announce_cache_started.elapsed().as_secs_f64();
        self.build_delivery_announce_packet_at_typed(
            DeliveryAnnounceKind::Broadcast,
            wall_now,
            cache_now,
        )
    }

    /// Path-response variant: an otherwise-identical delivery announce tagged
    /// with the `PathResponse` context. Answers a path request by delivering
    /// our identity + path (and display name / stamp cost) to the requester
    /// without triggering wide rebroadcast. Without this, a peer that has never
    /// announced can't learn our keys/path, so replies to them stall until we
    /// announce. See the `AnnounceRequested` handler in `lib.rs`.
    pub fn create_path_response_announce_packet(
        &mut self,
        tag: Option<&[u8]>,
    ) -> Result<Vec<u8>, String> {
        self.build_delivery_announce_packet(DeliveryAnnounceKind::PathResponse { tag })
    }

    pub(crate) fn create_deferred_path_response_announce_packet(
        &mut self,
        tag: Option<&[u8]>,
    ) -> Result<Vec<u8>, CoordinatedDeliveryAnnounceError> {
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cache_now = self.announce_cache_started.elapsed().as_secs_f64();
        self.build_delivery_announce_packet_at_typed(
            DeliveryAnnounceKind::PathResponse { tag },
            wall_now,
            cache_now,
        )
    }

    fn build_delivery_announce_packet(
        &mut self,
        kind: DeliveryAnnounceKind<'_>,
    ) -> Result<Vec<u8>, String> {
        let wall_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let cache_now = self.announce_cache_started.elapsed().as_secs_f64();
        self.build_delivery_announce_packet_at(kind, wall_now, cache_now)
    }

    fn build_delivery_announce_packet_at(
        &mut self,
        kind: DeliveryAnnounceKind<'_>,
        wall_now: u64,
        cache_now: f64,
    ) -> Result<Vec<u8>, String> {
        self.build_delivery_announce_packet_at_typed(kind, wall_now, cache_now)
            .map_err(|error| match error {
                CoordinatedDeliveryAnnounceError::Coalesced => {
                    DeliveryRatchetError::Coalesced.to_string()
                }
                CoordinatedDeliveryAnnounceError::Failed(message) => message,
            })
    }

    fn build_delivery_announce_packet_at_typed(
        &mut self,
        kind: DeliveryAnnounceKind<'_>,
        wall_now: u64,
        cache_now: f64,
    ) -> Result<Vec<u8>, CoordinatedDeliveryAnnounceError> {
        // Pack as LXMF-compatible msgpack with a Ratspeak extension tail.
        // The first three fields match Python `LXMRouter.get_announce_app_data`.
        // Raw UTF-8 forces Python receivers onto a legacy path that skips
        // stamp-cost detection.
        let display_name_opt = if self.display_name.is_empty() {
            None
        } else {
            Some(self.display_name.as_str())
        };
        let status_opt = if self.announce_ratspeak_usage {
            Some(self.status.as_str())
        } else {
            None
        };
        let stamp_cost = self.router.config.stamp_cost;
        let app_data_bytes = get_ratspeak_announce_app_data(
            display_name_opt,
            stamp_cost,
            status_opt,
            self.announce_ratspeak_usage,
        );

        let raw = self
            .delivery_ratchets
            .create_announce(&self.identity, &app_data_bytes, wall_now, cache_now, kind)
            .map_err(|error| match error {
                DeliveryRatchetError::Coalesced => CoordinatedDeliveryAnnounceError::Coalesced,
                error => CoordinatedDeliveryAnnounceError::Failed(error.to_string()),
            })?;
        if raw.len() > rns_wire::constants::MTU {
            return Err(CoordinatedDeliveryAnnounceError::Failed(format!(
                "Announce packet exceeds Reticulum MTU ({} > {})",
                raw.len(),
                rns_wire::constants::MTU
            )));
        }
        Ok(raw)
    }

    pub fn create_propagation_announce_packet(&mut self) -> Result<Vec<u8>, String> {
        use rns_identity::announce::AnnounceData;

        let app_data = self.router.get_propagation_node_app_data();
        let announce =
            AnnounceData::create(&self.identity, "lxmf.propagation", Some(&app_data), None)
                .map_err(|e| format!("Failed to create propagation announce: {e}"))?;

        let mut raw = rns_wire::header::PacketHeader {
            flags: rns_wire::flags::PacketFlags {
                header_type: rns_wire::flags::HeaderType::Header1,
                context_flag: false,
                transport_type: rns_wire::flags::TransportType::Broadcast,
                destination_type: rns_wire::flags::DestinationType::Single,
                packet_type: rns_wire::flags::PacketType::Announce,
            },
            hops: 0,
            transport_id: None,
            destination_hash: self.propagation_dest_hash,
            context: rns_wire::context::PacketContext::None,
        }
        .pack();
        raw.extend_from_slice(&announce.pack());
        if raw.len() > rns_wire::constants::MTU {
            return Err(format!(
                "Propagation announce packet exceeds Reticulum MTU ({} > {})",
                raw.len(),
                rns_wire::constants::MTU
            ));
        }
        Ok(raw)
    }

    /// Memory-only; callers persist via `db::update_identity` outside the
    /// manager lock so the mutex is never held across a DB write.
    pub fn set_display_name(&mut self, name: &str) {
        self.display_name = name.to_string();
    }

    /// Memory-only; callers persist via `db::update_identity_status` outside
    /// the manager lock.
    pub fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
    }

    pub fn request_all_paths(&self, db_pool: &DbPool, identity_id: &str) -> usize {
        let contacts = db::get_all_contacts(db_pool, identity_id);
        let mut count = 0;
        for contact in &contacts {
            if let Some(bytes) = contact
                .get("dest_hash")
                .and_then(|value| value.as_str())
                .and_then(|hash| hex::decode(hash).ok())
                .filter(|bytes| bytes.len() == 16)
            {
                let mut dest = [0u8; 16];
                dest.copy_from_slice(&bytes);
                if let Some(tx) = &self.router.transport_tx {
                    if tx
                        .try_send(TransportMessage::RequestPath {
                            destination_hash: dest,
                        })
                        .is_err()
                    {
                        tracing::warn!(dest = %crate::short_id(&hex::encode(dest)), reason = "backpressure", "path request drop; next sweep will retry");
                    }
                    count += 1;
                }
            }
        }
        count
    }

    /// `known_path_hashes` from poll loop's path-table snapshot.
    pub fn check_contacts_identity_status(
        &self,
        db_pool: &DbPool,
        identity_id: &str,
        known_path_hashes: &std::collections::HashSet<String>,
    ) -> Value {
        let contacts = db::get_all_contacts(db_pool, identity_id);
        let mut status = serde_json::Map::new();
        for contact in &contacts {
            if let Some(hash) = contact.get("dest_hash").and_then(|v| v.as_str()) {
                let s = if known_path_hashes.contains(hash) {
                    "known"
                } else {
                    "unknown"
                };
                status.insert(hash.to_string(), json!(s));
            }
        }
        Value::Object(status)
    }

    pub fn ratchets_dir(&self) -> std::path::PathBuf {
        self.data_dir
            .join("identities")
            .join(&self.identity_hash)
            .join("ratchets")
    }

    /// Binary: repeated `[dest_hash:16][pubkey:64]` records.
    pub fn known_identities_blob(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(self.known_identities.len() * 80);
        for (hash_hex, pk) in &self.known_identities {
            if let Some(hash_bytes) = hex::decode(hash_hex)
                .ok()
                .filter(|hash_bytes| hash_bytes.len() == 16)
            {
                data.extend_from_slice(&hash_bytes);
                data.extend_from_slice(pk);
            }
        }
        data
    }

    pub fn known_identities_snapshot(&self) -> KnownIdentitiesSnapshot {
        KnownIdentitiesSnapshot {
            identity_hash: self.identity_hash.clone(),
            revision: self.known_identities_revision,
            path: self.ratchets_dir().join("known_identities"),
            bytes: self.known_identities_blob(),
            count: self.known_identities.len(),
        }
    }

    pub fn checkpoint_snapshot(&self) -> LxmfCheckpointSnapshot {
        LxmfCheckpointSnapshot {
            known_identities: self.known_identities_snapshot(),
            router_state: self.router.state_snapshot(),
            router_state_dir: self.lxmf_storage_dir.clone(),
            node_policies: self.propagation_node_policies.clone(),
        }
    }

    pub fn persistence_delta_snapshot(
        &self,
        identities_changed: bool,
        changed_ratchet_hashes: &[String],
        router_changed: bool,
    ) -> LxmfPersistenceDelta {
        let received_dir = self.ratchets_dir().join("received");
        let ratchet_hashes = self
            .dirty_received_ratchets
            .iter()
            .chain(changed_ratchet_hashes.iter())
            .collect::<HashSet<_>>();
        let received_ratchets = ratchet_hashes
            .into_iter()
            .filter_map(|hash| {
                self.received_ratchets.get(hash).copied().map(|ratchet| {
                    (
                        hash.clone(),
                        received_dir.join(format!("{hash}.ratchet")),
                        ratchet,
                    )
                })
            })
            .collect();
        LxmfPersistenceDelta {
            identity_hash: self.identity_hash.clone(),
            known_identities: (identities_changed || self.known_identities_dirty())
                .then(|| self.known_identities_snapshot()),
            received_ratchets,
            router_state: router_changed
                .then(|| (self.lxmf_storage_dir.clone(), self.router.state_snapshot())),
            node_policies: router_changed.then(|| {
                (
                    self.lxmf_storage_dir.clone(),
                    self.propagation_node_policies.clone(),
                )
            }),
        }
    }

    pub fn dirty_received_ratchets_snapshot(&self) -> LxmfPersistenceDelta {
        let received_dir = self.ratchets_dir().join("received");
        let received_ratchets = self
            .dirty_received_ratchets
            .iter()
            .filter_map(|hash| {
                self.received_ratchets.get(hash).copied().map(|ratchet| {
                    (
                        hash.clone(),
                        received_dir.join(format!("{hash}.ratchet")),
                        ratchet,
                    )
                })
            })
            .collect();
        LxmfPersistenceDelta {
            identity_hash: self.identity_hash.clone(),
            known_identities: None,
            received_ratchets,
            router_state: None,
            node_policies: None,
        }
    }

    /// Clear only ratchet versions that the persistence owner actually wrote.
    /// If the live value changed while disk I/O was in flight, the dirty bit is
    /// retained and the newer value is handled by the next serialized pass.
    pub fn acknowledge_persistence_delta(&mut self, delta: &LxmfPersistenceDelta) {
        if self.identity_hash != delta.identity_hash {
            return;
        }
        if let Some(snapshot) = delta.known_identities.as_ref() {
            self.acknowledge_known_identities_snapshot(snapshot);
        }
        for (hash, persisted) in delta.persisted_ratchets() {
            if self.received_ratchets.get(hash).is_some_and(|current| {
                current.ratchet_pub == persisted.ratchet_pub
                    && current.received_at == persisted.received_at
            }) {
                self.dirty_received_ratchets.remove(hash);
            }
        }
    }

    pub fn acknowledge_known_identities_snapshot(&mut self, snapshot: &KnownIdentitiesSnapshot) {
        if self.identity_hash == snapshot.identity_hash
            && self.known_identities_revision == snapshot.revision
        {
            self.known_identities_persisted_revision = snapshot.revision;
        }
    }

    pub fn acknowledge_checkpoint_snapshot(&mut self, snapshot: &LxmfCheckpointSnapshot) {
        self.acknowledge_known_identities_snapshot(&snapshot.known_identities);
    }

    pub fn known_identities_dirty(&self) -> bool {
        self.known_identities_revision != self.known_identities_persisted_revision
    }

    pub fn remove_known_identities(
        &mut self,
        hashes: &std::collections::HashSet<String>,
    ) -> Vec<(String, [u8; 64])> {
        let removed = hashes
            .iter()
            .filter_map(|hash| {
                self.known_identities
                    .remove(hash)
                    .map(|public_key| (hash.clone(), public_key))
            })
            .collect::<Vec<_>>();
        if !removed.is_empty() {
            self.known_identities_revision = self.known_identities_revision.wrapping_add(1);
        }
        removed
    }

    pub fn restore_known_identities(&mut self, entries: &[(String, [u8; 64])]) -> usize {
        let mut changed = false;
        for (hash, public_key) in entries {
            if self.known_identities.get(hash) != Some(public_key) {
                self.known_identities.insert(hash.clone(), *public_key);
                changed = true;
            }
        }
        if changed {
            self.known_identities_revision = self.known_identities_revision.wrapping_add(1);
        }
        self.known_identities.len()
    }

    pub fn save_router_state(&self) {
        if self.router.save_state(&self.lxmf_storage_dir).is_err() {
            tracing::warn!(reason = "save_failed", "Failed to save LXMF router state");
        }
    }

    pub fn save_crypto_state(&self) -> std::io::Result<()> {
        established_identity_namespace(&self.data_dir, &self.identity_hash)?;
        self.delivery_ratchets.save_durable(&self.identity)?;
        let received_dir = self.ratchets_dir().join("received");
        for (hash_hex, ratchet) in &self.received_ratchets {
            if !crate::helpers::is_protocol_hash_16(hash_hex) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid received ratchet destination",
                ));
            }
            ratchet.save_durable(&received_dir.join(format!("{hash_hex}.ratchet")))?;
        }
        self.known_identities_snapshot().persist()?;
        self.router.save_state(&self.lxmf_storage_dir)?;
        lxmf_core::persist::save_node_policies(
            &self.lxmf_storage_dir,
            &self.propagation_node_policies,
        )
    }

    /// Returns `(identity_changed, ratchet_changed)`. No-op updates are
    /// skipped so callers persist only real deltas, and an unchanged
    /// ratchet keeps its original `received_at` (Python `_remember_ratchet`).
    pub fn update_remote_crypto(
        &mut self,
        dest_hash_hex: &str,
        pk: &[u8; 64],
        ratchet: Option<&[u8; 32]>,
    ) -> (bool, bool) {
        let identity_changed = self.known_identities.get(dest_hash_hex) != Some(pk);
        if identity_changed {
            self.known_identities.insert(dest_hash_hex.to_string(), *pk);
            self.known_identities_revision = self.known_identities_revision.wrapping_add(1);
        }
        let mut ratchet_changed = false;
        if let Some(r) = ratchet.filter(|ratchet| {
            self.received_ratchets
                .get(dest_hash_hex)
                .is_none_or(|received| received.ratchet_pub != **ratchet)
        }) {
            self.received_ratchets
                .insert(dest_hash_hex.to_string(), ReceivedRatchet::new(*r));
            self.dirty_received_ratchets
                .insert(dest_hash_hex.to_string());
            ratchet_changed = true;
        }
        (identity_changed, ratchet_changed)
    }

    pub fn replace_route_hops_from_path_table(
        &mut self,
        entries: &[rns_transport::messages::PathTableRpcEntry],
    ) {
        self.replace_routes_observed_at(entries, Instant::now());
    }

    pub(crate) fn replace_routes_observed_at(
        &mut self,
        entries: &[PathTableRpcEntry],
        started: Instant,
    ) {
        if started < self.route_snapshot_started {
            return;
        }
        self.route_snapshot_started = started;
        self.route_hops.clear();
        self.route_entries.clear();
        for entry in entries {
            self.route_hops.insert(entry.hash, entry.hops.max(1));
            self.route_entries.insert(entry.hash, entry.clone());
        }
    }

    pub fn update_route_hop(&mut self, dest_hash: [u8; 16], hops: u8) {
        self.route_hops.insert(dest_hash, hops.max(1));
    }

    fn delivery_link_hops(&self, dest_hash: [u8; 16]) -> u8 {
        self.route_hops.get(&dest_hash).copied().unwrap_or(1).max(1)
    }

    fn direct_route_entry(&self, dest_hash: [u8; 16], now: f64) -> Option<&PathTableRpcEntry> {
        self.route_entries
            .get(&dest_hash)
            .filter(|entry| entry.expires > now)
    }

    pub fn has_live_direct_route(&self, dest_hash: [u8; 16], now: f64) -> bool {
        self.direct_route_entry(dest_hash, now).is_some()
    }

    fn direct_reusable_link_state(&self, dest_hash: [u8; 16]) -> DirectReusableLinkState {
        let Some(snapshot) = self
            .link_delivery
            .as_ref()
            .and_then(|ld| ld.direct_link_snapshot(dest_hash))
        else {
            return DirectReusableLinkState::None;
        };

        if snapshot.idle_expired
            || snapshot.link_state == rns_link::link::LinkState::Closed
            || snapshot.delivery_state == DeliveryState::Failed
        {
            return DirectReusableLinkState::Closed { activated: false };
        }

        if snapshot.link_state == rns_link::link::LinkState::Active
            && snapshot.delivery_state == DeliveryState::Idle
        {
            DirectReusableLinkState::Active
        } else {
            DirectReusableLinkState::Pending
        }
    }

    fn expire_pending_direct_link_identifications(&mut self) {
        self.pending_direct_link_identifications
            .retain(|_, pending| {
                pending.observed_at.elapsed() <= DIRECT_BACKCHANNEL_IDENTIFY_GRACE
            });
    }

    fn pending_direct_link_identification_state(
        &self,
        dest_hash: [u8; 16],
    ) -> DirectReusableLinkState {
        self.pending_direct_link_identifications
            .get(&dest_hash)
            .filter(|pending| pending.observed_at.elapsed() <= DIRECT_BACKCHANNEL_IDENTIFY_GRACE)
            .map(|_| DirectReusableLinkState::Pending)
            .unwrap_or(DirectReusableLinkState::None)
    }

    fn direct_or_identifying_link_state(&self, dest_hash: [u8; 16]) -> DirectReusableLinkState {
        let direct_state = self.direct_reusable_link_state(dest_hash);
        if direct_state != DirectReusableLinkState::None {
            return direct_state;
        }
        self.pending_direct_link_identification_state(dest_hash)
    }

    fn direct_reusable_link_state_for_router(
        &self,
        dest_hash: [u8; 16],
    ) -> DirectReusableLinkState {
        let direct_state = self.direct_reusable_link_state(dest_hash);
        if direct_state != DirectReusableLinkState::None {
            return direct_state;
        }

        let pending_identification = self.pending_direct_link_identification_state(dest_hash);
        if pending_identification != DirectReusableLinkState::None {
            return pending_identification;
        }

        let Some(snapshot) = self
            .link_delivery
            .as_ref()
            .and_then(|ld| ld.backchannel_link_snapshot(dest_hash))
        else {
            return DirectReusableLinkState::None;
        };

        if snapshot.queued_deliveries > 0 || snapshot.in_flight_deliveries > 0 {
            return DirectReusableLinkState::Pending;
        }

        if self.lxmf_link_command_tx.is_some() {
            DirectReusableLinkState::Active
        } else {
            DirectReusableLinkState::None
        }
    }

    fn ensure_link_delivery_backchannel_sender(&mut self) {
        if self.lxmf_link_command_tx.is_none()
            || self.lxmf_backchannel_command_rx.is_some()
            || self.link_delivery.is_none()
        {
            return;
        }

        let (tx, rx) = mpsc::channel(BACKCHANNEL_COMMAND_BUFFER);
        if let Some(ref mut ld) = self.link_delivery {
            ld.set_cancellation_aware_backchannel_sender(tx);
            self.lxmf_backchannel_command_rx = Some(rx);
        }
    }

    fn ensure_link_delivery_inbound_sender(&mut self) {
        let Some(tx) = self.lxmf_direct_link_packet_tx.clone() else {
            return;
        };
        if let Some(ref mut ld) = self.link_delivery {
            ld.set_inbound_packet_sender(tx);
        }
    }

    fn ensure_link_delivery_resource_handlers(&mut self) {
        let Some(link_delivery) = self.link_delivery.as_mut() else {
            return;
        };
        if let Some(handler) = self.direct_inbound_resource_accept_handler.clone() {
            link_delivery.set_inbound_resource_accept_handler(move |link_id, advertisement| {
                handler(link_id, advertisement)
            });
        }
        if let Some(handler) = self.direct_inbound_resource_concluded_handler.clone() {
            link_delivery.set_inbound_resource_concluded_handler(move |link_id, resource_id| {
                handler(link_id, resource_id);
            });
        }
        if let Some(handler) = self.direct_inbound_resource_completion_handler.clone() {
            link_delivery.set_inbound_resource_completion_handler(
                move |link_id, resource_id, data| {
                    handler(link_id, resource_id, data);
                },
            );
        }
    }

    fn ensure_link_delivery_manager(&mut self) -> bool {
        if self.link_delivery.is_some() {
            if let Some(ref mut link_delivery) = self.link_delivery {
                link_delivery.set_inbound_resource_limit_bytes(
                    ((self.delivery_limit_kb * 1_000.0) as usize).min(MAX_LXMF_RESOURCE_BYTES),
                );
            }
            self.ensure_link_delivery_backchannel_sender();
            self.ensure_link_delivery_inbound_sender();
            self.ensure_link_delivery_resource_handlers();
            return true;
        }

        let Some(ref tx) = self.router.transport_tx else {
            return false;
        };

        self.link_delivery = Some(lxmf_core::link_delivery::LinkDeliveryManager::new(
            tx.clone(),
            Some(self.identity.get_public_key()),
            self.identity.get_signing_key(),
        ));
        if let Some(ref mut link_delivery) = self.link_delivery {
            if let Some(handle) = &self.endpoint_dispatch {
                link_delivery.set_link_endpoint_dispatch_handle(handle.clone());
            }
            link_delivery.set_inbound_resource_limit_bytes(
                ((self.delivery_limit_kb * 1_000.0) as usize).min(MAX_LXMF_RESOURCE_BYTES),
            );
        }
        self.ensure_link_delivery_backchannel_sender();
        self.ensure_link_delivery_inbound_sender();
        self.ensure_link_delivery_resource_handlers();
        true
    }

    fn push_failed_outbound_state(
        &mut self,
        msg_hash: Option<[u8; 32]>,
        results: &mut Vec<(String, &'static str)>,
    ) {
        if let Some(hash) = msg_hash {
            self.clear_auto_live_fallback(&hash);
            // Not undelivered while an earlier packet can still be proven.
            if self.has_live_proof(&hash) {
                self.deferred_failures.insert(hash);
                return;
            }
            if self.ephemeral_outbound.remove(&hash) {
                return;
            }
            results.push((hex::encode(hash), "failed"));
        }
    }

    fn auto_live_fallback_hash(
        msg: &impl OutboundMessageView,
        preference: DeliveryPreference,
    ) -> Option<[u8; 32]> {
        (preference == DeliveryPreference::Auto
            && matches!(
                msg.method,
                DeliveryMethod::Direct | DeliveryMethod::Opportunistic
            ))
        .then_some(msg.message_id())
    }

    fn clear_auto_live_fallback(&mut self, hash: &[u8; 32]) {
        self.auto_live_fallback.remove(hash);
        self.live_preparation.auto_messages.remove(hash);
        self.live_preparation.discovery.remove(hash);
        self.last_reported_steps.remove(&hex::encode(hash));
    }

    fn prepare_live_message_for_propagation(message: &mut LxMessage) {
        message.method = DeliveryMethod::Propagated;
        message.delivery_attempts = 0;
        message.last_delivery_attempt = 0.0;
        message.next_delivery_attempt = 0.0;
        message.progress = 0.0;
    }

    /// Retry an Auto-selected live delivery once through the configured
    /// Offline Inbox. The caller must invoke this only after the live method
    /// has reached a terminal failure; rejection, cancellation and expiry are
    /// intentionally not fallback triggers.
    fn try_auto_propagation_fallback(
        &mut self,
        mut message: LxMessage,
        dest_hash: [u8; 16],
        reason: &str,
        results: &mut Vec<(String, &'static str)>,
    ) -> bool {
        let Some(hash) = Some(message.message_id()) else {
            return false;
        };
        if !self.auto_live_fallback.contains(&hash) {
            return false;
        }
        let Some(prop_hash) = self
            .client_propagation_enabled
            .then_some(self.router.outbound_propagation_node)
            .flatten()
        else {
            tracing::warn!(
                dest = %crate::short_id(&hex::encode(dest_hash)),
                reason,
                propagation_enabled = self.client_propagation_enabled,
                propagation_ready = self.router.outbound_propagation_node.is_some(),
                "Auto live delivery failed without an available Offline Inbox fallback"
            );
            return false;
        };

        tracing::warn!(
            dest = %crate::short_id(&hex::encode(dest_hash)),
            prop = %crate::short_id(&hex::encode(prop_hash)),
            reason,
            live_method = ?message.method,
            attempts = message.delivery_attempts,
            "Auto live delivery failed; retrying through Offline Inbox"
        );
        Self::prepare_live_message_for_propagation(&mut message);
        self.clear_auto_live_fallback(&hash);
        self.start_propagation_delivery(message, prop_hash, results);
        true
    }

    fn start_direct_link_delivery_with_results(
        &mut self,
        input: DirectLinkDeliveryStartInput,
        results: &mut Vec<(String, &'static str)>,
    ) {
        let DirectLinkDeliveryStartInput {
            message,
            dest_hash,
            hops,
            now,
            msg_hash,
            is_ephemeral,
            router_owned,
        } = input;
        let dest_hex = hex::encode(dest_hash);
        if !self.ensure_link_delivery_manager() {
            self.push_failed_outbound_state(msg_hash, results);
            return;
        }

        self.log_direct_route_state(dest_hash, now);
        let timing = self.link_timing(dest_hash);
        if let Some(ref mut ld) = self.link_delivery {
            let attempts = message.delivery_attempts;
            match ld.start_delivery_with_report_and_timing(message, dest_hash, hops, timing) {
                Ok(report) => {
                    let step = direct_link_start_step(report.kind);
                    tracing::info!(
                        link_id = %crate::short_id(&hex::encode(report.link_id)),
                        dest = %crate::short_id(&dest_hex),
                        kind = ?report.kind,
                        link_state = ?report.link_state,
                        delivery_state = ?report.delivery_state,
                        queued = report.queued_deliveries,
                        in_flight = report.in_flight_deliveries,
                        "outbound LXMF: Direct Link delivery accepted"
                    );
                    if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                        results.push((hex::encode(hash), step));
                    }
                }
                Err(err) => {
                    let reason = err.error.to_string();
                    let failed_message = *err.message;
                    tracing::warn!(
                        dest = %crate::short_id(&dest_hex),
                        attempts,
                        retryable = is_retryable_link_delivery_failure(&reason),
                        reason = "link_start_failed",
                        "outbound LXMF: failed to start Direct link delivery"
                    );
                    let requeued = if router_owned {
                        self.requeue_or_defer_direct_after_link_failure(
                            failed_message.clone(),
                            dest_hash,
                            &reason,
                            None,
                        )
                    } else {
                        self.requeue_direct_after_link_failure(
                            failed_message.clone(),
                            dest_hash,
                            &reason,
                            None,
                        )
                    };
                    if requeued {
                        if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                            results.push((hex::encode(hash), "routing"));
                        }
                    } else if !self.try_auto_propagation_fallback(
                        failed_message,
                        dest_hash,
                        &reason,
                        results,
                    ) {
                        self.push_failed_outbound_state(msg_hash, results);
                    }
                }
            }
        } else {
            self.push_failed_outbound_state(msg_hash, results);
        }
        self.drain_link_delivery_progress_updates();
    }

    /// Python `handle_outbound` pre-emptively requests an unknown path for
    /// Opportunistic messages and defers the first attempt by
    /// `PATH_REQUEST_WAIT` (LXMRouter.py:1675-1679). No-op when a path is
    /// already known or the method is not Opportunistic.
    fn preempt_opportunistic_path(&mut self, msg: &mut impl OutboundMessageView) {
        if msg.method != DeliveryMethod::Opportunistic {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if self
            .direct_route_entry(msg.destination_hash(), now)
            .is_some()
        {
            return;
        }
        self.request_path_recovery(msg.destination_hash(), None);
        msg.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
    }

    fn requeue_direct_after_link_failure(
        &mut self,
        mut message: LxMessage,
        dest_hash: [u8; 16],
        reason: &str,
        failed_link: Option<[u8; 16]>,
    ) -> bool {
        let retryable = is_retryable_link_delivery_failure(reason);
        if !matches!(
            message.method,
            DeliveryMethod::Direct | DeliveryMethod::Opportunistic
        ) || message.delivery_attempts >= MAX_DELIVERY_ATTEMPTS
            || !retryable
        {
            return false;
        }

        let drop_existing = recovery::failure_invalidates_route(reason);
        self.request_path_recovery(dest_hash, failed_link.filter(|_| drop_existing));
        // Python sets next_delivery_attempt = now + PATH_REQUEST_WAIT in the
        // closed/never-activated branch (LXMRouter.py:2640/2669).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
        tracing::warn!(
            dest = %crate::short_id(&hex::encode(dest_hash)),
            attempts = message.delivery_attempts,
            max_attempts = MAX_DELIVERY_ATTEMPTS,
            retryable,
            reason = "direct_delivery_failed",
            "direct link delivery failed before completion; rediscovering path and re-queuing"
        );
        self.queue_router_message(message, "direct link retry")
    }

    fn queue_router_message(&mut self, message: LxMessage, operation: &'static str) -> bool {
        match self.router.try_send(message) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, operation, "router rejected LXMF message");
                false
            }
        }
    }

    fn requeue_or_defer_direct_after_link_failure(
        &mut self,
        message: LxMessage,
        dest_hash: [u8; 16],
        reason: &str,
        failed_link: Option<[u8; 16]>,
    ) -> bool {
        let Some(hash) = Some(message.message_id()) else {
            return self.requeue_direct_after_link_failure(message, dest_hash, reason, failed_link);
        };
        let router_owned = self
            .router
            .pending_outbound
            .iter()
            .any(|pending| Some(pending.message_id()) == Some(hash));
        if !router_owned {
            return self.requeue_direct_after_link_failure(message, dest_hash, reason, failed_link);
        }

        let retryable = is_retryable_link_delivery_failure(reason);
        if !matches!(
            message.method,
            DeliveryMethod::Direct | DeliveryMethod::Opportunistic
        ) || message.delivery_attempts >= MAX_DELIVERY_ATTEMPTS
            || !retryable
        {
            let _ = self.router.mark_outbound_failed(&hash);
            return false;
        }

        let drop_existing = recovery::failure_invalidates_route(reason);
        self.request_path_recovery(dest_hash, failed_link.filter(|_| drop_existing));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let _ = self.router.defer_outbound_for_path_request(&hash, now);
        tracing::warn!(
            dest = %crate::short_id(&hex::encode(dest_hash)),
            attempts = message.delivery_attempts,
            max_attempts = MAX_DELIVERY_ATTEMPTS,
            retryable,
            reason = "direct_delivery_failed",
            "direct link delivery failed before completion; rediscovering path and deferring router-owned message"
        );
        true
    }

    fn log_direct_route_state(&self, dest_hash: [u8; 16], now: f64) {
        let dest_hex = hex::encode(dest_hash);
        if let Some(entry) = self.direct_route_entry(dest_hash, now) {
            tracing::info!(
                dest = %crate::short_id(&dest_hex),
                has_path = true,
                hops = entry.hops,
                path_age_secs = now - entry.timestamp,
                path_expires_in_secs = entry.expires - now,
                has_next_hop = entry.via.is_some(),
                "outbound LXMF: starting Direct delivery via Link"
            );
        } else {
            tracing::warn!(
                dest = %crate::short_id(&dest_hex),
                has_path = false,
                cached_hops = self.route_hops.get(&dest_hash).copied(),
                "outbound LXMF: Direct delivery has no current path snapshot"
            );
        }
    }

    pub fn update_lxmf_announce_app_data(
        &mut self,
        dest_hash: [u8; 16],
        name_hash: [u8; 10],
        app_data: Option<&[u8]>,
    ) -> bool {
        let Some(app_data) = app_data else {
            return false;
        };

        if name_hash == rns_identity::name_hash::name_hash(LXMF_PROPAGATION_APP_NAME) {
            if let Some(pn) = lxmf_core::handlers::parse_pn_announce_data(app_data) {
                self.router.set_stamp_cost(dest_hash, pn.stamp_cost);
                self.propagation_transfer_limits_kb
                    .insert(dest_hash, pn.transfer_limit);
                self.propagation_node_policies.insert(
                    dest_hash,
                    AdvertisedNodePolicy {
                        stamp_cost: pn.stamp_cost,
                        transfer_limit_kb: pn.transfer_limit as f64,
                        recorded_at: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs_f64(),
                    },
                );
                // A refreshed observation must be persisted even when its values match.
                return true;
            }
        } else if name_hash == rns_identity::name_hash::name_hash(LXMF_APP_NAME) {
            let mut changed = false;
            if let Some(cost) = lxmf_core::handlers::stamp_cost_from_app_data(app_data) {
                changed |= self.router.get_stamp_cost(&dest_hash) != Some(cost);
                self.router.set_stamp_cost(dest_hash, cost);
            }
            let compression_support =
                lxmf_core::handlers::compression_support_state_from_app_data(app_data);
            match compression_support {
                CompressionSupport::Supported | CompressionSupport::Unsupported => {
                    changed |= self
                        .peer_lxmf_compression_support
                        .insert(dest_hash, compression_support)
                        != Some(compression_support);
                }
                CompressionSupport::Unknown => {}
            }
            if compression_support == CompressionSupport::Unsupported
                && let Some(delivery) = self.link_delivery.as_mut()
            {
                delivery.disable_pending_direct_compression(dest_hash);
            }
            return changed;
        }
        false
    }

    /// Prefers ratchet pubkey, falls back to identity pubkey.
    pub fn encrypt_for_destination(
        &self,
        dest_hash_hex: &str,
        plaintext: &[u8],
    ) -> Option<Vec<u8>> {
        let pub_key = self.known_identities.get(dest_hash_hex)?;
        tracing::info!(dest = %crate::short_id(dest_hash_hex), "encrypting for destination — key found");
        let remote = Identity::from_public_key(pub_key).ok()?;
        let ratchet_pub = self
            .received_ratchets
            .get(dest_hash_hex)
            .filter(|rr| !rr.is_expired())
            .map(|rr| &rr.ratchet_pub);
        remote.encrypt(plaintext, ratchet_pub).ok()
    }

    pub fn decrypt_inbound(&self, ciphertext: &[u8]) -> Option<Vec<u8>> {
        let prv_keys = self.delivery_ratchets.ring().private_keys();
        let refs: Vec<&[u8; 32]> = prv_keys.iter().collect();
        let ratchets = if refs.is_empty() {
            None
        } else {
            Some(refs.as_slice())
        };
        let result = self.identity.decrypt(ciphertext, ratchets, false);
        tracing::info!(
            success = result.is_ok(),
            ct_len = ciphertext.len(),
            num_ratchets = prv_keys.len(),
            "decrypt_inbound attempt"
        );
        result.ok()
    }

    pub fn is_destination_known(&self, dest_hash_hex: &str) -> bool {
        self.known_identities.contains_key(dest_hash_hex)
    }

    /// Resolve a destination hash to its identity hash via learned announce
    /// keys — the equivalent of `RNS.Identity.recall` (LXMessage.py:776).
    pub fn recall_identity_hash(&self, dest_hash: &[u8; 16]) -> Option<[u8; 16]> {
        self.known_identities
            .get(&hex::encode(dest_hash))
            .map(|pub_key| rns_crypto::sha::truncated_hash(pub_key))
    }

    pub fn propagation_node_ready_for_send(&self, prop_hash: &[u8; 16]) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        propagation_readiness(
            *prop_hash,
            self.known_identities.contains_key(&hex::encode(prop_hash)),
            self.direct_route_entry(*prop_hash, now).is_some(),
            self.propagation_node_policies.get(prop_hash).copied(),
            true,
            now,
        )
        .is_ok()
    }

    pub fn inbound_source_public_key(&self, source: &[u8; 16]) -> Option<[u8; 64]> {
        self.known_identities.get(&hex::encode(source)).copied()
    }
    pub fn verify_inbound_signature(&self, msg: &mut LxMessage) -> Option<bool> {
        let key = self.inbound_source_public_key(&msg.source_hash())?;
        Some(msg.verify(&key).is_ok())
    }
    pub fn propagation_decrypt_context(&self) -> PropagationDecryptContext {
        PropagationDecryptContext {
            identity: self.identity.clone(),
            destination: self.lxmf_dest_hash,
            ratchets: zeroize::Zeroizing::new(
                self.delivery_ratchets.ring().private_keys().to_vec(),
            ),
        }
    }

    /// Ed25519-sign full packet hash; address to truncated hash for
    /// reverse_table routing.
    pub fn create_delivery_proof(&self, raw_packet: &[u8]) -> Option<Vec<u8>> {
        let (header, _) = rns_wire::header::PacketHeader::unpack(raw_packet).ok()?;

        let full_hash = rns_wire::hash::packet_hash(raw_packet, header.flags.header_type);
        let trunc_hash =
            rns_wire::hash::truncated_packet_hash(raw_packet, header.flags.header_type);

        let signature = self.identity.sign(&full_hash)?;

        let proof_flags = rns_wire::flags::PacketFlags {
            header_type: rns_wire::flags::HeaderType::Header1,
            context_flag: false,
            transport_type: rns_wire::flags::TransportType::Broadcast,
            destination_type: rns_wire::flags::DestinationType::Single,
            packet_type: rns_wire::flags::PacketType::Proof,
        };
        let proof_header = rns_wire::header::PacketHeader {
            flags: proof_flags,
            hops: 0,
            transport_id: None,
            destination_hash: trunc_hash,
            context: rns_wire::context::PacketContext::None,
        };
        let mut proof_raw = proof_header.pack();
        proof_raw.extend_from_slice(&signature);

        Some(proof_raw)
    }

    /// Defer ratchet cleanup +900s on foreground resume.
    pub fn mark_foreground_resume(&mut self) {
        self.last_ratchet_clean = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
    }

    pub fn take_expired_received_ratchets(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_expired_received_ratchets)
    }

    pub fn requeue_expired_received_ratchets(&mut self, hashes: Vec<String>) {
        for hash in hashes {
            if !self.pending_expired_received_ratchets.contains(&hash) {
                self.pending_expired_received_ratchets.push(hash);
            }
        }
    }

    pub fn auto_propagation_check_due(&self, network_available: bool) -> bool {
        if !network_available || !self.client_propagation_enabled {
            return false;
        }
        let Some(client) = self.propagation_client.as_ref() else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        now - self.last_propagation_check > AUTO_PROPAGATION_CHECK_INTERVAL_SECS
            && client.state() == lxmf_core::propagation_client::PropagationClientState::Idle
    }

    pub fn propagated_deposit_pending(&self) -> bool {
        !self.in_flight_propagation.is_empty()
            || self
                .router
                .pending_outbound
                .iter()
                .any(|msg| msg.method == DeliveryMethod::Propagated)
            || self
                .router
                .pending_deferred_stamps
                .values()
                .any(|msg| msg.method == DeliveryMethod::Propagated)
    }

    /// Returns `(msg_hash_hex, status)` for each send outcome.
    pub fn tick(&mut self) -> Vec<(String, &'static str)> {
        self.tick_with_auto_propagation_download_ready(true)
    }

    /// Register the router task for Direct delivery input and local receipts.
    /// Periodic maintenance remains separately scheduled by the embedding task.
    pub fn poll_delivery_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        self.link_delivery
            .as_mut()
            .map_or(std::task::Poll::Pending, |ld| ld.poll_ready(cx))
    }

    /// Advance Direct delivery only. Safe to call on packet readiness without
    /// accelerating router jobs, persistence, discovery or automatic downloads.
    pub fn process_delivery_events(&mut self) -> Vec<(String, &'static str)> {
        let mut results = Vec::new();
        if let Some(ref mut ld) = self.link_delivery {
            ld.drain_events(&self.known_identities);
        }

        if let Some(ref mut ld) = self.link_delivery {
            let delivery_results = ld.tick();
            for result in delivery_results {
                self.handle_link_delivery_result(result, &mut results);
            }
            self.drain_link_delivery_progress_updates();
        }
        self.drain_core_backchannel_resource_cancellations();

        results
    }

    /// Like [`Self::tick`], but only starts automatic Offline Inbox downloads
    /// when the selected propagation node is already reachable and metadata-ready.
    /// In-flight syncs still advance so they can finish or fail normally.
    pub fn tick_with_auto_propagation_download_ready(
        &mut self,
        auto_download_ready: bool,
    ) -> Vec<(String, &'static str)> {
        let mut results = Vec::new();

        // 15-min ratchet cleanup cadence (matches reference). Only inspect
        // coherent in-memory metadata here. The serialized persistence owner
        // revalidates and removes files after this manager lock is released.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        if now - self.last_ratchet_clean > 900.0 {
            let expired = self
                .received_ratchets
                .iter()
                .filter(|(_, ratchet)| ratchet.is_expired())
                .map(|(hash, _)| hash.clone())
                .collect::<Vec<_>>();
            for hash in &expired {
                self.received_ratchets.remove(hash);
                self.dirty_received_ratchets.remove(hash);
            }
            if !expired.is_empty() {
                let removed = expired.len();
                self.pending_expired_received_ratchets.extend(expired);
                tracing::debug!(removed, "ratchet cleanup pass: expired in-memory entries");
            }
            self.last_ratchet_clean = now;
        }

        self.poll_opportunistic_proofs(&mut results);
        self.retry_due_opportunistic_deliveries(now, &mut results);
        self.poll_path_recoveries();
        self.hold_messages_for_path_recovery(now);
        self.hold_messages_for_timing(now);
        self.drain_backchannel_events(&mut results);
        self.queue_resumable(now, &mut results);
        self.router.process_deferred_stamps();
        results.extend(self.prepare_live_outbound(now, Instant::now()));
        let known_identities = self
            .known_identities
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        let route_entries = self.route_entries.clone();
        let direct_destinations = self
            .router
            .pending_outbound
            .iter()
            .filter(|message| message.method == DeliveryMethod::Direct)
            .map(|message| message.destination_hash())
            .collect::<HashSet<_>>();
        let reusable_links = direct_destinations
            .iter()
            .copied()
            .map(|dest| (dest, self.direct_reusable_link_state_for_router(dest)))
            .collect::<HashMap<_, _>>();
        let actions = self.router.process_outbound_with_direct(|message, now| {
            let dest = message.destination_hash();
            let route = route_entries
                .get(&dest)
                .filter(|entry| entry.expires > now)
                .map(|entry| direct_route_snapshot_from_entry(dest, entry));
            DirectDeliveryPlanInput {
                identity_known: known_identities.contains(&hex::encode(dest)),
                route,
                reusable_link: reusable_links
                    .get(&dest)
                    .copied()
                    .unwrap_or(DirectReusableLinkState::None),
            }
        });
        if !actions.is_empty() {
            results.extend(self.execute_encrypted_actions(actions));
            self.drain_link_delivery_progress_updates();
        }
        // Advance the router jobloop (transient caches, store cull, peer
        // rotation at Python cadences) — process_outbound_with_direct does
        // not drive it. Internally time-gated, safe at the 500 ms tick rate.
        self.router.run_jobs_tick();
        if now - self.last_router_cull > 300.0 {
            self.router.cull_stamp_costs();
            self.last_router_cull = now;
        }

        results.extend(self.process_delivery_events());

        if let Some(ref mut ps) = self.propagation_sync {
            ps.drain_events(&self.known_identities);
            ps.tick();
        }

        let mut downloaded = Vec::new();
        if let Some(ref mut client) = self.propagation_client {
            client.drain_events(&self.known_identities);
            client.tick();
            let transfer_status = client.transfer_status();
            let terminal_state = if matches!(
                transfer_status.state,
                lxmf_core::propagation_client::PropagationClientState::Complete
                    | lxmf_core::propagation_client::PropagationClientState::Partial
                    | lxmf_core::propagation_client::PropagationClientState::Failed
            ) {
                Some(transfer_status.state)
            } else {
                None
            };
            if let Some(node) = self.router.outbound_propagation_node {
                match terminal_state {
                    Some(lxmf_core::propagation_client::PropagationClientState::Complete) => {
                        self.completed_propagation_syncs.push(node);
                    }
                    Some(lxmf_core::propagation_client::PropagationClientState::Partial) => {
                        self.failed_propagation_syncs
                            .push((node, "sync_partial".to_string()));
                    }
                    Some(lxmf_core::propagation_client::PropagationClientState::Failed) => {
                        self.failed_propagation_syncs
                            .push((node, "sync_failed".to_string()));
                    }
                    _ => {}
                }
            }
            downloaded = client.take_downloaded_messages();
            if terminal_state.is_some() {
                self.last_propagation_admission = transfer_status.admission;
                client.acknowledge_transfer();
            }

            // Auto-poll every 5 minutes when idle. Missing relay readiness is
            // not an inbox check, so it must not consume the pickup interval.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            if self.client_propagation_enabled
                && now - self.last_propagation_check > AUTO_PROPAGATION_CHECK_INTERVAL_SECS
                && client.state() == lxmf_core::propagation_client::PropagationClientState::Idle
                && auto_download_ready
            {
                self.last_propagation_check = now;
                client.start_download();
                tracing::debug!("auto-triggered propagation download check");
            }
        }
        self.downloaded_propagation_messages.extend(downloaded);

        self.filter_repeated_steps(results)
    }

    fn filter_repeated_steps(
        &mut self,
        results: Vec<(String, &'static str)>,
    ) -> Vec<(String, &'static str)> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.last_reported_steps
            .retain(|_, reported| now - reported.observed_at <= REPORTED_STEP_TTL_SECS);

        let mut filtered = Vec::with_capacity(results.len());
        for (msg_id, step) in results {
            if self
                .last_reported_steps
                .get(&msg_id)
                .is_some_and(|reported| reported.step == step)
            {
                tracing::trace!(
                    msg_id = %crate::short_id(&msg_id),
                    step,
                    "suppressing repeated LXMF step"
                );
                continue;
            }

            if terminal_delivery_step(step) {
                if let Some(id) = hex::decode(&msg_id)
                    .ok()
                    .and_then(|v| <[u8; 32]>::try_from(v).ok())
                {
                    self.propagation_envelopes.remove(&id);
                    self.pending_propagation_envelopes.remove(&id);
                }
                self.last_reported_steps.remove(&msg_id);
            } else {
                if self.last_reported_steps.len() >= MAX_REPORTED_STEPS
                    && !self.last_reported_steps.contains_key(&msg_id)
                    && let Some(oldest) = self
                        .last_reported_steps
                        .iter()
                        .min_by(|a, b| a.1.observed_at.total_cmp(&b.1.observed_at))
                        .map(|(id, _)| id.clone())
                {
                    self.last_reported_steps.remove(&oldest);
                }
                self.last_reported_steps.insert(
                    msg_id.clone(),
                    ReportedStep {
                        step,
                        observed_at: now,
                    },
                );
            }
            filtered.push((msg_id, step));
        }
        filtered
    }

    pub fn start_propagation_sync(&mut self, node_dest_hash: [u8; 16]) {
        let transport_tx = match &self.router.transport_tx {
            Some(tx) => tx.clone(),
            None => return,
        };

        let mut task = lxmf_core::propagation_sync::PropagationSyncTask::new(
            transport_tx,
            self.lxmf_dest_hash,
        );
        task.set_node(node_dest_hash);
        self.propagation_sync = Some(task);
        tracing::info!(
            node = %crate::short_id(&hex::encode(node_dest_hash)),
            "propagation sync enabled"
        );
    }

    pub fn stop_propagation_sync(&mut self) {
        self.propagation_sync = None;
        tracing::info!("propagation sync disabled");
    }

    pub fn take_downloaded_propagation_messages(&mut self) -> Vec<DownloadedPropagationMessage> {
        std::mem::take(&mut self.downloaded_propagation_messages)
    }

    pub fn take_propagation_health(&mut self) -> PropagationHealth {
        (
            std::mem::take(&mut self.completed_propagation_deposits),
            std::mem::take(&mut self.failed_propagation_deposits),
            std::mem::take(&mut self.completed_propagation_syncs),
            std::mem::take(&mut self.failed_propagation_syncs),
        )
    }

    pub fn take_delivery_progress_updates(&mut self) -> Vec<LxmfDeliveryProgressUpdate> {
        std::mem::take(&mut self.delivery_progress_updates)
    }

    pub fn take_delivery_failure_updates(&mut self) -> Vec<LxmfDeliveryFailureUpdate> {
        std::mem::take(&mut self.delivery_failure_updates)
    }

    /// Apply a validated Reticulum proof to an Opportunistic message retained
    /// outside the router while its packet was in flight.
    pub fn complete_opportunistic_delivery(&mut self, msg_id: &str) -> bool {
        let Ok(decoded) = hex::decode(msg_id) else {
            return false;
        };
        let Ok(hash) = <[u8; 32]>::try_from(decoded.as_slice()) else {
            return false;
        };
        let Some(owner) = self.opportunistic_proofs.remove(&hash) else {
            return false;
        };

        self.deferred_failures.remove(&hash);
        self.resumable.retain(|send| send.message_id != hash);
        self.opportunistic_in_flight.remove(&hash);
        self.router.cancel_outbound(&hash);
        if let Some(delivery) = &mut self.link_delivery {
            delivery.cancel_delivery_by_message_hash(hash);
        }
        self.drain_core_backchannel_resource_cancellations();
        self.in_flight_propagation.remove(&hash);

        self.clear_auto_live_fallback(&hash);
        self.ephemeral_outbound.remove(&hash);
        self.router.complete_outbound_message(owner.message);
        self.pending_path_recoveries.retain(|dest, _| {
            self.router
                .pending_outbound
                .iter()
                .any(|message| message.destination_hash() == *dest)
        });
        true
    }

    fn retry_due_opportunistic_deliveries(
        &mut self,
        now: f64,
        results: &mut Vec<(String, &'static str)>,
    ) {
        let due = self
            .opportunistic_in_flight
            .iter()
            .filter_map(|(hash, pending)| (pending.retry_at <= now).then_some(*hash))
            .collect::<Vec<_>>();

        for hash in due {
            let Some(pending) = self.opportunistic_in_flight.remove(&hash) else {
                continue;
            };
            if let Err(error) = self.router.try_send(pending.message) {
                tracing::warn!(
                    msg_id = %crate::short_id(&hex::encode(hash)),
                    %error,
                    "failed to schedule opportunistic delivery retry"
                );
                self.clear_auto_live_fallback(&hash);
                if !self.ephemeral_outbound.remove(&hash) {
                    results.push((hex::encode(hash), "failed"));
                }
            }
        }
    }

    pub fn cancel_outbound_message(&mut self, msg_id: &str) -> bool {
        let Ok(decoded) = hex::decode(msg_id.trim()) else {
            return false;
        };
        let Ok(hash) = <[u8; 32]>::try_from(decoded.as_slice()) else {
            return false;
        };

        let mut cancelled = false;
        self.clear_auto_live_fallback(&hash);
        let resumable = self.resumable.len();
        self.resumable.retain(|send| send.message_id != hash);
        cancelled |= self.resumable.len() != resumable;
        cancelled |= self.deferred_failures.remove(&hash);
        cancelled |= self.ephemeral_outbound.remove(&hash);
        cancelled |= self.in_flight_propagation.remove(&hash).is_some();
        self.propagation_envelopes.remove(&hash);
        self.pending_propagation_envelopes.remove(&hash);
        cancelled |= self.opportunistic_in_flight.remove(&hash).is_some();
        // Packets already sent keep their receipts: a copy that arrives still
        // shows as delivered.
        cancelled |= self.opportunistic_proofs.contains_key(&hash);

        if self
            .link_delivery
            .as_mut()
            .is_some_and(|delivery| delivery.cancel_delivery_by_message_hash(hash))
        {
            cancelled = true;
        }
        self.drain_core_backchannel_resource_cancellations();
        if self.router.cancel_outbound(&hash) {
            cancelled = true;
        }

        self.pending_path_recoveries.retain(|dest, _| {
            self.router
                .pending_outbound
                .iter()
                .any(|message| message.destination_hash() == *dest)
        });

        cancelled
    }

    fn handle_link_delivery_result(
        &mut self,
        result: DeliveryResult,
        results: &mut Vec<(String, &'static str)>,
    ) {
        match result {
            DeliveryResult::Complete { msg_hash, .. } => {
                if let Some(hash) = msg_hash {
                    self.propagation_envelopes.remove(&hash);
                    self.pending_propagation_envelopes.remove(&hash);
                    self.opportunistic_proofs.remove(&hash);
                    self.opportunistic_in_flight.remove(&hash);
                    self.clear_auto_live_fallback(&hash);
                    if self.ephemeral_outbound.remove(&hash) {
                        return;
                    }
                    // Propagation deposit confirms node-storage, not recipient
                    // delivery. Direct/backchannel link delivery is end-to-end.
                    let state = if let Some(prop_hash) = self.in_flight_propagation.remove(&hash) {
                        self.completed_propagation_deposits.push(prop_hash);
                        "propagated"
                    } else {
                        let _ = self.router.mark_outbound_delivered(&hash);
                        "delivered"
                    };
                    results.push((hex::encode(hash), state));
                }
            }
            DeliveryResult::Rejected {
                msg_hash,
                dest_hash,
                reason,
                ..
            } => {
                tracing::warn!(
                    dest = %crate::short_id(&hex::encode(dest_hash)),
                    reason = "rejected",
                    "link delivery rejected"
                );
                if let Some(hash) = msg_hash {
                    self.clear_auto_live_fallback(&hash);
                    if self.ephemeral_outbound.remove(&hash) {
                        return;
                    }
                    if let Some(prop_hash) = self.in_flight_propagation.remove(&hash) {
                        self.failed_propagation_deposits.push((prop_hash, reason));
                        results.push((hex::encode(hash), "failed"));
                        return;
                    }
                    let _ = self.router.mark_outbound_rejected(&hash);
                    results.push((hex::encode(hash), "rejected"));
                }
            }
            DeliveryResult::Failed {
                link_id,
                msg_hash,
                dest_hash,
                message,
                reason,
                ..
            } => {
                tracing::warn!(
                    dest = %crate::short_id(&hex::encode(dest_hash)),
                    reason = "delivery_failed",
                    "link delivery failed"
                );
                if let Some(hash) = msg_hash {
                    if self.ephemeral_outbound.remove(&hash) {
                        return;
                    }
                    if let Some(prop_hash) = self.in_flight_propagation.remove(&hash) {
                        self.clear_auto_live_fallback(&hash);
                        self.failed_propagation_deposits
                            .push((prop_hash, reason.clone()));
                        results.push((hex::encode(hash), "failed"));
                        return;
                    }
                    if self.requeue_or_defer_direct_after_link_failure(
                        message.clone(),
                        dest_hash,
                        &reason,
                        Some(link_id),
                    ) {
                        results.push((hex::encode(hash), "routing"));
                        return;
                    }
                    if self.try_auto_propagation_fallback(message, dest_hash, &reason, results) {
                        return;
                    }
                    self.clear_auto_live_fallback(&hash);
                    results.push((hex::encode(hash), "failed"));
                } else {
                    let _ = self.requeue_or_defer_direct_after_link_failure(
                        message,
                        dest_hash,
                        &reason,
                        Some(link_id),
                    );
                }
            }
        }
    }

    fn drain_link_delivery_progress_updates(&mut self) {
        let events = if let Some(ref mut ld) = self.link_delivery {
            ld.take_delivery_events()
        } else {
            Vec::new()
        };
        self.delivery_progress_updates.extend(
            events
                .into_iter()
                .filter_map(Self::progress_update_from_link_event),
        );
    }

    fn progress_update_from_link_event(
        event: LxmfDeliveryEvent,
    ) -> Option<LxmfDeliveryProgressUpdate> {
        let msg_hash = event.msg_hash?;
        let kind = match event.kind {
            LxmfDeliveryEventKind::LinkEstablishing => LxmfDeliveryProgressKind::LinkEstablishing,
            LxmfDeliveryEventKind::LinkEstablished => LxmfDeliveryProgressKind::LinkEstablished,
            LxmfDeliveryEventKind::DirectLinkPending => LxmfDeliveryProgressKind::DirectLinkPending,
            LxmfDeliveryEventKind::DirectLinkReused => LxmfDeliveryProgressKind::DirectLinkReused,
            LxmfDeliveryEventKind::BackchannelLinkReused => {
                LxmfDeliveryProgressKind::BackchannelLinkReused
            }
            LxmfDeliveryEventKind::TransferStarted => LxmfDeliveryProgressKind::TransferStarted,
            LxmfDeliveryEventKind::TransferProgress => LxmfDeliveryProgressKind::TransferProgress,
            LxmfDeliveryEventKind::AwaitingProof => LxmfDeliveryProgressKind::AwaitingProof,
            LxmfDeliveryEventKind::Delivered => LxmfDeliveryProgressKind::Delivered,
            LxmfDeliveryEventKind::Rejected => LxmfDeliveryProgressKind::Rejected,
            LxmfDeliveryEventKind::Failed => LxmfDeliveryProgressKind::Failed,
        };
        let event_method = match event.method {
            LxmfDeliveryEventMethod::Direct => LxmfDeliveryProgressMethod::Direct,
            LxmfDeliveryEventMethod::PropagationDeposit => {
                LxmfDeliveryProgressMethod::PropagationDeposit
            }
        };
        let delivery_representation = match event.representation {
            DeliveryRepresentation::Unknown => LxmfDeliveryProgressRepresentation::Unknown,
            DeliveryRepresentation::Packet => LxmfDeliveryProgressRepresentation::Packet,
            DeliveryRepresentation::Resource => LxmfDeliveryProgressRepresentation::Resource,
            DeliveryRepresentation::Paper => LxmfDeliveryProgressRepresentation::Paper,
        };
        let step = match event.kind {
            LxmfDeliveryEventKind::LinkEstablishing => "link_establishing",
            LxmfDeliveryEventKind::LinkEstablished => {
                if event.representation == DeliveryRepresentation::Resource {
                    "resource_link_ready"
                } else {
                    "sending_via_link"
                }
            }
            LxmfDeliveryEventKind::TransferStarted => {
                if event.representation == DeliveryRepresentation::Resource {
                    "resource_advertised"
                } else {
                    "sending_via_link"
                }
            }
            LxmfDeliveryEventKind::TransferProgress => {
                if event.representation == DeliveryRepresentation::Resource {
                    if event.delivery_state == DeliveryState::AwaitingProof
                        || event.progress.unwrap_or_default() >= 0.99
                    {
                        "resource_waiting_for_proof"
                    } else {
                        "resource_transferring"
                    }
                } else {
                    "sending_via_link"
                }
            }
            LxmfDeliveryEventKind::AwaitingProof => {
                if event.representation == DeliveryRepresentation::Resource {
                    "resource_waiting_for_proof"
                } else {
                    "sending_via_link"
                }
            }
            LxmfDeliveryEventKind::DirectLinkPending => "sending_via_link",
            LxmfDeliveryEventKind::DirectLinkReused => "reusing_direct_link",
            LxmfDeliveryEventKind::BackchannelLinkReused => "reusing_backchannel",
            LxmfDeliveryEventKind::Delivered => "delivered",
            LxmfDeliveryEventKind::Rejected => "rejected",
            LxmfDeliveryEventKind::Failed => "failed",
        };
        Some(LxmfDeliveryProgressUpdate {
            msg_id: hex::encode(msg_hash),
            kind,
            event_method,
            delivery_representation,
            step,
            method: match event.method {
                LxmfDeliveryEventMethod::Direct => "direct",
                LxmfDeliveryEventMethod::PropagationDeposit => "propagated",
            },
            progress: event.progress,
            link_id: Some(hex::encode(event.link_id)),
            dest_hash: hex::encode(event.dest_hash),
            attempts: event.attempts,
            representation: match event.representation {
                DeliveryRepresentation::Unknown => "unknown",
                DeliveryRepresentation::Packet => "packet",
                DeliveryRepresentation::Resource => "resource",
                DeliveryRepresentation::Paper => "paper",
            },
            queued_deliveries: event.queued_deliveries,
            in_flight_deliveries: event.in_flight_deliveries,
            reason: event.reason,
        })
    }

    /// Revoke envelope preparation and return its actual blocking owners. The
    /// lifecycle caller must await these handles outside the manager lock before
    /// dropping the identity or considering shutdown complete.
    pub fn cancel_and_take_propagation_workers(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        self.pending_propagation_envelopes.clear();
        self.propagation_envelopes.clear();
        std::mem::take(&mut self.propagation_preparation_tasks)
    }

    fn pack_message_for_propagation(
        &mut self,
        message: &LxMessage,
        prepared: lxmf_core::propagation_send::PreparedNode,
        now: f64,
    ) -> PreparedPropagation {
        let id = message.message_id();
        let dest = hex::encode(message.destination_hash());
        let Some(key) = self.known_identities.get(&dest).copied() else {
            return PreparedPropagation::Failed(MessageError::PackFailed(
                "recipient key missing".into(),
            ));
        };
        let ratchet = self
            .received_ratchets
            .get(&dest)
            .filter(|r| !r.is_expired())
            .map(|r| r.ratchet_pub);
        let mut binding_data = key.to_vec();
        if let Some(ratchet) = ratchet {
            binding_data.extend_from_slice(&ratchet);
        }
        let binding = rns_crypto::sha::full_hash(&binding_data);
        if let Some(job) = self.pending_propagation_envelopes.get_mut(&id) {
            // Dropping a receiver revokes this generation only; the worker owns
            // its permit and message until it really exits.
            if job.binding != binding || job.cost != prepared.policy.stamp_cost {
                self.pending_propagation_envelopes.remove(&id);
            } else {
                match job.result.try_recv() {
                    Ok(Ok(envelope)) => {
                        self.pending_propagation_envelopes.remove(&id);
                        self.propagation_envelopes.insert(id, envelope);
                    }
                    Ok(Err(error)) => {
                        self.pending_propagation_envelopes.remove(&id);
                        return PreparedPropagation::Failed(error);
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {
                        return PreparedPropagation::Pending;
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        self.pending_propagation_envelopes.remove(&id);
                        return PreparedPropagation::Failed(MessageError::PackFailed(
                            "propagation worker unavailable".into(),
                        ));
                    }
                }
            }
        }
        if let Some(envelope) = self.propagation_envelopes.get_mut(&id)
            && envelope.matches(message, &binding)
            && envelope.stamp_value() >= u32::from(prepared.policy.stamp_cost)
        {
            return match envelope.pack_for(prepared, now) {
                Ok(bytes) => PreparedPropagation::Ready(bytes),
                Err(e) => PreparedPropagation::Failed(e),
            };
        }
        let Ok(permit) = propagation_preparation_slots().try_acquire_owned() else {
            return PreparedPropagation::Pending;
        };
        let previous = self
            .propagation_envelopes
            .remove(&id)
            .filter(|e| e.matches(message, &binding));
        let message = message.clone();
        let cost = prepared.policy.stamp_cost;
        let (tx, result) = oneshot::channel();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        self.propagation_preparation_tasks
            .retain(|task| !task.is_finished());
        self.propagation_preparation_tasks
            .push(tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let outcome = (|| {
                    let mut envelope = match previous {
                        Some(envelope) => envelope,
                        None => PropagationEnvelope::new_cancellable(
                            message,
                            binding,
                            cost,
                            |plaintext| {
                                let remote = Identity::from_public_key(&key)
                                    .map_err(|e| MessageError::PackFailed(e.to_string()))?;
                                remote
                                    .encrypt(plaintext, ratchet.as_ref())
                                    .map_err(|e| MessageError::PackFailed(e.to_string()))
                            },
                            &worker_cancel,
                        )?,
                    };
                    // This is the only potentially expensive path; policy is checked
                    // again on the manager before dispatch.
                    envelope.pack_for_cancellable(prepared, now, &worker_cancel)?;
                    Ok(envelope)
                })();
                let _ = tx.send(outcome);
            }));
        self.pending_propagation_envelopes.insert(
            id,
            PendingPropagationEnvelope {
                binding,
                cost,
                result,
                cancel,
            },
        );
        PreparedPropagation::Pending
    }

    fn start_propagation_delivery(
        &mut self,
        mut message: LxMessage,
        prop_hash: [u8; 16],
        results: &mut Vec<(String, &'static str)>,
    ) {
        if !self.timing_ready(prop_hash) {
            message.next_delivery_attempt = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                + 0.5;
            self.queue_router_message(message, "propagation timing observation");
            return;
        }
        let msg_hash = Some(message.message_id());
        let dest_hex = hex::encode(message.destination_hash());
        let prop_hex = hex::encode(prop_hash);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let prepared = match propagation_readiness(
            prop_hash,
            self.known_identities.contains_key(&prop_hex),
            self.direct_route_entry(prop_hash, now).is_some(),
            self.propagation_node_policies.get(&prop_hash).copied(),
            self.known_identities.contains_key(&dest_hex),
            now,
        ) {
            Ok(prepared) => prepared,
            Err(lxmf_core::propagation_send::MissingPropagationInput::RecipientIdentity) => {
                let destination = message.destination_hash();
                self.defer_propagation_delivery(message, destination);
                return;
            }
            Err(_) => {
                self.defer_propagation_delivery(message, prop_hash);
                return;
            }
        };
        let packed = match self.pack_message_for_propagation(&message, prepared, now) {
            PreparedPropagation::Ready(packed) => packed,
            PreparedPropagation::Pending => {
                message.next_delivery_attempt = now + 0.5;
                // Preparation is not a transmitted payload attempt.
                message.delivery_attempts = message.delivery_attempts.saturating_sub(1);
                self.queue_router_message(message, "propagation envelope preparation");
                return;
            }
            PreparedPropagation::Failed(error) => {
                tracing::warn!(%error,"propagation envelope preparation failed");
                if let MessageError::TooLarge { size, limit } = &error {
                    self.delivery_failure_updates
                        .push(LxmfDeliveryFailureUpdate {
                            msg_id: hex::encode(message.message_id()),
                            code: "propagation_limit_exceeded",
                            actual_bytes: *size,
                            limit_bytes: *limit,
                        });
                }
                self.propagation_envelopes.remove(&message.message_id());
                results.push((hex::encode(message.message_id()), "failed"));
                return;
            }
        };

        let _ = self.ensure_link_delivery_manager();

        let timing = self.link_timing(prop_hash);
        let hops = self.delivery_link_hops(prop_hash);
        if let Some(ref mut ld) = self.link_delivery {
            match ld
                .start_packed_delivery_with_timing(message, prop_hash, hops, packed, false, timing)
            {
                Ok(_) => {
                    if let Some(hash) = msg_hash {
                        self.in_flight_propagation.insert(hash, prop_hash);
                        results.push((hex::encode(hash), "propagating"));
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        reason = "link_start_failed",
                        prop = %crate::short_id(&hex::encode(prop_hash)),
                        "failed to start propagation link delivery"
                    );
                    if let Some(hash) = msg_hash {
                        results.push((hex::encode(hash), "failed"));
                    }
                }
            }
        } else if let Some(hash) = msg_hash {
            results.push((hex::encode(hash), "failed"));
        }
    }

    fn defer_propagation_delivery(&mut self, mut message: LxMessage, request_hash: [u8; 16]) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        message.last_delivery_attempt = now;
        // Waiting on propagation-node path/identity/stamp — Python defers
        // PATH_REQUEST_WAIT after the path request (LXMRouter.py:2726).
        message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
        if let Some(ref tx) = self.router.transport_tx {
            let _ = tx.try_send(TransportMessage::RequestPath {
                destination_hash: request_hash,
            });
        }
        self.queue_router_message(message, "propagation path wait");
    }

    fn drain_backchannel_events(&mut self, results: &mut Vec<(String, &'static str)>) {
        while let Some(receipt) = self
            .pending_backchannel_abandon
            .take()
            .or_else(|| self.backchannel_abandon_rx.try_recv().ok())
        {
            match receipt {
                BackchannelSendReceipt::Packet {
                    link_id,
                    packet_hash,
                } => {
                    if let Some(ld) = &mut self.link_delivery
                        && !ld.abandon_backchannel_packet(link_id, packet_hash)
                    {
                        self.pending_backchannel_abandon = Some(BackchannelSendReceipt::Packet {
                            link_id,
                            packet_hash,
                        });
                        break;
                    }
                }
                BackchannelSendReceipt::Resource {
                    link_id,
                    resource_hash,
                } => {
                    self.pending_backchannel_resource_cancellations.push_back(
                        lxmf_core::link_delivery::BackchannelResourceCancelRequest {
                            link_id,
                            resource_hash,
                        },
                    );
                }
            }
        }
        self.ensure_link_delivery_backchannel_sender();
        self.expire_pending_direct_link_identifications();

        if let Some(rx) = self.lxmf_link_identified_rx.as_mut() {
            let mut identified = Vec::new();
            while let Ok(item) = rx.try_recv() {
                identified.push(item);
            }
            for (link_id, identity_hash) in identified {
                if !self.ensure_link_delivery_manager() {
                    continue;
                }
                let dest_hash =
                    Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity_hash));
                self.pending_direct_link_identifications.remove(&dest_hash);
                if let Some(ref mut ld) = self.link_delivery {
                    ld.register_backchannel(dest_hash, link_id);
                }
                tracing::info!(
                    link_id = %crate::short_id(&hex::encode(link_id)),
                    identity = %crate::short_id(&hex::encode(identity_hash)),
                    dest = %crate::short_id(&hex::encode(dest_hash)),
                    "LXMF inbound Link identified; registered core backchannel"
                );
            }
        }

        let mut backchannel_events = Vec::new();
        if let Some(rx) = self.lxmf_backchannel_event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                backchannel_events.push(event);
            }
        }
        for event in backchannel_events {
            match event {
                BackchannelLinkEvent::PacketWait {
                    link_id,
                    packet_hash,
                    started_at,
                    timeout,
                    awaiting_admission,
                    cancellation,
                } => {
                    if let Some(ld) = &mut self.link_delivery {
                        ld.observe_backchannel_packet_wait(
                            link_id,
                            packet_hash,
                            started_at,
                            timeout,
                            awaiting_admission,
                            cancellation,
                        );
                    }
                }
                BackchannelLinkEvent::ResourceWait {
                    link_id,
                    resource_hash,
                    started_at,
                    timeout,
                } => {
                    if let Some(ld) = &mut self.link_delivery {
                        ld.observe_backchannel_resource_wait(
                            link_id,
                            resource_hash,
                            started_at,
                            timeout,
                        );
                    }
                }
                BackchannelLinkEvent::PacketProof(proof) => {
                    if let Some(result) = self.link_delivery.as_mut().and_then(|ld| {
                        ld.handle_backchannel_packet_proof(proof.link_id, proof.packet_hash)
                    }) {
                        self.handle_link_delivery_result(result, results);
                    }
                }
                BackchannelLinkEvent::ResourceConclusion {
                    link_id,
                    resource_hash,
                    conclusion,
                } => {
                    let result = self.link_delivery.as_mut().and_then(|ld| match conclusion {
                        rns_runtime::link_manager::LinkResourceConclusion::Complete => {
                            ld.handle_backchannel_resource_proof(link_id, resource_hash)
                        }
                        rns_runtime::link_manager::LinkResourceConclusion::Rejected => ld
                            .handle_backchannel_resource_conclusion(
                                link_id,
                                resource_hash,
                                lxmf_core::link_delivery::BackchannelResourceConclusion::Rejected,
                                "resource rejected",
                            ),
                        rns_runtime::link_manager::LinkResourceConclusion::Failed(reason) => ld
                            .handle_backchannel_resource_conclusion(
                                link_id,
                                resource_hash,
                                lxmf_core::link_delivery::BackchannelResourceConclusion::Failed,
                                reason,
                            ),
                        rns_runtime::link_manager::LinkResourceConclusion::Cancelled => ld
                            .handle_backchannel_resource_conclusion(
                                link_id,
                                resource_hash,
                                lxmf_core::link_delivery::BackchannelResourceConclusion::Failed,
                                "resource cancelled",
                            ),
                    });
                    if let Some(result) = result {
                        self.handle_link_delivery_result(result, results);
                    }
                }
                BackchannelLinkEvent::LinkClosed { link_id } => {
                    self.pending_direct_link_identifications
                        .retain(|_, pending| pending.link_id != link_id);
                    let closed_results = self
                        .link_delivery
                        .as_mut()
                        .map(|ld| ld.fail_backchannel_link(link_id, "link closed"))
                        .unwrap_or_default();
                    tracing::debug!(
                        link_id = %crate::short_id(&hex::encode(link_id)),
                        failed_deliveries = closed_results.len(),
                        "LXMF inbound Link closed; removed core backchannel state"
                    );
                    for result in closed_results {
                        self.handle_link_delivery_result(result, results);
                    }
                }
            }
        }

        self.drain_core_backchannel_send_commands();
        self.drain_core_backchannel_resource_cancellations();
        self.drain_link_delivery_progress_updates();
    }

    fn drain_core_backchannel_send_commands(&mut self) {
        let Some(rx) = self.lxmf_backchannel_command_rx.as_mut() else {
            return;
        };
        let Some(command_tx) = self.lxmf_link_command_tx.clone() else {
            while let Ok(command) = rx.try_recv() {
                let _ = command
                    .result_tx
                    .send(Err(BackchannelSendError::TransportUnavailable));
            }
            return;
        };

        while let Ok(command) = rx.try_recv() {
            if command.result_tx.is_closed() {
                continue;
            }
            let Ok(bridge_slot) = self.backchannel_bridge_slots.clone().try_acquire_owned() else {
                let _ = command
                    .result_tx
                    .send(Err(BackchannelSendError::TransportUnavailable));
                continue;
            };
            let (result_tx, result_rx) = oneshot::channel();
            let link_id = command.link_id;
            let link_command = rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                link_id,
                payload: command.payload,
                auto_compress: command.auto_compress,
                result_tx: Some(result_tx),
            };
            match command_tx.try_send(link_command) {
                Ok(()) => {
                    let abandon_tx = self.backchannel_abandon_tx.clone();
                    tokio::spawn(async move {
                        let _bridge_slot = bridge_slot;
                        forward_backchannel_receipt(result_rx, command.result_tx, abandon_tx).await;
                    });
                }
                Err(_) => {
                    tracing::warn!(
                        link_id = %crate::short_id(&hex::encode(link_id)),
                        reason = "queue_failed",
                        "failed to queue LXMF backchannel send command"
                    );
                    let _ = command
                        .result_tx
                        .send(Err(BackchannelSendError::TransportUnavailable));
                }
            }
        }
    }

    fn drain_core_backchannel_resource_cancellations(&mut self) {
        if let Some(ref mut delivery) = self.link_delivery {
            self.pending_backchannel_resource_cancellations
                .extend(delivery.take_backchannel_resource_cancellations());
        }

        let Some(command_tx) = self.lxmf_link_command_tx.as_ref() else {
            return;
        };
        while let Some(request) = self
            .pending_backchannel_resource_cancellations
            .front()
            .copied()
        {
            match command_tx.try_reserve() {
                Ok(permit) => {
                    self.pending_backchannel_resource_cancellations.pop_front();
                    permit.send(
                        rns_runtime::link_manager::LinkManagerCommand::CancelLinkResource {
                            link_id: request.link_id,
                            resource_id: request.resource_hash,
                            direction: rns_runtime::link_manager::LinkResourceDirection::Outbound,
                            result_tx: None,
                        },
                    );
                }
                Err(mpsc::error::TrySendError::Full(_)) => break,
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::warn!(
                        pending = self.pending_backchannel_resource_cancellations.len(),
                        "LXMF backchannel Resource cancellation channel closed"
                    );
                    self.pending_backchannel_resource_cancellations.clear();
                    break;
                }
            }
        }
    }

    fn execute_encrypted_actions(
        &mut self,
        actions: Vec<OutboundAction>,
    ) -> Vec<(String, &'static str)> {
        let mut results = Vec::new();

        for action in actions {
            let (message, dest_hash, is_opportunistic, mut direct_plan) = match action {
                OutboundAction::DeliverDirect { message, dest_hash } => {
                    (message, dest_hash, false, None)
                }
                OutboundAction::PlanDirect {
                    message,
                    dest_hash,
                    plan,
                } => (message, dest_hash, false, Some(plan)),
                OutboundAction::DeliverOpportunistic { message, dest_hash } => {
                    (message, dest_hash, true, None)
                }
                OutboundAction::DeliverPropagated { message, prop_hash } => {
                    tracing::info!(
                        dest = %crate::short_id(&hex::encode(message.destination_hash())),
                        prop = %crate::short_id(&hex::encode(prop_hash)),
                        "routing message via propagation node"
                    );
                    self.start_propagation_delivery(message, prop_hash, &mut results);
                    continue;
                }
                OutboundAction::Failed(message) => {
                    let msg_hash = Some(message.message_id());
                    let dest_hash = message.destination_hash();
                    if !self.try_auto_propagation_fallback(
                        message,
                        dest_hash,
                        "live delivery attempt budget exhausted",
                        &mut results,
                    ) {
                        self.push_failed_outbound_state(msg_hash, &mut results);
                    }
                    continue;
                }
                OutboundAction::Expired(message) => {
                    // An expired message is no longer timely enough to deposit
                    // for later pickup. Do not reinterpret expiry as an
                    // Offline Inbox retry.
                    self.push_failed_outbound_state(Some(message.message_id()), &mut results);
                    continue;
                }
            };
            // Router output is already finalized after any deferred recipient stamp.
            let mut message = message;

            let msg_hash = Some(message.message_id());
            let is_ephemeral = msg_hash
                .as_ref()
                .is_some_and(|hash| self.ephemeral_outbound.contains(hash));
            let dest_hex = hex::encode(dest_hash);
            self.apply_peer_lxmf_compression_support(&mut message, None, &dest_hex);

            if !is_opportunistic {
                let waiting_for_reusable =
                    matches!(direct_plan, Some(DirectDeliveryPlan::WaitForReusableLink));
                if !waiting_for_reusable
                    && self.direct_reusable_link_state(dest_hash) == DirectReusableLinkState::None
                    && self
                        .link_delivery
                        .as_ref()
                        .and_then(|ld| ld.backchannel_link_snapshot(dest_hash))
                        .is_some()
                    && self.lxmf_link_command_tx.is_some()
                {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    if let Some(ref mut ld) = self.link_delivery {
                        match ld.start_backchannel_delivery(message, dest_hash) {
                            Ok(_) => {
                                self.drain_core_backchannel_send_commands();
                                if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                                    results.push((hex::encode(hash), "reusing_backchannel"));
                                }
                                continue;
                            }
                            Err(err) => {
                                message = *err.message;
                                message.delivery_attempts =
                                    message.delivery_attempts.saturating_sub(1);
                                direct_plan = None;
                                tracing::debug!(
                                    dest = %crate::short_id(&dest_hex),
                                    reason = "backchannel_unavailable",
                                    "LXMF backchannel unavailable; falling back to outbound Direct link"
                                );
                            }
                        }
                    } else {
                        message.delivery_attempts = message.delivery_attempts.saturating_sub(1);
                        direct_plan = None;
                        tracing::debug!(
                            dest = %crate::short_id(&dest_hex),
                            "LXMF backchannel unavailable; falling back to outbound Direct link"
                        );
                    }
                }

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let route = self
                    .direct_route_entry(dest_hash, now)
                    .map(|entry| direct_route_snapshot_from_entry(dest_hash, entry));
                let identity_known = self.known_identities.contains_key(&dest_hex);
                let had_expired_snapshot = identity_known
                    && route.is_none()
                    && self.route_entries.contains_key(&dest_hash);
                let router_owned = direct_plan.is_some();
                let plan = direct_plan.unwrap_or_else(|| {
                    plan_direct_delivery(
                        &mut message,
                        DirectDeliveryPlanInput {
                            identity_known,
                            route,
                            reusable_link: self.direct_or_identifying_link_state(dest_hash),
                        },
                        now,
                    )
                });

                match plan {
                    DirectDeliveryPlan::RequestPath { drop_existing } => {
                        let drop_existing = drop_existing || had_expired_snapshot;
                        self.request_path_recovery(dest_hash, None);
                        tracing::warn!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            drop_existing,
                            identity_known,
                            expired_snapshot = had_expired_snapshot,
                            "outbound LXMF: Direct delivery waiting for path"
                        );
                        if !router_owned {
                            self.queue_router_message(message, "direct path wait");
                        }
                        if let Some(hash) = msg_hash.filter(|_| identity_known && !is_ephemeral) {
                            results.push((hex::encode(hash), "routing"));
                        }
                    }
                    DirectDeliveryPlan::DeferTerminalFailure => {
                        tracing::warn!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            max_attempts = MAX_DELIVERY_ATTEMPTS,
                            "outbound LXMF: Direct delivery attempt budget reached; deferring terminal failure"
                        );
                        if !router_owned {
                            self.queue_router_message(message, "direct terminal deferral");
                        }
                    }
                    DirectDeliveryPlan::WaitForReusableLink => {
                        tracing::debug!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            "outbound LXMF: Direct delivery waiting for reusable Link"
                        );
                        if !router_owned {
                            self.queue_router_message(message, "reusable Link wait");
                        }
                        if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                            results.push((hex::encode(hash), "sending_via_link"));
                        }
                    }
                    DirectDeliveryPlan::UseReusableLink => {
                        let hops = self.delivery_link_hops(dest_hash);
                        self.start_direct_link_delivery_with_results(
                            DirectLinkDeliveryStartInput {
                                message,
                                dest_hash,
                                hops,
                                now,
                                msg_hash,
                                is_ephemeral,
                                router_owned,
                            },
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::StartNewLink { hops } => {
                        self.start_direct_link_delivery_with_results(
                            DirectLinkDeliveryStartInput {
                                message,
                                dest_hash,
                                hops,
                                now,
                                msg_hash,
                                is_ephemeral,
                                router_owned,
                            },
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::Fail => {
                        if let Some(hash) = msg_hash {
                            if self.ephemeral_outbound.remove(&hash) {
                                continue;
                            }
                            results.push((hex::encode(hash), "failed"));
                        }
                    }
                }
                continue;
            }

            // Opportunistic path escalation (LXMRouter.py:2566-2592): try
            // pathless up to MAX_PATHLESS_TRIES, then request a path, then
            // drop+rediscover once, before resuming best-effort sends.
            {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let has_path = self.direct_route_entry(dest_hash, now).is_some();
                let needs_discovery = (message.delivery_attempts >= MAX_PATHLESS_TRIES
                    && !has_path)
                    || (message.delivery_attempts == MAX_PATHLESS_TRIES + 1 && has_path);
                if needs_discovery {
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
                    self.request_packet_path_recovery(
                        dest_hash,
                        msg_hash.and_then(|hash| self.last_failed_packet(hash)),
                    );
                    self.queue_router_message(message, "opportunistic path escalation");
                    if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                        results.push((hex::encode(hash), "routing"));
                    }
                    continue;
                }
            }

            tracing::info!(
                dest = %crate::short_id(&dest_hex),
                known = self.known_identities.contains_key(&dest_hex),
                total_known = self.known_identities.len(),
                "outbound: identity lookup for destination"
            );

            let destination_public_key = self.known_identities.get(&dest_hex).copied();
            let mut missing_identity = false;
            let payload = match message.pack_opportunistic_encrypted(|plaintext| {
                self.encrypt_for_destination(&dest_hex, plaintext)
                    .ok_or_else(|| {
                        missing_identity = true;
                        lxmf_core::message_api::MessageError::PackFailed(format!(
                            "no identity key for destination {dest_hex}"
                        ))
                    })
            }) {
                Ok(ct) => {
                    tracing::info!(
                        dest = %crate::short_id(&dest_hex),
                        encrypted_len = ct.len(),
                        "outbound LXMF: encrypted and sending opportunistic payload"
                    );
                    ct
                }
                Err(_) if missing_identity => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs_f64();
                    message.delivery_attempts += 1;
                    message.last_delivery_attempt = now;
                    self.request_path_recovery(dest_hash, None);

                    message.next_delivery_attempt = now + PATH_REQUEST_WAIT as f64;
                    tracing::warn!(
                        dest = %crate::short_id(&dest_hex),
                        attempts = message.delivery_attempts,
                        reason = "identity_unknown",
                        "outbound LXMF: destination key unknown, re-queuing"
                    );
                    self.queue_router_message(message, "unknown identity retry");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(
                        dest = %crate::short_id(&dest_hex),
                        reason = "pack_failed",
                        "outbound LXMF: failed to pack opportunistic message"
                    );
                    self.push_failed_outbound_state(msg_hash, &mut results);
                    continue;
                }
            };
            let Some(destination_public_key) = destination_public_key else {
                tracing::error!(
                    dest = %crate::short_id(&dest_hex),
                    reason = "identity_invariant",
                    "outbound LXMF encryption succeeded without a retained destination identity"
                );
                continue;
            };

            let flags = rns_wire::flags::PacketFlags {
                header_type: rns_wire::flags::HeaderType::Header1,
                context_flag: false,
                transport_type: rns_wire::flags::TransportType::Broadcast,
                destination_type: rns_wire::flags::DestinationType::Single,
                packet_type: rns_wire::flags::PacketType::Data,
            };
            let header = rns_wire::header::PacketHeader {
                flags,
                hops: 0,
                transport_id: None,
                destination_hash: dest_hash,
                context: rns_wire::context::PacketContext::None,
            };
            let mut raw = header.pack();
            raw.extend_from_slice(&payload);

            if raw.len() > rns_wire::constants::MTU {
                tracing::info!(
                    dest = %crate::short_id(&dest_hex),
                    packet_len = raw.len(),
                    mtu = rns_wire::constants::MTU,
                    "outbound LXMF packet exceeds MTU — routing to link delivery"
                );

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let route = self
                    .direct_route_entry(dest_hash, now)
                    .map(|entry| direct_route_snapshot_from_entry(dest_hash, entry));
                let identity_known = self.known_identities.contains_key(&dest_hex);
                let had_expired_snapshot = identity_known
                    && route.is_none()
                    && self.route_entries.contains_key(&dest_hash);
                let plan = plan_direct_delivery(
                    &mut message,
                    DirectDeliveryPlanInput {
                        identity_known,
                        route,
                        reusable_link: self.direct_or_identifying_link_state(dest_hash),
                    },
                    now,
                );

                match plan {
                    DirectDeliveryPlan::RequestPath { drop_existing } => {
                        let drop_existing = drop_existing || had_expired_snapshot;
                        self.request_path_recovery(dest_hash, None);
                        tracing::warn!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            drop_existing,
                            identity_known,
                            expired_snapshot = had_expired_snapshot,
                            "outbound LXMF: oversized Link delivery waiting for path"
                        );
                        self.queue_router_message(message, "oversized Link path wait");
                        if let Some(hash) = msg_hash.filter(|_| identity_known && !is_ephemeral) {
                            results.push((hex::encode(hash), "routing"));
                        }
                    }
                    DirectDeliveryPlan::DeferTerminalFailure => {
                        tracing::warn!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            max_attempts = MAX_DELIVERY_ATTEMPTS,
                            "outbound LXMF: oversized Link delivery attempt budget reached; deferring terminal failure"
                        );
                        self.queue_router_message(message, "oversized Link terminal deferral");
                    }
                    DirectDeliveryPlan::WaitForReusableLink => {
                        tracing::debug!(
                            dest = %crate::short_id(&dest_hex),
                            attempts = message.delivery_attempts,
                            "outbound LXMF: oversized Link delivery waiting for reusable Link"
                        );
                        self.queue_router_message(message, "oversized reusable Link wait");
                        if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                            results.push((hex::encode(hash), "sending_via_link"));
                        }
                    }
                    DirectDeliveryPlan::UseReusableLink => {
                        let hops = self.delivery_link_hops(dest_hash);
                        self.start_direct_link_delivery_with_results(
                            DirectLinkDeliveryStartInput {
                                message,
                                dest_hash,
                                hops,
                                now,
                                msg_hash,
                                is_ephemeral,
                                router_owned: false,
                            },
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::StartNewLink { hops } => {
                        self.start_direct_link_delivery_with_results(
                            DirectLinkDeliveryStartInput {
                                message,
                                dest_hash,
                                hops,
                                now,
                                msg_hash,
                                is_ephemeral,
                                router_owned: false,
                            },
                            &mut results,
                        );
                    }
                    DirectDeliveryPlan::Fail => {
                        self.push_failed_outbound_state(msg_hash, &mut results);
                    }
                }
                continue;
            }

            let Some(_) = self.router.transport_tx else {
                tracing::error!(dest = %crate::short_id(&dest_hex), reason = "transport_unavailable", "transport unavailable; message dropped");
                self.push_failed_outbound_state(msg_hash, &mut results);
                continue;
            };

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            message.delivery_attempts += 1;
            message.last_delivery_attempt = now;
            message.next_delivery_attempt = now + self.packet_retry_window(dest_hash).as_secs_f64();

            let dispatch_result =
                self.dispatch_opportunistic_packet(&message, raw, destination_public_key);
            match dispatch_result {
                Ok(packet_hash) => {
                    if let Some(hash) = msg_hash {
                        let msg_id_hex = hex::encode(hash);
                        self.opportunistic_in_flight.insert(
                            hash,
                            PendingOpportunisticDelivery {
                                retry_at: message.next_delivery_attempt,
                                packet_hash,
                                message,
                            },
                        );
                        if !is_ephemeral {
                            results.push((msg_id_hex, "sent"));
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        dest = %crate::short_id(&dest_hex),
                        reason = "backpressure",
                        %error,
                        "opportunistic dispatch deferred"
                    );
                    message.delivery_attempts = message.delivery_attempts.saturating_sub(1);
                    message.next_delivery_attempt = now + 0.5;
                    self.queue_router_message(message, "opportunistic dispatch retry");
                    if let Some(hash) = msg_hash.filter(|_| !is_ephemeral) {
                        results.push((hex::encode(hash), "routing"));
                    }
                }
            }
        }
        results
    }
}

fn attachment_storage_name(file_name: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let prefix = format!("{ts}-{}_", uuid::Uuid::new_v4().simple());
    let budget = 255 - prefix.len();
    let mut name: String = file_name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        .take(240)
        .collect();
    if name.len() > budget || file_name.chars().take(241).count() > 240 {
        // Preserve an ordinary extension even when the long stem was clipped.
        let extension = file_name
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .filter(|ext| {
                !ext.is_empty()
                    && ext.len() <= 16
                    && ext.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
        let suffix = extension.map(|ext| format!(".{ext}")).unwrap_or_default();
        let stem = name.strip_suffix(&suffix).unwrap_or(&name);
        let mut end = stem.len().min(budget - suffix.len());
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        name = format!("{}{suffix}", &stem[..end]);
    }
    let name = name.trim_end_matches([' ', '.']);
    let name = if name.is_empty() || name.chars().all(|c| c == '.' || c == ' ') {
        "file"
    } else {
        name
    };
    format!("{prefix}{name}")
}

/// Validate an existing storage key without renaming or truncating it.
/// Creation budgets the complete filename separately; legacy keys may exceed
/// that new byte budget on filesystems with different Unicode length rules.
/// Unsafe keys must not alias a different attachment during read or deletion.
pub fn sanitize_stored_file_name(raw: &str) -> Option<String> {
    if raw.is_empty()
        || raw.len() > 1024
        || !raw
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        || raw.chars().all(|c| c == '.' || c == ' ')
    {
        return None;
    }
    Some(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use lxmf_core::constants::DELIVERY_RETRY_WAIT;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_LXMF_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn bridge_packet_receipt(marker: u8) -> rns_runtime::link_manager::LinkPayloadSendReceipt {
        rns_runtime::link_manager::LinkPayloadSendReceipt::Packet(
            rns_runtime::link_manager::LinkPacketSendReceipt {
                link_id: [marker; 16],
                packet_hash: [marker; 32],
            },
        )
    }

    #[tokio::test]
    async fn backchannel_receipt_bridge_closes_before_late_runtime_publication() {
        let (runtime_tx, runtime_rx) = oneshot::channel();
        let (owner_tx, owner_rx) = oneshot::channel();
        let (abandon_tx, mut abandon_rx) = mpsc::channel(1);
        drop(owner_rx);
        forward_backchannel_receipt(runtime_rx, owner_tx, abandon_tx).await;

        // The runtime retains this rejected receipt and therefore its exact
        // cancellation responsibility. No packet key is lost into the bridge.
        let rejected = runtime_tx.send(Ok(bridge_packet_receipt(1))).unwrap_err();
        assert!(
            matches!(rejected, Ok(rns_runtime::link_manager::LinkPayloadSendReceipt::Packet(receipt))
            if receipt.link_id == [1; 16] && receipt.packet_hash == [1; 32])
        );
        assert!(abandon_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn backchannel_receipt_bridge_retains_published_receipt_when_abandon_queue_is_full() {
        use rns_runtime::link_manager::{LinkPayloadSendReceipt, LinkResourceSendReceipt};

        for receipt in [
            bridge_packet_receipt(2),
            LinkPayloadSendReceipt::Resource(LinkResourceSendReceipt {
                link_id: [3; 16],
                resource_hash: [3; 32],
            }),
        ] {
            let expected = backchannel_receipt_from_runtime(receipt.clone());
            let (runtime_tx, runtime_rx) = oneshot::channel();
            let (owner_tx, owner_rx) = oneshot::channel();
            let (abandon_tx, mut abandon_rx) = mpsc::channel(1);
            abandon_tx
                .send(BackchannelSendReceipt::Packet {
                    link_id: [9; 16],
                    packet_hash: [9; 32],
                })
                .await
                .unwrap();
            runtime_tx.send(Ok(receipt)).unwrap();
            drop(owner_rx);
            let mut bridge = tokio::spawn(forward_backchannel_receipt(
                runtime_rx, owner_tx, abandon_tx,
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut bridge)
                    .await
                    .is_err(),
                "a full cleanup queue must retain the exact receipt, not discard it"
            );
            assert!(
                matches!(abandon_rx.recv().await, Some(BackchannelSendReceipt::Packet { link_id, packet_hash })
                if link_id == [9; 16] && packet_hash == [9; 32])
            );
            tokio::time::timeout(Duration::from_secs(1), bridge)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(abandon_rx.recv().await.unwrap(), expected);
            assert!(abandon_rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn backchannel_receipt_bridge_timeout_is_finite_and_rejects_late_publication() {
        let (runtime_tx, runtime_rx) = oneshot::channel();
        let (owner_tx, owner_rx) = oneshot::channel();
        let (abandon_tx, mut abandon_rx) = mpsc::channel(1);
        tokio::time::timeout(
            Duration::from_secs(12),
            forward_backchannel_receipt(runtime_rx, owner_tx, abandon_tx),
        )
        .await
        .unwrap();
        assert!(matches!(
            owner_rx.await.unwrap(),
            Err(BackchannelSendError::TransportUnavailable)
        ));
        assert!(runtime_tx.send(Ok(bridge_packet_receipt(4))).is_err());
        assert!(abandon_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn backchannel_receipt_bridge_delivers_live_owner_without_abandonment() {
        let (runtime_tx, runtime_rx) = oneshot::channel();
        let (owner_tx, owner_rx) = oneshot::channel();
        let (abandon_tx, mut abandon_rx) = mpsc::channel(1);
        runtime_tx.send(Ok(bridge_packet_receipt(5))).unwrap();
        forward_backchannel_receipt(runtime_rx, owner_tx, abandon_tx).await;
        assert!(
            matches!(owner_rx.await.unwrap(), Ok(BackchannelSendReceipt::Packet { link_id, packet_hash })
            if link_id == [5; 16] && packet_hash == [5; 32])
        );
        assert!(abandon_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn backchannel_receipt_bridge_capacity_covers_blocked_cleanup_until_handoff() {
        use rns_runtime::link_manager::LinkManagerCommand;

        let mut manager = test_manager();
        assert_eq!(manager.backchannel_bridge_slots.available_permits(), 256);
        manager.backchannel_bridge_slots = Arc::new(tokio::sync::Semaphore::new(1));
        let (runtime_tx, mut runtime_rx) = mpsc::channel(2);
        let (command_tx, command_rx) = mpsc::channel(2);
        manager.lxmf_link_command_tx = Some(runtime_tx);
        manager.lxmf_backchannel_command_rx = Some(command_rx);
        for _ in 0..256 {
            manager
                .backchannel_abandon_tx
                .try_send(BackchannelSendReceipt::Packet {
                    link_id: [9; 16],
                    packet_hash: [9; 32],
                })
                .unwrap();
        }
        let (owner_tx, owner_rx) = oneshot::channel();
        command_tx
            .try_send(BackchannelSendCommand {
                link_id: [6; 16],
                payload: vec![6],
                auto_compress: false,
                result_tx: owner_tx,
            })
            .unwrap();
        manager.drain_core_backchannel_send_commands();
        assert_eq!(manager.backchannel_bridge_slots.available_permits(), 0);
        let LinkManagerCommand::SendLinkPayload {
            result_tx: Some(result_tx),
            ..
        } = runtime_rx.try_recv().unwrap()
        else {
            panic!("expected one runtime payload command");
        };
        result_tx.send(Ok(bridge_packet_receipt(6))).unwrap();
        drop(owner_rx);
        tokio::task::yield_now().await;

        // The first send no longer has a message receiver, but its exact
        // abandoned receipt still owns the helper slot while cleanup is full.
        let (next_tx, next_rx) = oneshot::channel();
        command_tx
            .try_send(BackchannelSendCommand {
                link_id: [7; 16],
                payload: vec![7],
                auto_compress: false,
                result_tx: next_tx,
            })
            .unwrap();
        manager.drain_core_backchannel_send_commands();
        assert!(matches!(
            next_rx.await.unwrap(),
            Err(BackchannelSendError::TransportUnavailable)
        ));
        assert!(runtime_rx.try_recv().is_err());
        assert_eq!(manager.backchannel_bridge_slots.available_permits(), 0);

        manager.backchannel_abandon_rx.try_recv().unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while manager.backchannel_bridge_slots.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut old = 0;
        let mut exact = 0;
        while let Ok(receipt) = manager.backchannel_abandon_rx.try_recv() {
            match receipt {
                BackchannelSendReceipt::Packet {
                    link_id,
                    packet_hash,
                } if link_id == [9; 16] && packet_hash == [9; 32] => old += 1,
                BackchannelSendReceipt::Packet {
                    link_id,
                    packet_hash,
                } if link_id == [6; 16] && packet_hash == [6; 32] => exact += 1,
                _ => panic!("unexpected cleanup owner"),
            }
        }
        assert_eq!((old, exact), (255, 1));
    }

    #[cfg(feature = "lxst-voice")]
    fn valid_ogg_opus_fixture() -> Vec<u8> {
        use ogg::writing::{PacketWriteEndInfo, PacketWriter};

        let serial = 0x5253_4155;
        let mut writer = PacketWriter::new(Vec::new());
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]);
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&24_000u32.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        writer
            .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&0u32.to_le_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        writer
            .write_packet(
                vec![0xf8, 0xff, 0xfe],
                serial,
                PacketWriteEndInfo::EndStream,
                960,
            )
            .unwrap();
        writer.into_inner()
    }

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        db::init_schema(&pool).unwrap();
        pool
    }

    impl LxmfManager {
        pub(super) fn create_final_message(
            &mut self,
            dest: &str,
            content: &str,
            title: &str,
            method: DeliveryMethod,
        ) -> Option<LxMessage> {
            let signed = self.create_message(dest, content, title, method)?;
            let stamp = signed.generate_recipient_stamp().ok()?;
            signed.finalize(stamp).ok()
        }
        pub(super) fn create_final_message_with_custom_fields(
            &mut self,
            dest: &str,
            content: &str,
            title: &str,
            method: DeliveryMethod,
            fields: &[(u8, Vec<u8>)],
        ) -> Option<LxMessage> {
            let signed =
                self.create_message_with_custom_fields(dest, content, title, method, fields)?;
            let stamp = signed.generate_recipient_stamp().ok()?;
            signed.finalize(stamp).ok()
        }
    }

    pub(super) fn test_manager() -> LxmfManager {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-policy-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        LxmfManager::load_or_create(&tmp, None, None).unwrap()
    }

    fn ready_relay_policy(mgr: &mut LxmfManager, node: [u8; 16], cost: u8, limit: usize) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let announce = lxmf_core::handlers::PropagationNodeAnnounceData::new(
            true,
            limit as u64,
            limit as u64,
            cost,
            0,
            0,
        );
        assert!(mgr.update_lxmf_announce_app_data(
            node,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&lxmf_core::handlers::get_propagation_node_app_data(
                &announce
            ))
        ));
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: node,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 600.,
            interface: "relay fixture".into(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);
    }
    async fn finish_pending_propagation(
        mgr: &mut LxmfManager,
        mut states: Vec<(String, &'static str)>,
    ) -> Vec<(String, &'static str)> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while states.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
                for msg in &mut mgr.router.pending_outbound {
                    msg.next_delivery_attempt = 0.;
                }
                states.extend(mgr.tick());
            }
            states
        })
        .await
        .expect("bounded envelope worker should settle")
    }

    fn ready_auto_propagation_fallback(
        mgr: &mut LxmfManager,
        dest: [u8; 16],
        node: [u8; 16],
    ) -> tokio::sync::mpsc::Receiver<TransportMessage> {
        let remote = Identity::new();
        let relay = Identity::new();
        mgr.known_identities
            .insert(hex::encode(dest), remote.get_public_key());
        mgr.known_identities
            .insert(hex::encode(node), relay.get_public_key());
        ready_relay_policy(mgr, node, 0, 256);
        mgr.router.set_stamp_cost(node, 0);
        mgr.propagation_transfer_limits_kb.insert(node, 256);
        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.configured_propagation_node = Some(node);
        mgr.client_propagation_enabled = true;
        let (tx, rx) = tokio::sync::mpsc::channel::<TransportMessage>(64);
        mgr.router.set_transport(tx);
        rx
    }

    #[test]
    fn lrgp_fields_are_native_lxmf_values_not_binary_wrappers() {
        let envelope = lrgp::protocol::pack_envelope(
            "ttt",
            1,
            lrgp::protocol::CMD_CHALLENGE,
            "0123456789abcdef",
            None,
            None,
        )
        .unwrap();
        let fields = lrgp::protocol::pack_lxmf_fields(&envelope).unwrap();
        let mut message = lxmf_core::message::LxMessageDraft::new(
            [0x11; 16],
            [0x22; 16],
            "",
            "[LRGP TTT] Sent a challenge!",
            DeliveryMethod::Opportunistic,
        )
        .unwrap();

        apply_lrgp_fields_to_message(&mut message, &fields).unwrap();
        assert!(matches!(
            message.get_field(lrgp::protocol::FIELD_CUSTOM_TYPE),
            Some(lxmf_core::message::FieldValue::MessagePack(_))
        ));
        assert!(matches!(
            message.get_field(lrgp::protocol::FIELD_CUSTOM_META),
            Some(lxmf_core::message::FieldValue::MessagePack(_))
        ));

        // Decode the complete LXMF payload exactly as a Python client would:
        // the allocated LRGP fields must be a native string and map, never
        // binary values containing another MessagePack document.
        let packed = message.pack_payload().unwrap();
        let decoded = rmpv::decode::read_value(&mut packed.as_slice()).unwrap();
        let field_map = decoded
            .as_array()
            .and_then(|values| values.get(3))
            .and_then(rmpv::Value::as_map)
            .unwrap();
        let field = |wanted: u8| {
            field_map.iter().find_map(|(key, value)| {
                let actual = key
                    .as_u64()
                    .map(|value| value as u8)
                    .or_else(|| key.as_i64().map(|value| value as u8));
                (actual == Some(wanted)).then_some(value)
            })
        };
        assert_eq!(
            field(lrgp::protocol::FIELD_CUSTOM_TYPE).and_then(rmpv::Value::as_str),
            Some(lrgp::protocol::PROTOCOL_TYPE)
        );
        assert!(
            field(lrgp::protocol::FIELD_CUSTOM_META)
                .and_then(rmpv::Value::as_map)
                .is_some()
        );
    }

    fn delivery_app_data_with_features(features: &[u8]) -> Vec<u8> {
        let values = features
            .iter()
            .map(|feature| rmpv::Value::from(*feature as u64))
            .collect();
        let arr = rmpv::Value::Array(vec![
            rmpv::Value::Binary(b"Peer".to_vec()),
            rmpv::Value::Nil,
            rmpv::Value::Array(values),
        ]);
        encode_msgpack_value(&arr)
    }

    fn next_outbound(rx: &mut mpsc::Receiver<TransportMessage>) -> Vec<u8> {
        while let Ok(message) = rx.try_recv() {
            match message {
                TransportMessage::Outbound(request) => return request.raw.to_vec(),
                TransportMessage::SendLinkRequest { request, result_tx } => {
                    result_tx.send(Ok(())).unwrap();
                    return request.raw.to_vec();
                }
                _ => {}
            }
        }
        panic!("expected outbound transport message");
    }

    fn accept_initiator_endpoint_binding(
        rx: &mut mpsc::Receiver<TransportMessage>,
        expected_link_id: [u8; 16],
        expected_interface_id: rns_transport::messages::InterfaceId,
    ) {
        let message = rx.try_recv().expect("expected endpoint binding");
        let TransportMessage::BindLinkEndpoint {
            binding,
            lifecycle_tx,
            result_tx,
        } = message
        else {
            panic!("expected endpoint binding, got {message:?}");
        };
        assert_eq!(binding.link_id, expected_link_id);
        assert_eq!(binding.interface_id, expected_interface_id);
        assert_eq!(
            binding.role,
            rns_transport::messages::LinkEndpointRole::Initiator
        );
        result_tx
            .send(rns_transport::messages::LinkEndpointBindResult::Bound)
            .unwrap();
        std::mem::forget(lifecycle_tx);
    }

    fn next_initiator_endpoint_outbound(rx: &mut mpsc::Receiver<TransportMessage>) -> Vec<u8> {
        while let Ok(message) = rx.try_recv() {
            match message {
                TransportMessage::SendLinkEndpoint {
                    role,
                    request,
                    result_tx,
                    ..
                }
                | TransportMessage::SendLinkEndpointAndUnbind {
                    role,
                    request,
                    result_tx,
                    ..
                } => {
                    assert_eq!(role, rns_transport::messages::LinkEndpointRole::Initiator);
                    result_tx
                        .send(rns_transport::messages::LinkEndpointSendResult::Sent)
                        .unwrap();
                    return request.raw.to_vec();
                }
                _ => {}
            }
        }
        panic!("expected initiator endpoint transport message");
    }

    #[test]
    fn received_ratchet_loader_rejects_arbitrary_filename_stems() {
        assert_eq!(
            received_ratchet_hash_from_path(Path::new("0123456789abcdef0123456789abcdef.ratchet")),
            Some("0123456789abcdef0123456789abcdef")
        );
        for path in [
            "human-label.ratchet",
            "0123456789abcdef.ratchet",
            "0123456789abcdef0123456789abcdeg.ratchet",
        ] {
            assert_eq!(received_ratchet_hash_from_path(Path::new(path)), None);
        }
    }

    #[test]
    fn invalid_manual_propagation_node_is_not_reported_as_restored() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        db::save_identity(&pool, &identity_id, &mgr.lxmf_hash, "Me", "Me");

        assert!(!mgr.set_propagation_node(
            Some("human-label-that-is-not-a-destination"),
            &pool,
            &identity_id,
        ));
        assert_eq!(mgr.configured_propagation_node, None);
        assert_eq!(
            db::get_identity(&pool, &identity_id).and_then(|identity| identity
                .get("propagation_node")
                .and_then(|node| node.as_str())
                .map(str::to_string)),
            Some(String::new())
        );
    }

    /// T1-7: one shared sanitizer — names that save (spaces included) must
    /// list, download, and delete; traversal input never escapes files_dir.
    #[test]
    fn attachment_with_spaces_round_trips_save_get_delete() {
        let mgr = test_manager();

        let stored = mgr.save_attachment("my report final.pdf", b"data").unwrap();
        assert!(stored.ends_with("_my report final.pdf"));

        let listed = mgr.list_received_files();
        assert!(
            listed
                .iter()
                .any(|f| f.get("stored_name").and_then(|v| v.as_str()) == Some(stored.as_str())),
            "saved file must appear in the listing"
        );

        let path = mgr
            .get_received_file(&stored)
            .expect("space-bearing stored name must resolve");
        assert_eq!(std::fs::read(&path).unwrap(), b"data");

        std::fs::remove_file(&path).unwrap();
        assert!(mgr.get_received_file(&stored).is_none());
    }

    #[test]
    fn attachment_storage_names_round_trip_with_long_and_unicode_names() {
        let mgr = test_manager();
        for name in [
            format!("{}.pdf", "a".repeat(189)),
            format!("{}.pdf", "a".repeat(190)),
            format!("{}.pdf", "a".repeat(196)),
            format!("{}.pdf", "a".repeat(400)),
            format!("{}.png", "界".repeat(200)),
            format!("{}.txt", "é".repeat(200)),
        ] {
            for adopt in [false, true] {
                let stored = if adopt {
                    let source = mgr.data_dir.join("name-test-stage");
                    std::fs::write(&source, b"original bytes").unwrap();
                    mgr.adopt_staged_attachment(&name, &source).unwrap()
                } else {
                    mgr.save_attachment(&name, b"original bytes").unwrap()
                };
                assert!(stored.len() <= 255);
                assert_eq!(Path::new(&stored).extension(), Path::new(&name).extension());
                let path = mgr
                    .get_received_file(&stored)
                    .expect("saved name must resolve");
                assert_eq!(std::fs::read(&path).unwrap(), b"original bytes");
                assert!(
                    mgr.list_received_files()
                        .iter()
                        .any(|entry| entry["stored_name"] == stored)
                );
                let key = sanitize_stored_file_name(&stored).expect("cleanup must keep exact key");
                assert_eq!(key, stored);
                std::fs::remove_file(mgr.files_dir().join(key)).unwrap();
                assert!(mgr.get_received_file(&stored).is_none());
            }
        }
    }

    #[test]
    fn attachment_storage_resolves_existing_long_keys_without_renaming() {
        let mgr = test_manager();
        let stored = format!("1700000000000-{}_{}.pdf", "a".repeat(32), "b".repeat(196));
        assert_eq!(stored.len(), 247);
        let path = mgr.files_dir().join(&stored);
        std::fs::write(&path, b"legacy bytes").unwrap();
        assert_eq!(
            mgr.get_received_file(&stored),
            Some(path.canonicalize().unwrap())
        );
        assert!(mgr.get_received_file(&format!("/{stored}")).is_none());
        assert_eq!(sanitize_stored_file_name(&stored), Some(stored.clone()));
        std::fs::remove_file(
            mgr.files_dir()
                .join(sanitize_stored_file_name(&stored).unwrap()),
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn received_attachment_resolution_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let mgr = test_manager();
        let outside = mgr.data_dir.join("outside-private-file");
        std::fs::write(&outside, b"secret").unwrap();
        let link = mgr.files_dir().join("escaped.bin");
        symlink(&outside, &link).unwrap();

        assert!(mgr.get_received_file("escaped.bin").is_none());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(outside).unwrap();
    }

    #[test]
    fn stored_file_name_sanitizer_blocks_traversal() {
        assert_eq!(
            sanitize_stored_file_name("../../etc/passwd").as_deref(),
            None,
            "unsafe keys must not be rewritten into another attachment's key"
        );
        assert_eq!(sanitize_stored_file_name(".."), None);
        assert_eq!(sanitize_stored_file_name("."), None);
        assert_eq!(sanitize_stored_file_name(". ."), None);
        assert_eq!(sanitize_stored_file_name("///"), None);
        assert_eq!(sanitize_stored_file_name(""), None);
        assert_eq!(
            sanitize_stored_file_name("a/b\\c").as_deref(),
            None,
            "separators are rejected, not stripped"
        );
        assert_eq!(
            sanitize_stored_file_name("my file.txt").as_deref(),
            Some("my file.txt")
        );

        let mgr = test_manager();
        assert!(mgr.get_received_file("../../etc/passwd").is_none());
        assert!(mgr.get_received_file("..").is_none());
    }

    #[test]
    fn ratspeak_announce_extension_is_optional_and_parseable() {
        let legacy = get_ratspeak_announce_app_data(Some("Alice"), Some(8), Some("Away"), false);
        assert!(ratspeak_capability_services_from_app_data(&legacy).is_empty());
        assert_eq!(ratspeak_status_from_app_data(&legacy), None);
        let (name, cost) = lxmf_core::handlers::parse_announce_app_data(&legacy).unwrap();
        assert_eq!(name.as_deref(), Some("Alice"));
        assert_eq!(cost, Some(8));

        let extended = get_ratspeak_announce_app_data(Some("Alice"), Some(8), Some("Away"), true);
        let services = ratspeak_capability_services_from_app_data(&extended);
        assert!(services.contains(&db::PEER_SERVICE_RATSPEAK_CLIENT));
        assert!(services.contains(&db::PEER_SERVICE_RATSPEAK_GAMES));
        assert!(services.contains(&db::PEER_SERVICE_RATSPEAK_CHAT));
        assert_eq!(
            ratspeak_status_from_app_data(&extended).as_deref(),
            Some("Away")
        );
        let (name, cost) = lxmf_core::handlers::parse_announce_app_data(&extended).unwrap();
        assert_eq!(name.as_deref(), Some("Alice"));
        assert_eq!(cost, Some(8));
        assert!(lxmf_core::handlers::compression_support_from_app_data(
            &extended
        ));
    }

    #[test]
    fn ratspeak_announce_status_distinguishes_absent_from_cleared() {
        let cleared = get_ratspeak_announce_app_data(Some("Alice"), Some(8), Some(""), true);
        assert_eq!(ratspeak_status_from_app_data(&cleared).as_deref(), Some(""));
        let legacy = lxmf_core::handlers::get_announce_app_data(Some("Alice"), Some(8));
        assert_eq!(ratspeak_status_from_app_data(&legacy), None);
    }

    #[test]
    fn delivery_announce_packet_stays_under_reticulum_mtu_with_max_display_name() {
        let mut mgr = test_manager();
        mgr.display_name = "a".repeat(crate::helpers::ANNOUNCED_DISPLAY_NAME_MAX_BYTES);
        mgr.status = "b".repeat(crate::helpers::ANNOUNCED_STATUS_MAX_BYTES);
        mgr.announce_ratspeak_usage = true;

        let raw = mgr.create_announce_packet().expect("announce packet");

        assert!(raw.len() <= rns_wire::constants::MTU);
    }

    #[test]
    fn coordinated_delivery_announce_preserves_typed_coalescing() {
        let mut mgr = test_manager();
        mgr.build_delivery_announce_packet_at_typed(DeliveryAnnounceKind::Broadcast, 100, 1.0)
            .expect("first announce");
        assert!(matches!(
            mgr.build_delivery_announce_packet_at_typed(DeliveryAnnounceKind::Broadcast, 100, 1.5,),
            Err(CoordinatedDeliveryAnnounceError::Coalesced)
        ));
    }

    #[test]
    fn propagation_announce_never_carries_the_delivery_ratchet() {
        let mut mgr = test_manager();
        let raw = mgr
            .create_propagation_announce_packet()
            .expect("propagation announce");
        let (header, offset) =
            rns_wire::header::PacketHeader::unpack(&raw).expect("propagation header");
        let announce =
            rns_identity::announce::AnnounceData::unpack(&raw[offset..], header.flags.context_flag)
                .expect("propagation announce data");

        assert!(
            !header.flags.context_flag,
            "reusing the delivery ring here would make propagation unreachable to Python peers"
        );
        assert_eq!(announce.ratchet, None);
    }

    #[test]
    fn path_response_announce_uses_path_response_context() {
        let mut mgr = test_manager();

        let normal = mgr
            .build_delivery_announce_packet_at(DeliveryAnnounceKind::Broadcast, 100, 1.0)
            .expect("announce packet");
        let (normal_hdr, _) =
            rns_wire::header::PacketHeader::unpack(&normal).expect("unpack normal announce");
        assert_eq!(normal_hdr.context, rns_wire::context::PacketContext::None);
        assert_eq!(
            normal_hdr.flags.packet_type,
            rns_wire::flags::PacketType::Announce
        );

        let response = mgr
            .build_delivery_announce_packet_at(
                DeliveryAnnounceKind::PathResponse {
                    tag: Some(b"path-tag"),
                },
                101,
                2.0,
            )
            .expect("path-response announce packet");
        let (resp_hdr, _) =
            rns_wire::header::PacketHeader::unpack(&response).expect("unpack path response");
        // The distinguishing bit: PathResponse context, so this answers a path
        // request rather than being treated as a fresh broadcast announce.
        assert_eq!(
            resp_hdr.context,
            rns_wire::context::PacketContext::PathResponse
        );
        assert_eq!(
            resp_hdr.flags.packet_type,
            rns_wire::flags::PacketType::Announce
        );
        assert_eq!(resp_hdr.destination_hash, mgr.lxmf_dest_hash);
    }

    #[test]
    fn repeated_path_request_tag_reuses_exact_signed_announce() {
        let mut mgr = test_manager();
        let tag = b"same-path-request";
        let first = mgr
            .build_delivery_announce_packet_at(
                DeliveryAnnounceKind::PathResponse { tag: Some(tag) },
                100,
                1.0,
            )
            .expect("first path response");
        let committed_wire = mgr.delivery_ratchets.control().last_announce_wire();

        mgr.display_name = "changed after first response".to_string();
        let replay = mgr
            .build_delivery_announce_packet_at(
                DeliveryAnnounceKind::PathResponse { tag: Some(tag) },
                100,
                2.0,
            )
            .expect("cached path response");

        assert_eq!(
            replay, first,
            "dropping the request tag would create different signed bytes"
        );
        assert_eq!(
            mgr.delivery_ratchets.control().last_announce_wire(),
            committed_wire,
            "a cached response must not consume another persisted announce value"
        );
    }

    #[test]
    fn retained_delivery_key_decrypts_messages_after_rotation() {
        let mut mgr = test_manager();
        let initial_public = mgr
            .delivery_ratchets
            .ring()
            .current_public_key()
            .expect("initial durable ratchet");
        let remote = Identity::from_public_key(&mgr.identity.get_public_key()).unwrap();
        let ciphertext = remote
            .encrypt(b"in flight before rotation", Some(&initial_public))
            .unwrap();
        assert!(
            mgr.identity.decrypt(&ciphertext, None, false).is_err(),
            "the fixture must actually require the ratchet private key"
        );

        let rotate_at = mgr
            .delivery_ratchets
            .control()
            .last_rotation_wall()
            .unwrap()
            + mgr.delivery_ratchets.ring().ratchet_interval();
        mgr.build_delivery_announce_packet_at(DeliveryAnnounceKind::Broadcast, rotate_at, 1.0)
            .expect("rotating announce");

        assert_ne!(
            mgr.delivery_ratchets.ring().current_public_key(),
            Some(initial_public)
        );
        assert_eq!(
            mgr.decrypt_inbound(&ciphertext).as_deref(),
            Some(b"in flight before rotation".as_slice()),
            "trying only the newest key would break in-flight messages after rotation"
        );
    }

    #[test]
    fn outbound_encryption_prefers_a_live_peer_ratchet() {
        let mut mgr = test_manager();
        let peer = Identity::new();
        let destination = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&peer.hash));
        let destination_hex = hex::encode(destination);
        mgr.known_identities
            .insert(destination_hex.clone(), peer.get_public_key());
        let private = rns_crypto::x25519::X25519PrivateKey::generate();
        let public = private.public_key().to_bytes();
        mgr.received_ratchets
            .insert(destination_hex.clone(), ReceivedRatchet::new(public));

        let ciphertext = mgr
            .encrypt_for_destination(&destination_hex, b"prefer peer ratchet")
            .expect("encrypted message");
        assert!(peer.decrypt(&ciphertext, None, false).is_err());
        let private = private.to_bytes();
        assert_eq!(
            peer.decrypt(&ciphertext, Some(&[&private]), false)
                .unwrap()
                .as_slice(),
            b"prefer peer ratchet"
        );
    }

    #[test]
    fn checked_chat_schema_rejects_duplicates_without_falling_back_to_chat() {
        use lxmf_core::admission::SuppressionReason;
        let mut manager = test_manager();
        let destination = hex::encode([0x22; 16]);
        // The same normalized integer key uses two legal MessagePack encodings.
        let mut reaction = vec![0x83, 0x00, 0xc4, 0x20];
        reaction.extend_from_slice(&[0x11; 32]);
        reaction.extend_from_slice(&[0xcc, 0x00, 0xc4, 0x20]);
        reaction.extend_from_slice(&[0x22; 32]);
        reaction.extend_from_slice(&[0x01, 0xa1, b'x']);
        let message = manager
            .create_final_message_with_custom_fields(
                &destination,
                "fallback",
                "",
                DeliveryMethod::Propagated,
                &[(lxmf_core::constants::FIELD_REACTION, reaction)],
            )
            .unwrap();
        assert_eq!(
            decode_ratspeak_chat_extension_checked(&message),
            Err(SuppressionReason::Malformed)
        );
        assert_eq!(decode_ratspeak_chat_extension(&message), None);

        let payload = encode_msgpack_value(&rmpv::Value::Map(vec![
            ("kind".into(), "reply".into()),
            ("kind".into(), "reaction".into()),
            ("target".into(), "old-id".into()),
            ("emoji".into(), "x".into()),
        ]));
        let message = manager
            .create_final_message_with_custom_fields(
                &destination,
                "fallback",
                "",
                DeliveryMethod::Propagated,
                &[
                    (
                        lxmf_core::constants::FIELD_CUSTOM_TYPE,
                        RATSPEAK_CHAT_CUSTOM_TYPE_V1.to_vec(),
                    ),
                    (lxmf_core::constants::FIELD_CUSTOM_DATA, payload),
                ],
            )
            .unwrap();
        assert_eq!(
            decode_ratspeak_chat_extension_checked(&message),
            Err(SuppressionReason::Malformed)
        );
    }

    #[test]
    fn checked_chat_schema_skips_unknown_trees_and_bounds_legacy_json() {
        use lxmf_core::admission::SuppressionReason;
        let mut manager = test_manager();
        let destination = hex::encode([0x22; 16]);
        let mut reaction = vec![0x83, 0x00, 0xc4, 0x20];
        reaction.extend_from_slice(&[0x11; 32]);
        reaction.extend_from_slice(&[0x01, 0xa1, b'x', 0x02, 0xdc, 0xff, 0xff]);
        reaction.extend(std::iter::repeat_n(0xc0, 65535));
        let message = manager
            .create_final_message_with_custom_fields(
                &destination,
                "",
                "",
                DeliveryMethod::Propagated,
                &[(lxmf_core::constants::FIELD_REACTION, reaction)],
            )
            .unwrap();
        assert!(
            matches!(decode_ratspeak_chat_extension_checked(&message), Ok(Some(RatspeakChatExtension::Reaction { emoji, .. })) if emoji == "x")
        );

        let message = manager
            .create_final_message_with_custom_fields(
                &destination,
                &" ".repeat(4097),
                "",
                DeliveryMethod::Propagated,
                &[(
                    lxmf_core::constants::FIELD_CUSTOM_TYPE,
                    LEGACY_RATSPEAK_REACTION_TYPE.to_vec(),
                )],
            )
            .unwrap();
        assert_eq!(
            decode_ratspeak_chat_extension_checked(&message),
            Err(SuppressionReason::TooLarge)
        );
        let message = manager
            .create_final_message_with_custom_fields(
                &destination,
                r#"{"message_id":"one","message_id":"two","emoji":"x"}"#,
                "",
                DeliveryMethod::Propagated,
                &[(
                    lxmf_core::constants::FIELD_CUSTOM_TYPE,
                    LEGACY_RATSPEAK_REACTION_TYPE.to_vec(),
                )],
            )
            .unwrap();
        assert_eq!(
            decode_ratspeak_chat_extension_checked(&message),
            Err(SuppressionReason::Malformed)
        );
    }

    #[test]
    fn ratspeak_chat_extension_round_trips_reaction_and_reply_fields() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);
        let target_hash_hex = hex::encode([0xAA; 32]);
        let sender_hash_hex = hex::encode([0xBB; 32]);

        let reaction_fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reaction {
            target: target_hash_hex.clone(),
            emoji: "👍".to_string(),
            action: "add".to_string(),
        })
        .expect("reaction fields");
        // v2 emit: namespace tag must be v2 and the target field must be the
        // 32 raw bytes, not 64 hex chars.
        let tag = reaction_fields
            .iter()
            .find(|(id, _)| *id == lxmf_core::constants::FIELD_CUSTOM_TYPE)
            .map(|(_, v)| v.as_slice());
        assert_eq!(tag, Some(RATSPEAK_CHAT_CUSTOM_TYPE_V2));
        let reaction_msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "",
                "",
                DeliveryMethod::Direct,
                &reaction_fields,
            )
            .expect("reaction msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&reaction_msg),
            Some(RatspeakChatExtension::Reaction {
                target: target_hash_hex.clone(),
                emoji: "👍".to_string(),
                action: "add".to_string(),
            })
        );

        let reply_fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reply {
            target: target_hash_hex.clone(),
            preview: "original text".to_string(),
            target_sender: Some(sender_hash_hex.clone()),
        })
        .expect("reply fields");
        let reply_msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "reply text",
                "",
                DeliveryMethod::Direct,
                &reply_fields,
            )
            .expect("reply msg");
        // Standard FIELD_REPLY_TO wins on decode; target_sender only exists in
        // the legacy envelope (and nothing downstream consumes it).
        assert_eq!(
            decode_ratspeak_chat_extension(&reply_msg),
            Some(RatspeakChatExtension::Reply {
                target: target_hash_hex,
                preview: "original text".to_string(),
                target_sender: None,
            })
        );
    }

    // Outbound dual-emit: standard LXMF fields primary, legacy 0xFB/0xFC
    // envelope alongside while EMIT_LEGACY_REPLY_REACTION_ENVELOPE holds.
    #[test]
    fn outbound_reply_emits_standard_fields_and_legacy_envelope() {
        let target_hash = [0xAA; 32];
        let fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reply {
            target: hex::encode(target_hash),
            preview: "quoted".to_string(),
            target_sender: None,
        })
        .expect("reply fields");

        let get = |id: u8| fields.iter().find(|(fid, _)| *fid == id).map(|(_, v)| v);
        assert_eq!(
            get(lxmf_core::constants::FIELD_REPLY_TO).map(Vec::as_slice),
            Some(&target_hash[..])
        );
        assert_eq!(
            get(lxmf_core::constants::FIELD_REPLY_QUOTE).map(Vec::as_slice),
            Some("quoted".as_bytes())
        );
        assert_eq!(
            get(lxmf_core::constants::FIELD_CUSTOM_TYPE).map(Vec::as_slice),
            Some(RATSPEAK_CHAT_CUSTOM_TYPE_V2)
        );
        assert!(get(lxmf_core::constants::FIELD_CUSTOM_DATA).is_some());

        // Empty preview: no quote field.
        let no_quote = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reply {
            target: hex::encode(target_hash),
            preview: String::new(),
            target_sender: None,
        })
        .expect("reply fields");
        assert!(
            !no_quote
                .iter()
                .any(|(id, _)| *id == lxmf_core::constants::FIELD_REPLY_QUOTE)
        );
    }

    // FIELD_REACTION dict must be byte-exact with Python:
    // msgpack.packb({0x00: bytes(hash), 0x01: content}, use_bin_type=True).
    #[test]
    fn outbound_reaction_field_is_byte_exact_with_python() {
        let fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reaction {
            target: hex::encode([0xAA; 32]),
            emoji: "👍".to_string(),
            action: "add".to_string(),
        })
        .expect("reaction fields");
        let reaction = fields
            .iter()
            .find(|(id, _)| *id == lxmf_core::constants::FIELD_REACTION)
            .map(|(_, v)| v.as_slice())
            .expect("standard reaction field");

        let mut expected = vec![0x82, 0x00, 0xc4, 0x20];
        expected.extend_from_slice(&[0xAA; 32]);
        expected.extend_from_slice(&[0x01, 0xc4, 0x04, 0xf0, 0x9f, 0x91, 0x8d]);
        assert_eq!(reaction, expected.as_slice());
    }

    // Reaction removals have no standard representation — emitting 0x40 would
    // read as an add on standard peers, so removals stay envelope-only.
    #[test]
    fn outbound_reaction_remove_is_legacy_envelope_only() {
        let fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reaction {
            target: hex::encode([0xAA; 32]),
            emoji: "👍".to_string(),
            action: "remove".to_string(),
        })
        .expect("reaction fields");
        assert!(
            !fields
                .iter()
                .any(|(id, _)| *id == lxmf_core::constants::FIELD_REACTION)
        );
        assert!(
            fields
                .iter()
                .any(|(id, _)| *id == lxmf_core::constants::FIELD_CUSTOM_TYPE)
        );
    }

    // Standard-only peers (e.g. MeshChatX replies) carry no 0xFB/0xFC envelope.
    #[test]
    fn decodes_standard_only_reply_and_reaction() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);
        let target_hash = [0xCD; 32];

        let reply_fields = vec![
            (lxmf_core::constants::FIELD_REPLY_TO, target_hash.to_vec()),
            (
                lxmf_core::constants::FIELD_REPLY_QUOTE,
                b"quoted line".to_vec(),
            ),
        ];
        let reply_msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "reply text",
                "",
                DeliveryMethod::Direct,
                &reply_fields,
            )
            .expect("reply msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&reply_msg),
            Some(RatspeakChatExtension::Reply {
                target: hex::encode(target_hash),
                preview: "quoted line".to_string(),
                target_sender: None,
            })
        );

        let reaction_fields = vec![(
            lxmf_core::constants::FIELD_REACTION,
            encode_standard_reaction_field(&target_hash, "👍"),
        )];
        let reaction_msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "Reacted to your message with 👍.",
                "",
                DeliveryMethod::Direct,
                &reaction_fields,
            )
            .expect("reaction msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&reaction_msg),
            Some(RatspeakChatExtension::Reaction {
                target: hex::encode(target_hash),
                emoji: "👍".to_string(),
                action: "add".to_string(),
            })
        );
    }

    // Legacy-only messages (pre-migration Ratspeak) must still decode.
    #[test]
    fn decodes_legacy_only_envelope() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);
        let target_hash_hex = hex::encode([0xAB; 32]);

        let legacy_fields: Vec<(u8, Vec<u8>)> = vec![
            (
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE_V2.to_vec(),
            ),
            (
                lxmf_core::constants::FIELD_CUSTOM_DATA,
                encode_chat_extension_data_v2(&RatspeakChatExtension::Reply {
                    target: target_hash_hex.clone(),
                    preview: "old peer".to_string(),
                    target_sender: None,
                })
                .expect("v2 payload"),
            ),
        ];
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "reply text",
                "",
                DeliveryMethod::Direct,
                &legacy_fields,
            )
            .expect("legacy msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&msg),
            Some(RatspeakChatExtension::Reply {
                target: target_hash_hex,
                preview: "old peer".to_string(),
                target_sender: None,
            })
        );
    }

    // Both present with divergent targets: standard field wins.
    #[test]
    fn standard_fields_preferred_over_legacy_envelope() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);
        let standard_target = [0x11; 32];
        let legacy_target_hex = hex::encode([0x99; 32]);

        let mut fields = vec![
            (
                lxmf_core::constants::FIELD_REPLY_TO,
                standard_target.to_vec(),
            ),
            (
                lxmf_core::constants::FIELD_REPLY_QUOTE,
                b"standard quote".to_vec(),
            ),
        ];
        fields.push((
            lxmf_core::constants::FIELD_CUSTOM_TYPE,
            RATSPEAK_CHAT_CUSTOM_TYPE_V2.to_vec(),
        ));
        fields.push((
            lxmf_core::constants::FIELD_CUSTOM_DATA,
            encode_chat_extension_data_v2(&RatspeakChatExtension::Reply {
                target: legacy_target_hex,
                preview: "legacy quote".to_string(),
                target_sender: None,
            })
            .expect("v2 payload"),
        ));
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "reply text",
                "",
                DeliveryMethod::Direct,
                &fields,
            )
            .expect("msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&msg),
            Some(RatspeakChatExtension::Reply {
                target: hex::encode(standard_target),
                preview: "standard quote".to_string(),
                target_sender: None,
            })
        );
    }

    // Hex-string REPLY_TO variant (64 ASCII chars instead of bin32) normalizes
    // into the same lowercase-hex id space.
    #[test]
    fn decodes_hex_string_reply_to_variant() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);
        let target_hash_hex = hex::encode([0xEF; 32]);

        let fields = vec![(
            lxmf_core::constants::FIELD_REPLY_TO,
            target_hash_hex.to_uppercase().into_bytes(),
        )];
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "reply text",
                "",
                DeliveryMethod::Direct,
                &fields,
            )
            .expect("msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&msg),
            Some(RatspeakChatExtension::Reply {
                target: target_hash_hex,
                preview: String::new(),
                target_sender: None,
            })
        );
    }

    // UUID-style ids (~36 chars, not valid hex of 32 bytes) must still round-
    // trip via the v1 string fallback — these come from outbound messages
    // whose msg.hash was None at send time and got a uuid::Uuid as a stand-in
    // id (see send_message_with_method path).
    #[test]
    fn ratspeak_chat_extension_falls_back_to_v1_for_uuid_targets() {
        let uuid_target = "550e8400-e29b-41d4-a716-446655440000";
        let fields = ratspeak_chat_custom_fields(&RatspeakChatExtension::Reaction {
            target: uuid_target.to_string(),
            emoji: "👍".to_string(),
            action: "add".to_string(),
        })
        .expect("reaction fields");
        let tag = fields
            .iter()
            .find(|(id, _)| *id == lxmf_core::constants::FIELD_CUSTOM_TYPE)
            .map(|(_, v)| v.as_slice());
        assert_eq!(tag, Some(RATSPEAK_CHAT_CUSTOM_TYPE_V1));
    }

    // A v2 message whose target field is shorter than 32 bytes must be
    // rejected cleanly — never thread a reaction/reply against a garbage hash.
    #[test]
    fn ratspeak_chat_extension_rejects_v2_with_wrong_length_target() {
        use rmpv::Value;
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);

        let payload = {
            let entries = vec![
                (
                    Value::String("kind".into()),
                    Value::String("reaction".into()),
                ),
                (
                    Value::String("target".into()),
                    Value::Binary(vec![0xAA; 16]),
                ),
                (Value::String("emoji".into()), Value::String("👍".into())),
                (Value::String("action".into()), Value::String("add".into())),
            ];
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &Value::Map(entries)).unwrap();
            buf
        };
        let fields = vec![
            (
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE_V2.to_vec(),
            ),
            (lxmf_core::constants::FIELD_CUSTOM_DATA, payload),
        ];
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "fallback",
                "",
                DeliveryMethod::Direct,
                &fields,
            )
            .expect("msg");
        assert_eq!(decode_ratspeak_chat_extension(&msg), None);
    }

    // Forward-compat probe — a v2 message with an unknown `kind` (e.g. a
    // future "edit"/"delete" extension) must drop cleanly on this version.
    #[test]
    fn ratspeak_chat_extension_rejects_v2_with_unknown_kind() {
        use rmpv::Value;
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);

        let payload = {
            let entries = vec![
                (Value::String("kind".into()), Value::String("edit".into())),
                (
                    Value::String("target".into()),
                    Value::Binary(vec![0xAA; 32]),
                ),
            ];
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &Value::Map(entries)).unwrap();
            buf
        };
        let fields = vec![
            (
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE_V2.to_vec(),
            ),
            (lxmf_core::constants::FIELD_CUSTOM_DATA, payload),
        ];
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "fallback",
                "",
                DeliveryMethod::Direct,
                &fields,
            )
            .expect("msg");
        assert_eq!(decode_ratspeak_chat_extension(&msg), None);
    }

    // Legacy peers sent reactions/replies with `v` and `type` keys inside the
    // 0xFC payload. We removed both from the encoder, but the decoder must
    // still parse legacy messages — it always discriminated on `kind` and
    // ignored those keys. This test pins that contract.
    #[test]
    fn ratspeak_chat_extension_decodes_legacy_v_and_type_fields() {
        use rmpv::Value;
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x22; 16]);

        let legacy_payload = {
            let entries = vec![
                (Value::String("v".into()), Value::Integer(1.into())),
                (
                    Value::String("type".into()),
                    Value::String("ratspeak.chat.v1".into()),
                ),
                (
                    Value::String("kind".into()),
                    Value::String("reaction".into()),
                ),
                (
                    Value::String("target".into()),
                    Value::String("legacy-id".into()),
                ),
                (Value::String("emoji".into()), Value::String("👍".into())),
                (Value::String("action".into()), Value::String("add".into())),
            ];
            let mut buf = Vec::new();
            rmpv::encode::write_value(&mut buf, &Value::Map(entries)).unwrap();
            buf
        };

        let legacy_fields = vec![
            (
                lxmf_core::constants::FIELD_CUSTOM_TYPE,
                RATSPEAK_CHAT_CUSTOM_TYPE.to_vec(),
            ),
            (lxmf_core::constants::FIELD_CUSTOM_DATA, legacy_payload),
        ];
        let msg = mgr
            .create_final_message_with_custom_fields(
                &dest_hex,
                "fallback",
                "",
                DeliveryMethod::Direct,
                &legacy_fields,
            )
            .expect("legacy msg");
        assert_eq!(
            decode_ratspeak_chat_extension(&msg),
            Some(RatspeakChatExtension::Reaction {
                target: "legacy-id".to_string(),
                emoji: "👍".to_string(),
                action: "add".to_string(),
            })
        );
    }

    #[test]
    fn reply_send_persists_reply_metadata_before_delivery() {
        let mut mgr = test_manager();
        let pool = test_pool();
        let dest_hex = hex::encode([0x33; 16]);
        let msg_id = mgr
            .send_reply_with_preference(ReplyMessageSendRequest {
                thread: "",
                dest_hash_hex: &dest_hex,
                content: "reply text",
                title: "",
                reply_to_id: "message-b",
                reply_to_preview: "original text",
                db_pool: &pool,
                identity_id: "identity-a",
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("reply send");

        let rows = db::get_conversation(&pool, &dest_hex, "identity-a", 10);
        let saved = rows
            .iter()
            .find(|row| row.get("id").and_then(|v| v.as_str()) == Some(msg_id.as_str()))
            .expect("saved reply");
        assert_eq!(
            saved.get("reply_to_id").and_then(|v| v.as_str()),
            Some("message-b")
        );
        assert_eq!(
            saved.get("reply_to_preview").and_then(|v| v.as_str()),
            Some("original text")
        );
    }

    #[test]
    fn load_or_create_honors_preferred_identity_hash() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-preferred-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = test_pool();
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let (second_hash, _) = mgr.create_identity("Second", &pool).unwrap();

        let loaded = LxmfManager::load_or_create(&tmp, Some(&second_hash), None).unwrap();
        assert_eq!(loaded.identity_hash, second_hash);
    }

    #[test]
    fn storage_namespace_requires_initialization_before_manager_creation() {
        let temp = tempfile::tempdir().unwrap();
        assert!(LxmfManager::load_or_create(temp.path(), None, None).is_err());
        assert!(!temp.path().join(".ratspeak").exists());
    }

    #[test]
    fn storage_namespace_fresh_bootstrap_and_restart_preserve_private_ratchet() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fresh/nested/profile");
        initialize_storage_namespace(&root).unwrap();
        let first = LxmfManager::load_or_create(&root, None, None).unwrap();
        let hash = first.identity_hash.clone();
        let key = first.delivery_ratchets.ring().current_public_key();
        assert!(key.is_some());
        assert!(first.received_ratchets_dir.is_dir());
        assert!(identity_namespace_marker(&root.join(".ratspeak"), &hash).is_file());
        first.save_crypto_state().unwrap();
        drop(first);
        initialize_storage_namespace(&root).unwrap();
        let restarted = LxmfManager::load_or_create(&root, Some(&hash), None).unwrap();
        assert_eq!(restarted.delivery_ratchets.ring().current_public_key(), key);
    }

    #[test]
    fn storage_namespace_live_rebuild_preserves_open_database_and_identity() {
        use std::io::{Read, Seek};
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let first = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let identity = first.identity.clone();
        let key = identity.get_private_key().unwrap().to_vec();
        let id_file = first
            .data_dir
            .join("identities")
            .join(&first.identity_hash)
            .join("identity");
        let mut identity_handle = std::fs::File::open(&id_file).unwrap();
        let pool = crate::db::init_pool(temp.path()).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let connection = pool.get().unwrap();
        let rebuilt =
            LxmfManager::from_loaded_identity(temp.path(), identity.clone(), false).unwrap();
        assert_eq!(
            rebuilt.delivery_ratchets.ring().current_public_key(),
            first.delivery_ratchets.ring().current_public_key()
        );
        let mut observed = Vec::new();
        identity_handle.read_to_end(&mut observed).unwrap();
        assert_eq!(observed, key);
        assert_eq!(
            connection
                .query_row("SELECT 1", [], |row| row.get::<_, i32>(0))
                .unwrap(),
            1
        );

        // Losing a committed marker during a live session must never invoke a
        // first-time Windows directory publication around its live handles.
        let marker = identity_namespace_marker(&first.data_dir, &first.identity_hash);
        std::fs::remove_file(marker).unwrap();
        let ring = std::fs::read(first.delivery_ratchets.ring_path()).unwrap();
        assert!(LxmfManager::from_loaded_identity(temp.path(), identity, false).is_err());
        assert_eq!(
            std::fs::read(first.delivery_ratchets.ring_path()).unwrap(),
            ring
        );
        identity_handle.rewind().unwrap();
        observed.clear();
        identity_handle.read_to_end(&mut observed).unwrap();
        assert_eq!(observed, key);
    }

    #[test]
    fn storage_namespace_barrier_failure_prevents_first_ratchet_publication() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let identity = Identity::new();
        let hash = hex::encode(identity.hash);
        let directory = temp.path().join(".ratspeak/identities").join(&hash);
        std::fs::create_dir_all(&directory).unwrap();
        identity.to_file(&directory.join("identity")).unwrap();
        std::fs::write(directory.join("ratchets"), b"preserve namespace blocker").unwrap();
        assert!(initialize_storage_namespace(temp.path()).is_err());
        assert!(LxmfManager::from_loaded_identity(temp.path(), identity, false).is_err());
        assert_eq!(
            std::fs::read(directory.join("ratchets")).unwrap(),
            b"preserve namespace blocker"
        );
        assert!(!directory.join("ratchets/ring").exists());
        assert!(!identity_namespace_marker(&temp.path().join(".ratspeak"), &hash).exists());
    }

    #[test]
    fn storage_namespace_unexplained_directory_loss_is_not_recreated() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let manager = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let identity = manager.identity.clone();
        let ring = std::fs::read(manager.delivery_ratchets.ring_path()).unwrap();
        std::fs::remove_dir(&manager.received_ratchets_dir).unwrap();
        assert!(LxmfManager::from_loaded_identity(temp.path(), identity.clone(), false).is_err());
        assert!(manager.save_crypto_state().is_err());
        drop(manager);
        assert!(initialize_storage_namespace(temp.path()).is_err());
        let directory = temp
            .path()
            .join(".ratspeak/identities")
            .join(hex::encode(identity.hash));
        assert!(!directory.join("ratchets/received").exists());
        assert_eq!(
            std::fs::read(directory.join("ratchets/ring")).unwrap(),
            ring
        );
    }

    #[test]
    fn storage_namespace_intentional_delete_reset_and_reimport_keep_live_roots() {
        for cascade in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            initialize_storage_namespace(temp.path()).unwrap();
            let first = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
            let hash = first.identity_hash.clone();
            let key = first.identity.get_private_key().unwrap().to_vec();
            let files = first.data_dir.join("identities").join(&hash).join("files");
            std::fs::create_dir_all(&files).unwrap();
            std::fs::write(files.join("history.bin"), b"retained history").unwrap();
            let pool = crate::db::init_pool(temp.path()).unwrap();
            crate::db::init_schema(&pool).unwrap();
            let connection = pool.get().unwrap();
            drop(first);
            LxmfManager::purge_identity_profile(temp.path(), &hash, cascade).unwrap();
            assert_eq!(files.join("history.bin").exists(), !cascade);
            assert!(!identity_namespace_marker(&temp.path().join(".ratspeak"), &hash).exists());
            let (imported, _) = LxmfManager::import_identity_to_data_dir(
                &temp.path().join(".ratspeak"),
                &key,
                "Restored",
                &pool,
            )
            .unwrap();
            assert_eq!(imported, hash);
            let restored = LxmfManager::load_or_create(temp.path(), Some(&hash), None).unwrap();
            assert!(
                restored
                    .delivery_ratchets
                    .ring()
                    .current_public_key()
                    .is_some()
            );
            drop(restored);
            clear_identity_storage_namespace(&temp.path().join(".ratspeak")).unwrap();
            assert!(temp.path().join(".ratspeak/identities").is_dir());
            assert_eq!(
                connection
                    .query_row("SELECT 1", [], |row| row.get::<_, i32>(0))
                    .unwrap(),
                1
            );
            LxmfManager::import_identity_to_data_dir(
                &temp.path().join(".ratspeak"),
                &key,
                "Restored again",
                &pool,
            )
            .unwrap();
        }
    }

    #[test]
    fn storage_namespace_reset_rejects_other_sibling_without_poisoning_ready() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let wrong = temp.path().join("other");
        std::fs::create_dir_all(wrong.join("identities")).unwrap();
        let sentinel = wrong.join("identities/private-file");
        std::fs::write(&sentinel, b"leave this sibling untouched").unwrap();
        assert!(clear_identity_storage_namespace(&wrong).is_err());
        assert!(initialize_identity_storage_namespace(&wrong, &"aa".repeat(16)).is_err());
        assert_eq!(
            std::fs::read(sentinel).unwrap(),
            b"leave this sibling untouched"
        );
        assert_eq!(
            std::fs::read(temp.path().join(STORAGE_NAMESPACE_READY)).unwrap(),
            STORAGE_NAMESPACE_BYTES
        );
        LxmfManager::load_or_create(temp.path(), None, None).unwrap();
    }

    #[test]
    fn storage_namespace_bulk_reset_finishes_pending_crypto_only_deletion() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let first = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let hash = first.identity_hash.clone();
        let ratspeak = first.data_dir.clone();
        let directory = ratspeak.join("identities").join(&hash);
        std::fs::create_dir_all(directory.join("files")).unwrap();
        std::fs::write(
            directory.join("files/history.bin"),
            b"also remove this for full reset",
        )
        .unwrap();
        drop(first);
        // A interrupted crypto-only removal must not block a later authorized
        // full reset, nor leave retained media after the stronger reset scope.
        durable_write(
            &ratspeak
                .join("identities")
                .join(format!("{hash}.namespace-delete")),
            DELETE_IDENTITY_CRYPTO,
        )
        .unwrap();
        clear_identity_storage_namespace(&ratspeak).unwrap();
        assert!(!directory.exists());
        assert_eq!(
            std::fs::read_dir(ratspeak.join("identities"))
                .unwrap()
                .count(),
            0
        );
        established_storage_namespace(temp.path()).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn storage_namespace_reset_unlinks_symlink_without_touching_destination() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let ratspeak = temp.path().join(".ratspeak");
        let outside = temp.path().join("outside-private-file");
        std::fs::write(&outside, b"outside reset scope").unwrap();
        let link = ratspeak.join("identities/unknown-link");
        let outside_dir = temp.path().join("outside-private-directory");
        std::fs::create_dir(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("keep"), b"outside directory unchanged").unwrap();
        let directory_link = ratspeak.join("identities/unknown-directory-link");
        let hash_link = ratspeak.join("identities").join("cc".repeat(16));
        let metadata_link = ratspeak
            .join("identities")
            .join(format!("{}.namespace-delete", "dd".repeat(16)));
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            std::os::unix::fs::symlink(&outside_dir, &directory_link).unwrap();
            std::os::unix::fs::symlink(&outside_dir, &hash_link).unwrap();
            std::os::unix::fs::symlink(&outside, &metadata_link).unwrap();
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(&outside, &link).unwrap();
            std::os::windows::fs::symlink_dir(&outside_dir, &directory_link).unwrap();
            std::os::windows::fs::symlink_dir(&outside_dir, &hash_link).unwrap();
            std::os::windows::fs::symlink_file(&outside, &metadata_link).unwrap();
        }
        clear_identity_storage_namespace(&ratspeak).unwrap();
        assert!(!link.try_exists().unwrap());
        assert!(!directory_link.try_exists().unwrap());
        assert!(!hash_link.try_exists().unwrap());
        assert!(!metadata_link.try_exists().unwrap());
        assert_eq!(
            std::fs::read(outside_dir.join("keep")).unwrap(),
            b"outside directory unchanged"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside reset scope");
        established_storage_namespace(temp.path()).unwrap();
    }

    #[test]
    fn storage_namespace_bulk_reset_restart_fences_imports_and_clears_unknown_children() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let first = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let identity = first.identity.clone();
        let key = identity.get_private_key().unwrap().to_vec();
        let ratspeak = first.data_dir.clone();
        let identities = ratspeak.join("identities");
        drop(first);
        std::fs::create_dir_all(identities.join("legacy-unrecognized-profile")).unwrap();
        std::fs::write(
            identities.join("legacy-unrecognized-profile/private-key"),
            b"reset this old data",
        )
        .unwrap();
        std::fs::write(
            identities.join("unrecognized-keyfile"),
            b"reset this old file",
        )
        .unwrap();
        // Simulate interruption immediately after committing the outside reset
        // intent. Creation is fenced until initial startup completes that wipe.
        durable_write(
            &temp.path().join(STORAGE_NAMESPACE_READY),
            STORAGE_NAMESPACE_RESET_BYTES,
        )
        .unwrap();
        let pool = test_pool();
        assert!(LxmfManager::from_loaded_identity(temp.path(), identity, false).is_err());
        assert!(
            LxmfManager::import_identity_to_data_dir(&ratspeak, &key, "Too early", &pool).is_err()
        );
        initialize_storage_namespace(temp.path()).unwrap();
        assert!(identities.is_dir());
        assert_eq!(std::fs::read_dir(&identities).unwrap().count(), 0);
        let (hash, _) =
            LxmfManager::import_identity_to_data_dir(&ratspeak, &key, "Restored", &pool).unwrap();
        let restored = LxmfManager::load_or_create(temp.path(), Some(&hash), None).unwrap();
        drop(restored);
        // A completed reset cannot replay over a deliberately restored profile.
        initialize_storage_namespace(temp.path()).unwrap();
        assert!(identities.join(hash).join("identity").is_file());
    }

    #[test]
    fn storage_namespace_restart_finishes_only_recorded_identity_deletion() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let first = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let hash = first.identity_hash.clone();
        let key = first.identity.get_private_key().unwrap().to_vec();
        let ratspeak = first.data_dir.clone();
        let identities = ratspeak.join("identities");
        let directory = identities.join(&hash);
        drop(first);
        let intent = identities.join(format!("{hash}.namespace-delete"));
        let receipt = durable_write(&intent, DELETE_IDENTITY_ALL).unwrap();
        let retired = identities.join(format!("{hash}.namespace-retired"));
        durable_rename_directory(&directory, &retired, &receipt).unwrap();
        // Simulate a restart after the move, before sidecar/private-byte cleanup.
        initialize_storage_namespace(temp.path()).unwrap();
        assert!(!directory.exists());
        assert!(!retired.exists());
        assert!(!intent.exists());
        assert!(!identity_namespace_marker(&ratspeak, &hash).exists());
        let pool = test_pool();
        LxmfManager::import_identity_to_data_dir(&ratspeak, &key, "Restored", &pool).unwrap();
        LxmfManager::load_or_create(temp.path(), Some(&hash), None).unwrap();
    }

    #[test]
    fn storage_namespace_touches_nothing_while_another_owner_holds_the_profile() {
        let temp = tempfile::tempdir().unwrap();
        let other = std::fs::File::create(temp.path().join(".ratspeak-host.lock")).unwrap();
        rns_identity::persistence::try_lock_exclusive(&other).unwrap();

        let error = initialize_storage_namespace(temp.path()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::ResourceBusy);
        assert!(!temp.path().join(".ratspeak").exists());
        assert!(!temp.path().join(STORAGE_NAMESPACE_WITNESS).exists());
    }

    /// Set only in the second process: the profile root it tries to open.
    const SECOND_OWNER_ROOT: &str = "RATSPEAK_TEST_SECOND_OWNER_ROOT";

    #[test]
    fn storage_namespace_second_process_leaves_recorded_deletions_untouched() {
        if let Some(root) = std::env::var_os(SECOND_OWNER_ROOT) {
            let error = initialize_storage_namespace(Path::new(&root)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::ResourceBusy);
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let manager = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let hash = manager.identity_hash.clone();
        let identities = manager.data_dir.join("identities");
        drop(manager);
        let intent = identities.join(format!("{hash}.namespace-delete"));
        durable_write(&intent, DELETE_IDENTITY_ALL).unwrap();
        let ready = temp.path().join(STORAGE_NAMESPACE_READY);
        durable_write(&ready, STORAGE_NAMESPACE_RESET_BYTES).unwrap();

        // The same test, run as another process against this profile.
        let name = concat!(
            module_path!(),
            "::storage_namespace_second_process_leaves_recorded_deletions_untouched"
        );
        let name = name.split_once("::").unwrap().1;
        let second = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(SECOND_OWNER_ROOT, temp.path())
            .output()
            .unwrap();
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&second.stdout),
            String::from_utf8_lossy(&second.stderr)
        );

        assert!(
            std::fs::read(&ready).unwrap() == STORAGE_NAMESPACE_RESET_BYTES,
            "the second process finished the pending reset"
        );
        assert!(
            intent.is_file() && identities.join(&hash).join("identity").is_file(),
            "the second process retired the identity"
        );
        assert!(second.status.success(), "{output}");
        assert!(output.contains("test result: ok. 1 passed"), "{output}");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn storage_namespace_refuses_a_linked_profile_before_reading_its_database() {
        let temp = tempfile::tempdir().unwrap();
        // A newer database somewhere else, and a profile whose `.ratspeak`
        // only links to it.
        let elsewhere = temp.path().join("elsewhere");
        let pool = crate::db::init_pool(&elsewhere).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let current: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        pool.get()
            .unwrap()
            .execute("UPDATE schema_version SET version = ?1", [current + 1])
            .unwrap();
        drop(pool);
        let root = temp.path().join("profile");
        std::fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(elsewhere.join(".ratspeak"), root.join(".ratspeak")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(elsewhere.join(".ratspeak"), root.join(".ratspeak"))
            .unwrap();

        let error = initialize_storage_namespace(&root).unwrap_err();

        assert!(
            error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<crate::db::NewerSchema>())
                .is_none(),
            "the linked database was read"
        );
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    // A profile made before private folders is readable by every local user.
    #[cfg(unix)]
    #[test]
    fn storage_namespace_makes_an_older_readable_profile_private() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let ratspeak = temp.path().join(".ratspeak");
        std::fs::create_dir(&ratspeak).unwrap();
        std::fs::set_permissions(&ratspeak, std::fs::Permissions::from_mode(0o755)).unwrap();

        initialize_storage_namespace(temp.path()).unwrap();

        let mode = std::fs::metadata(&ratspeak).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn storage_namespace_refuses_a_newer_database_before_reconciling() {
        let temp = tempfile::tempdir().unwrap();
        initialize_storage_namespace(temp.path()).unwrap();
        let manager = LxmfManager::load_or_create(temp.path(), None, None).unwrap();
        let hash = manager.identity_hash.clone();
        let identities = manager.data_dir.join("identities");
        drop(manager);
        let pool = crate::db::init_pool(temp.path()).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let current: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        pool.get()
            .unwrap()
            .execute("UPDATE schema_version SET version = ?1", [current + 1])
            .unwrap();
        drop(pool);
        // A newer build recorded a deletion and a reset in a format this
        // build may read differently; neither may be replayed here.
        let intent = identities.join(format!("{hash}.namespace-delete"));
        durable_write(&intent, DELETE_IDENTITY_ALL).unwrap();
        let ready = temp.path().join(STORAGE_NAMESPACE_READY);
        durable_write(&ready, STORAGE_NAMESPACE_RESET_BYTES).unwrap();

        let result = initialize_storage_namespace(temp.path());

        assert!(
            std::fs::read(&ready).unwrap() == STORAGE_NAMESPACE_RESET_BYTES,
            "the pending reset was finished"
        );
        assert!(
            intent.is_file() && identities.join(&hash).join("identity").is_file(),
            "the recorded deletion was replayed"
        );
        let error = result.unwrap_err();
        let newer = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<crate::db::NewerSchema>())
            .expect("a newer database is reported as such");
        assert_eq!((newer.found, newer.supported), (current + 1, current));
    }

    #[test]
    fn ratchet_files_are_identity_scoped_and_restart_verified() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-ratchet-identity-scope-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let pool = test_pool();
        initialize_storage_namespace(&tmp).unwrap();
        let first = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let first_hash = first.identity_hash.clone();
        let first_public = first.delivery_ratchets.ring().current_public_key().unwrap();
        let first_ring = first.delivery_ratchets.ring_path().to_path_buf();
        let first_control = first.delivery_ratchets.control_path().to_path_buf();
        let (second_hash, _) = first.create_identity("Second", &pool).unwrap();
        drop(first);

        let second = LxmfManager::load_or_create(&tmp, Some(&second_hash), None).unwrap();
        assert_ne!(second.delivery_ratchets.ring_path(), first_ring);
        assert_ne!(second.delivery_ratchets.control_path(), first_control);
        assert!(
            second
                .delivery_ratchets
                .ring_path()
                .starts_with(tmp.join(".ratspeak/identities").join(&second_hash))
        );
        assert!(second.delivery_ratchets.ring_path().is_file());
        assert!(second.delivery_ratchets.control_path().is_file());
        drop(second);

        let restarted = LxmfManager::load_or_create(&tmp, Some(&first_hash), None).unwrap();
        assert_eq!(restarted.delivery_ratchets.ring_path(), first_ring);
        assert_eq!(restarted.delivery_ratchets.control_path(), first_control);
        assert_eq!(
            restarted.delivery_ratchets.ring().current_public_key(),
            Some(first_public),
            "using one global ring would silently replace the first identity's decryption key"
        );
        assert_eq!(
            rns_identity::ratchet::RatchetRing::load_verified(&first_ring, &restarted.identity)
                .unwrap()
                .format(),
            rns_identity::ratchet::RatchetRingFormat::Canonical
        );
        rns_identity::announce_state::RatchetControlState::load_verified(
            &first_control,
            &restarted.identity,
            restarted.lxmf_dest_hash,
        )
        .unwrap();
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn corrupt_own_ring_is_preserved_and_only_unratcheted_announces_escape() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-ratchet-corrupt-ring-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let first = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let identity_hash = first.identity_hash.clone();
        let ring_path = first.delivery_ratchets.ring_path().to_path_buf();
        drop(first);
        let corrupt = b"preserve this corrupt ring for recovery";
        std::fs::write(&ring_path, corrupt).unwrap();

        let mut restarted = LxmfManager::load_or_create(&tmp, Some(&identity_hash), None).unwrap();
        let raw = restarted
            .build_delivery_announce_packet_at(DeliveryAnnounceKind::Broadcast, 100, 1.0)
            .expect("safe unratcheted fallback");
        let (header, offset) = rns_wire::header::PacketHeader::unpack(&raw).unwrap();
        let announce =
            rns_identity::announce::AnnounceData::unpack(&raw[offset..], header.flags.context_flag)
                .unwrap();
        assert!(restarted.save_crypto_state().is_err());

        assert!(!header.flags.context_flag);
        assert_eq!(announce.ratchet, None);
        assert_eq!(
            std::fs::read(&ring_path).unwrap(),
            corrupt,
            "startup or shutdown must not destroy evidence or recovery material"
        );
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn contact_card_public_key_uses_the_loaded_identity_without_its_key_file() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-export-active-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let hash = mgr.identity_hash.clone();
        let public_key = mgr.identity.get_public_key();
        let id_file = tmp
            .join(".ratspeak")
            .join("identities")
            .join(&hash)
            .join("identity");
        std::fs::remove_file(id_file).unwrap();

        assert_eq!(mgr.contact_card_public_key(&hash).unwrap(), public_key);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn contact_card_public_key_reads_plain_profile_without_manager_lock() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-profile-contact-card-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let hash = mgr.identity_hash.clone();
        let public_key = mgr.identity.get_public_key();
        drop(mgr);

        assert_eq!(
            contact_card_public_key_from_profile(&tmp.join(".ratspeak"), &hash),
            Some(public_key)
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(feature = "seed")]
    #[test]
    fn contact_card_public_key_uses_hwid_public_metadata_without_private_key() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-hwid-contact-card-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let hardware_identity = Identity::new();
        let hash = hex::encode(hardware_identity.hash);
        let public_key = hardware_identity.get_public_key();
        let id_dir = tmp.join(".ratspeak").join("identities").join(&hash);
        std::fs::create_dir_all(&id_dir).unwrap();
        std::fs::write(
            id_dir.join("identity.hwid"),
            format!(
                "[identity]\n\
                 hash = \"{hash}\"\n\
                 nickname = \"Hardware\"\n\
                 created_at = 1\n\
                 \n\
                 [device]\n\
                 type = \"yubikey5\"\n\
                 serial = 123456\n\
                 firmware = \"5.7.4\"\n\
                 \n\
                 [keys]\n\
                 ed25519_pub = \"{}\"\n\
                 x25519_pub = \"{}\"\n\
                 \n\
                 [slots]\n\
                 signing = \"9A\"\n\
                 encryption = \"9D\"\n\
                 \n\
                 [policy]\n\
                 pin_cache_timeout = 300\n\
                 touch_signing = \"always\"\n\
                 touch_encryption = \"cached\"\n",
                hex::encode(&public_key[32..]),
                hex::encode(&public_key[..32]),
            ),
        )
        .unwrap();

        assert_eq!(mgr.contact_card_public_key(&hash).unwrap(), public_key);
        assert_eq!(
            contact_card_public_key_from_profile(&tmp.join(".ratspeak"), &hash),
            Some(public_key)
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn load_or_create_rejects_missing_preferred_identity_hash() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-missing-preferred-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let _ = LxmfManager::load_or_create(&tmp, None, None).unwrap();

        let missing_hash = "ab".repeat(16);
        match LxmfManager::load_or_create(&tmp, Some(&missing_hash), None) {
            Ok(_) => panic!("missing preferred identity should fail"),
            Err(err) => assert!(err.to_string().contains("active identity file not found")),
        }
    }

    #[test]
    fn load_or_create_rejects_unvalidated_identity_hash_before_path_use() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-invalid-preferred-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let err = match LxmfManager::load_or_create(&tmp, Some("../../human-label"), None) {
            Ok(_) => panic!("unvalidated preferred identity should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("16-byte protocol hash"));
        assert!(
            !tmp.exists(),
            "invalid identity input must be rejected before filesystem setup"
        );
    }

    #[test]
    fn direct_link_hops_follow_cached_route_hops() {
        let mut mgr = test_manager();
        let dest = [0x42; 16];

        assert_eq!(mgr.delivery_link_hops(dest), 1);

        mgr.update_route_hop(dest, 4);
        assert_eq!(mgr.delivery_link_hops(dest), 4);

        mgr.update_route_hop(dest, 0);
        assert_eq!(mgr.delivery_link_hops(dest), 1);
    }

    #[test]
    fn identified_lxmf_link_registers_backchannel() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, _command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let link_id = [0x11; 16];
        let identity_hash = [0x22; 16];
        identified_tx.try_send((link_id, identity_hash)).unwrap();

        let mut results = Vec::new();
        mgr.drain_backchannel_events(&mut results);

        let dest_hash =
            Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity_hash));
        assert_eq!(
            mgr.link_delivery
                .as_ref()
                .and_then(|ld| ld.backchannel_link_snapshot(dest_hash))
                .map(|snapshot| snapshot.link_id),
            Some(link_id)
        );
        assert!(results.is_empty());
    }

    #[test]
    fn unexpected_runtime_identity_failures_retire_cached_backchannels() {
        for error in [
            rns_runtime::link_manager::LinkSendError::IdentityUnavailable,
            rns_runtime::link_manager::LinkSendError::IdentificationUnavailable,
        ] {
            assert_eq!(
                backchannel_error_from_runtime(error),
                BackchannelSendError::LinkNotActive
            );
        }
    }

    #[tokio::test]
    async fn direct_delivery_prefers_registered_backchannel() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x33; 16];
        let link_id = [0x44; 16];
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Backchannel",
            "reply over inbound link",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);
        assert_eq!(
            results,
            vec![(hex::encode(msg_hash), "reusing_backchannel")]
        );
        mgr.drain_link_delivery_progress_updates();
        let progress = mgr.take_delivery_progress_updates();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].msg_id, hex::encode(msg_hash));
        assert_eq!(progress[0].step, "reusing_backchannel");
        assert_eq!(progress[0].progress, Some(0.05));
        assert_eq!(progress[0].link_id, Some(hex::encode(link_id)));
        assert_eq!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .unwrap()
                .queued_deliveries,
            1
        );

        let command = command_rx.try_recv().expect("backchannel send command");
        match command {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                link_id: command_link,
                payload,
                ..
            } => {
                assert_eq!(command_link, link_id);
                assert!(!payload.is_empty());
            }
            _ => panic!("expected SendLinkPayload command"),
        }
    }

    #[tokio::test]
    async fn direct_delivery_to_explicit_no_bz2_peer_disables_backchannel_compression() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x34; 16];
        let dest_hex = hex::encode(dest);
        let link_id = [0x45; 16];
        mgr.peer_lxmf_compression_support
            .insert(dest, CompressionSupport::Unsupported);
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);

        let msg = mgr
            .create_final_message(&dest_hex, "direct no bz2", "", DeliveryMethod::Direct)
            .unwrap();
        assert!(!msg.auto_compress);
        let msg_hash = msg.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);
        assert_eq!(
            results,
            vec![(hex::encode(msg_hash), "reusing_backchannel")]
        );

        let command = command_rx.try_recv().expect("backchannel send command");
        match command {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                link_id: command_link,
                auto_compress,
                ..
            } => {
                assert_eq!(command_link, link_id);
                assert!(!auto_compress);
            }
            _ => panic!("expected SendLinkPayload command"),
        }
    }

    #[test]
    fn resource_progress_events_use_resource_specific_steps() {
        let msg_hash = [0x11; 32];
        let base = LxmfDeliveryEvent {
            kind: LxmfDeliveryEventKind::TransferStarted,
            method: LxmfDeliveryEventMethod::Direct,
            link_id: [0x22; 16],
            dest_hash: [0x33; 16],
            msg_hash: Some(msg_hash),
            attempts: 1,
            progress: Some(0.10),
            representation: DeliveryRepresentation::Resource,
            link_state: rns_link::link::LinkState::Active,
            delivery_state: DeliveryState::Transferring,
            queued_deliveries: 0,
            in_flight_deliveries: 1,
            reason: None,
        };

        let advertised = LxmfManager::progress_update_from_link_event(base.clone()).unwrap();
        assert_eq!(advertised.step, "resource_advertised");
        assert_eq!(advertised.kind, LxmfDeliveryProgressKind::TransferStarted);
        assert_eq!(advertised.event_method, LxmfDeliveryProgressMethod::Direct);
        assert_eq!(
            advertised.delivery_representation,
            LxmfDeliveryProgressRepresentation::Resource
        );

        let transferring = LxmfManager::progress_update_from_link_event(LxmfDeliveryEvent {
            kind: LxmfDeliveryEventKind::TransferProgress,
            progress: Some(0.42),
            ..base.clone()
        })
        .unwrap();
        assert_eq!(transferring.step, "resource_transferring");

        let waiting_for_proof = LxmfManager::progress_update_from_link_event(LxmfDeliveryEvent {
            kind: LxmfDeliveryEventKind::TransferProgress,
            progress: Some(0.99),
            ..base
        })
        .unwrap();
        assert_eq!(waiting_for_proof.step, "resource_waiting_for_proof");
    }

    #[tokio::test]
    async fn direct_reply_waits_for_link_identify_before_reusing_backchannel() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let identity_hash = [0x22; 16];
        let dest = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(&identity_hash));
        let link_id = [0x44; 16];
        mgr.note_pending_direct_backchannel(dest, link_id);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Pending identify",
            "reply should wait for LINKIDENTIFY",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();

        let waiting = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);
        assert_eq!(waiting, vec![(hex::encode(msg_hash), "sending_via_link")]);
        assert!(command_rx.try_recv().is_err());
        assert!(
            mgr.link_delivery
                .as_ref()
                .and_then(|ld| ld.backchannel_link_snapshot(dest))
                .is_none()
        );

        identified_tx.try_send((link_id, identity_hash)).unwrap();
        let results = mgr.tick();
        assert_eq!(
            results,
            vec![(hex::encode(msg_hash), "reusing_backchannel")]
        );

        let command = command_rx.try_recv().expect("backchannel send command");
        match command {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                link_id: command_link,
                payload,
                ..
            } => {
                assert_eq!(command_link, link_id);
                assert!(!payload.is_empty());
            }
            _ => panic!("expected SendLinkPayload command"),
        }
    }

    #[test]
    fn closed_lxmf_link_removes_registered_backchannel() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, _command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x35; 16];
        let link_id = [0x45; 16];
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .is_some()
        );

        backchannel_event_tx
            .send(BackchannelLinkEvent::LinkClosed { link_id })
            .unwrap();
        let mut results = Vec::new();
        mgr.drain_backchannel_events(&mut results);

        assert!(results.is_empty());
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .is_none()
        );
    }

    #[tokio::test]
    async fn stale_backchannel_send_failure_requeues_direct_message() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x36; 16];
        let link_id = [0x46; 16];
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Backchannel",
            "stale backchannel",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();

        let initial = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);
        assert_eq!(
            initial,
            vec![(hex::encode(msg_hash), "reusing_backchannel")]
        );

        match command_rx.try_recv().expect("backchannel send command") {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                result_tx: Some(result_tx),
                ..
            } => {
                result_tx
                    .send(Err(rns_runtime::link_manager::LinkSendError::LinkNotActive))
                    .unwrap();
            }
            _ => panic!("expected SendLinkPayload command with result channel"),
        }
        tokio::task::yield_now().await;

        let delivery_results = mgr.link_delivery.as_mut().unwrap().tick();
        let mut results = Vec::new();
        for result in delivery_results {
            mgr.handle_link_delivery_result(result, &mut results);
        }

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .is_none()
        );
        let requeued = mgr
            .router
            .pending_outbound
            .iter()
            .find(|message| message.message_id() == msg_hash)
            .expect("message should remain pending after stale backchannel");
        assert_eq!(requeued.method, DeliveryMethod::Direct);
        assert!(requeued.next_delivery_attempt > 0.0);
    }

    #[tokio::test]
    async fn ordered_backchannel_proof_before_receipt_and_close_marks_delivered() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x55; 16];
        let link_id = [0x66; 16];
        let packet_hash = [0x77; 32];
        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Proof",
            "proof event",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();
        mgr.auto_live_fallback.insert(msg_hash);
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .start_backchannel_delivery(msg, dest)
            .unwrap();
        mgr.drain_core_backchannel_send_commands();
        let command = command_rx.try_recv().expect("backchannel send command");
        let result_tx = match command {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                result_tx: Some(result_tx),
                ..
            } => result_tx,
            _ => panic!("expected SendLinkPayload command with result channel"),
        };

        // Reticulum can validate and publish the remote proof before this
        // asynchronous adapter observes the local send receipt. Preserve that
        // exact order so the production tick must reconcile both events.
        backchannel_event_tx
            .send(BackchannelLinkEvent::PacketProof(
                rns_runtime::link_manager::LinkPacketProof {
                    link_id,
                    packet_hash,
                },
            ))
            .unwrap();
        backchannel_event_tx
            .send(BackchannelLinkEvent::LinkClosed { link_id })
            .unwrap();
        result_tx
            .send(Ok(
                rns_runtime::link_manager::LinkPayloadSendReceipt::Packet(
                    rns_runtime::link_manager::LinkPacketSendReceipt {
                        link_id,
                        packet_hash,
                    },
                ),
            ))
            .unwrap();
        tokio::task::yield_now().await;
        let results = mgr.tick();

        assert_eq!(results, vec![(hex::encode(msg_hash), "delivered")]);
        let progress = mgr.take_delivery_progress_updates();
        let delivered = progress
            .iter()
            .find(|update| update.step == "delivered")
            .expect("delivered progress event");
        assert_eq!(delivered.msg_id, hex::encode(msg_hash));
        assert_eq!(delivered.progress, Some(1.0));
        assert_eq!(delivered.representation, "packet");
        assert_eq!(mgr.link_delivery.as_ref().unwrap().pending_count(), 0);
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .is_none(),
            "later Link closure must retire reuse without overriding its earlier proof"
        );
        assert!(!mgr.auto_live_fallback.contains(&msg_hash));
    }

    #[tokio::test]
    async fn outbound_backchannel_resource_rejection_settles_exact_owner() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(4);
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x81; 16];
        let link_id = [0x82; 16];
        let resource_hash = [0x83; 32];
        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Resource rejection",
            "rejected by receiver",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .start_backchannel_delivery(msg, dest)
            .unwrap();
        mgr.drain_core_backchannel_send_commands();

        let result_tx = match command_rx.try_recv().expect("backchannel send command") {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                result_tx: Some(result_tx),
                ..
            } => result_tx,
            _ => panic!("expected SendLinkPayload command with result channel"),
        };
        result_tx
            .send(Ok(
                rns_runtime::link_manager::LinkPayloadSendReceipt::Resource(
                    rns_runtime::link_manager::LinkResourceSendReceipt {
                        link_id,
                        resource_hash,
                    },
                ),
            ))
            .unwrap();
        tokio::task::yield_now().await;
        assert!(mgr.tick().is_empty());

        backchannel_event_tx
            .send(BackchannelLinkEvent::ResourceConclusion {
                link_id,
                resource_hash,
                conclusion: rns_runtime::link_manager::LinkResourceConclusion::Rejected,
            })
            .unwrap();
        assert_eq!(mgr.tick(), vec![(hex::encode(msg_hash), "rejected")]);
        assert_eq!(mgr.link_delivery.as_ref().unwrap().pending_count(), 0);
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .backchannel_link_snapshot(dest)
                .is_some(),
            "receiver rejection is message-terminal, not Link-terminal"
        );
    }

    #[tokio::test]
    async fn backchannel_resource_cancel_survives_command_backpressure() {
        let mut mgr = test_manager();
        let (transport_tx, _transport_rx) = mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(transport_tx);
        let (command_tx, mut command_rx) =
            mpsc::channel::<rns_runtime::link_manager::LinkManagerCommand>(1);
        let command_fill_tx = command_tx.clone();
        let (_identified_tx, identified_rx) = mpsc::channel::<([u8; 16], [u8; 16])>(4);
        let (direct_tx, _direct_rx) = mpsc::unbounded_channel::<(Vec<u8>, [u8; 16])>();
        let (_backchannel_event_tx, backchannel_event_rx) = mpsc::unbounded_channel();
        mgr.set_lxmf_link_control(command_tx, direct_tx, identified_rx, backchannel_event_rx);

        let dest = [0x91; 16];
        let link_id = [0x92; 16];
        let resource_hash = [0x93; 32];
        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Resource cancel",
            "cancel exact transfer",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();
        assert!(mgr.ensure_link_delivery_manager());
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .register_backchannel(dest, link_id);
        mgr.link_delivery
            .as_mut()
            .unwrap()
            .start_backchannel_delivery(msg, dest)
            .unwrap();
        mgr.drain_core_backchannel_send_commands();

        let result_tx = match command_rx.try_recv().expect("backchannel send command") {
            rns_runtime::link_manager::LinkManagerCommand::SendLinkPayload {
                result_tx: Some(result_tx),
                ..
            } => result_tx,
            _ => panic!("expected SendLinkPayload command with result channel"),
        };
        result_tx
            .send(Ok(
                rns_runtime::link_manager::LinkPayloadSendReceipt::Resource(
                    rns_runtime::link_manager::LinkResourceSendReceipt {
                        link_id,
                        resource_hash,
                    },
                ),
            ))
            .unwrap();
        tokio::task::yield_now().await;
        assert!(mgr.tick().is_empty());

        command_fill_tx
            .try_send(rns_runtime::link_manager::LinkManagerCommand::Announce)
            .unwrap();
        assert!(mgr.cancel_outbound_message(&hex::encode(msg_hash)));
        assert_eq!(mgr.pending_backchannel_resource_cancellations.len(), 1);
        assert!(matches!(
            command_rx.try_recv(),
            Ok(rns_runtime::link_manager::LinkManagerCommand::Announce)
        ));

        let _ = mgr.tick();
        match command_rx
            .try_recv()
            .expect("retained Resource cancellation")
        {
            rns_runtime::link_manager::LinkManagerCommand::CancelLinkResource {
                link_id: cancelled_link,
                resource_id,
                direction,
                ..
            } => {
                assert_eq!(cancelled_link, link_id);
                assert_eq!(resource_id, resource_hash);
                assert_eq!(
                    direction,
                    rns_runtime::link_manager::LinkResourceDirection::Outbound
                );
            }
            _ => panic!("expected exact CancelLinkResource command"),
        }
        assert!(mgr.pending_backchannel_resource_cancellations.is_empty());
    }

    #[test]
    fn propagation_resource_rejection_is_attributed_to_relay_and_cleans_inflight_state() {
        let mut mgr = test_manager();
        let recipient = [0x71; 16];
        let relay = [0x72; 16];
        let message = lxmf_core::message::LxMessageDraft::new(
            recipient,
            mgr.lxmf_dest_hash,
            "Relay",
            "reject",
            DeliveryMethod::Propagated,
        )
        .unwrap();
        let message = message
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let hash = message.message_id();
        mgr.in_flight_propagation.insert(hash, relay);
        mgr.auto_live_fallback.insert(hash);

        let mut results = Vec::new();
        mgr.handle_link_delivery_result(
            DeliveryResult::Rejected {
                link_id: [0x73; 16],
                msg_hash: Some(hash),
                dest_hash: relay,
                message,
                reason: "resource rejected".to_string(),
            },
            &mut results,
        );

        assert_eq!(results, vec![(hex::encode(hash), "failed")]);
        assert!(!mgr.in_flight_propagation.contains_key(&hash));
        assert!(!mgr.auto_live_fallback.contains(&hash));
        let (_, failed, _, _) = mgr.take_propagation_health();
        assert_eq!(failed, vec![(relay, "resource rejected".to_string())]);
    }

    #[test]
    fn reported_delivery_steps_are_bounded_and_terminal_entries_are_removed() {
        let mut mgr = test_manager();
        for index in 0..(MAX_REPORTED_STEPS + 8) {
            let id = format!("{index:064x}");
            assert_eq!(mgr.filter_repeated_steps(vec![(id, "routing")]).len(), 1);
        }
        assert_eq!(mgr.last_reported_steps.len(), MAX_REPORTED_STEPS);

        let terminal = "ff".repeat(32);
        mgr.filter_repeated_steps(vec![(terminal.clone(), "routing")]);
        assert!(mgr.last_reported_steps.contains_key(&terminal));
        mgr.filter_repeated_steps(vec![(terminal.clone(), "delivered")]);
        assert!(!mgr.last_reported_steps.contains_key(&terminal));
    }

    #[test]
    fn direct_delivery_without_path_requeues_and_requests_path() {
        let mut mgr = test_manager();
        let dest = [0x43; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "No Path",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
        assert!(mgr.router.pending_outbound[0].last_delivery_attempt > 0.0);

        // D1: a path-request requeue defers PATH_REQUEST_WAIT (7s), not the
        // 10s link-retry cadence.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "expected next_delivery_attempt ~ now+{PATH_REQUEST_WAIT}s, got {}",
            nda - now
        );

        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
    }

    #[test]
    fn establishment_failure_without_recovery_handle_only_discovers_and_requeues() {
        let mut mgr = test_manager();
        let dest = [0x44; 16];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Retry",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap();
        msg.delivery_attempts = 1;
        msg.last_delivery_attempt = 1.0;
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();

        assert!(mgr.requeue_direct_after_link_failure(
            msg,
            dest,
            "link establishment timeout",
            None
        ));
        assert_eq!(mgr.router.pending_outbound.len(), 1);

        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }

        // D1: link-failure rediscovery requeue defers PATH_REQUEST_WAIT (7s).
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "link-failure requeue must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );
    }

    #[test]
    fn unobserved_establishment_failure_preserves_cached_route_owner() {
        let mut mgr = test_manager();
        let dest = [0x46; 16];
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 60.0,
            interface: "Local Network".to_string(),
            interface_id: 7,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Retry",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap();
        msg.delivery_attempts = 1;
        msg.last_delivery_attempt = 1.0;
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();

        assert!(mgr.requeue_direct_after_link_failure(
            msg,
            dest,
            "link establishment timeout",
            None
        ));
        assert!(mgr.route_entries.contains_key(&dest));
        assert!(mgr.route_hops.contains_key(&dest));

        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
    }

    /// D3: Python increments before Link creation, but only opens a new Link
    /// while delivery_attempts < MAX_DELIVERY_ATTEMPTS (LXMRouter.py:2655-2669).
    /// At the post-increment boundary, Rust must not emit one extra LinkRequest.
    #[test]
    fn direct_delivery_at_attempt_boundary_does_not_start_extra_link() {
        let mut mgr = test_manager();
        let dest = [0x45; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Boundary",
            "hello",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let mut msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        msg.delivery_attempts = MAX_DELIVERY_ATTEMPTS - 1;
        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert!(results.is_empty());
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_DELIVERY_ATTEMPTS
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + DELIVERY_RETRY_WAIT as f64 - 2.0
                && nda < now + DELIVERY_RETRY_WAIT as f64 + 2.0,
            "attempt-boundary deferral must use DELIVERY_RETRY_WAIT (10s), got {}",
            nda - now
        );
        assert!(
            rx.try_recv().is_err(),
            "no LinkRequest should be emitted at the post-increment attempt boundary"
        );
    }

    #[test]
    fn direct_delivery_with_current_path_reports_link_establishing() {
        let mut mgr = test_manager();
        let dest = [0x46; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct",
            "hello over link",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "link_establishing")]);
        let progress = mgr.take_delivery_progress_updates();
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].msg_id, hex::encode(msg_hash));
        assert_eq!(progress[0].step, "link_establishing");
        assert_eq!(progress[0].method, "direct");
        assert_eq!(progress[0].progress, Some(0.03));
        assert_eq!(progress[0].dest_hash, dest_hex);
        assert!(progress[0].link_id.is_some());

        match rx.try_recv().unwrap() {
            TransportMessage::RegisterDestination {
                hash,
                app_name,
                delivery_tx,
            } => {
                assert_eq!(hash.len(), 16);
                assert_eq!(app_name, "lxmf.delivery.link");
                assert!(delivery_tx.is_some());
            }
            other => panic!("expected RegisterDestination, got {other:?}"),
        }

        match rx.try_recv().unwrap() {
            TransportMessage::SendLinkRequest { request, result_tx } => {
                result_tx.send(Ok(())).unwrap();
                assert_eq!(request.destination_hash, dest);
                let (header, _) = rns_wire::header::PacketHeader::unpack(&request.raw)
                    .expect("link request header");
                assert_eq!(
                    header.flags.packet_type,
                    rns_wire::flags::PacketType::LinkRequest
                );
                assert_eq!(header.destination_hash, dest);
            }
            other => panic!("expected outbound LinkRequest, got {other:?}"),
        }
    }

    #[test]
    fn tick_direct_delivery_keeps_router_pending_without_duplicate_link_request() {
        let mut mgr = test_manager();
        let dest = [0x48; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = mgr
            .create_final_message(&dest_hex, "router-owned direct", "", DeliveryMethod::Direct)
            .expect("message created");
        let msg_hash = msg.message_id();
        mgr.router.try_send(msg).unwrap();

        assert_eq!(
            mgr.tick(),
            vec![(hex::encode(msg_hash), "link_establishing")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].message_id(), msg_hash);

        rx.try_recv().expect("destination registration");
        let TransportMessage::SendLinkRequest { result_tx, .. } =
            rx.try_recv().expect("initial LinkRequest")
        else {
            panic!("typed Link request expected");
        };
        result_tx.send(Ok(())).unwrap();

        assert_eq!(
            mgr.tick(),
            vec![(hex::encode(msg_hash), "sending_via_link")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 1);
        assert!(
            rx.try_recv().is_err(),
            "pending router-owned Direct message must not emit a second LinkRequest"
        );

        assert!(
            mgr.tick().is_empty(),
            "pending reusable Direct poll must not re-emit the same sending step"
        );
        assert!(
            rx.try_recv().is_err(),
            "suppressed pending poll must still not emit another LinkRequest"
        );
    }

    #[test]
    fn direct_delivery_with_pending_link_waits_without_new_link_request() {
        let mut mgr = test_manager();
        let dest = [0x47; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let first = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct one",
            "opens link",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let first = first
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let first_hash = first.message_id();
        assert_eq!(
            mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
                message: first,
                dest_hash: dest,
            }]),
            vec![(hex::encode(first_hash), "link_establishing")]
        );

        rx.try_recv().expect("destination registration");
        let TransportMessage::SendLinkRequest { result_tx, .. } =
            rx.try_recv().expect("initial LinkRequest")
        else {
            panic!("typed Link request expected");
        };
        result_tx.send(Ok(())).unwrap();

        let second = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Direct two",
            "waits on pending link",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let second = second
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let second_hash = second.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverDirect {
            message: second,
            dest_hash: dest,
        }]);

        assert_eq!(
            results,
            vec![(hex::encode(second_hash), "sending_via_link")]
        );
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].delivery_attempts, 0);
        assert!(
            rx.try_recv().is_err(),
            "pending reusable Direct Link must not emit another LinkRequest"
        );

        mgr.router.pending_outbound[0].delivery_attempts = MAX_DELIVERY_ATTEMPTS;
        mgr.auto_live_fallback.insert(second_hash);
        let terminal_results = mgr.tick();
        assert!(
            !terminal_results
                .iter()
                .any(|(message, step)| message == &hex::encode(second_hash)
                    && *step == "propagating"),
            "a queued message must keep waiting for the Link that already owns its destination"
        );
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|message| message.message_id() == second_hash)
        );
        assert!(mgr.auto_live_fallback.contains(&second_hash));
    }

    #[tokio::test]
    async fn auto_direct_terminal_failure_escalates_to_propagation_when_ready() {
        let mut mgr = test_manager();
        let dest = [0x49; 16];
        let node = [0x4A; 16];
        let dest_hex = hex::encode(dest);
        let node_hex = hex::encode(node);
        let remote = Identity::new();
        let relay = Identity::new();
        let (tx, _rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.known_identities
            .insert(node_hex, relay.get_public_key());
        ready_relay_policy(&mut mgr, node, 0, 256);
        mgr.router.set_stamp_cost(node, 0);
        mgr.propagation_transfer_limits_kb.insert(node, 256);
        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.configured_propagation_node = Some(node);
        mgr.client_propagation_enabled = true;

        let msg = mgr
            .create_final_message(
                &dest_hex,
                "fallback to propagation",
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");
        let hash = msg.message_id();
        mgr.auto_live_fallback.insert(hash);

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::Failed(msg)]);

        let results = finish_pending_propagation(&mut mgr, results).await;
        assert_eq!(results, vec![(hex::encode(hash), "propagating")]);
        assert_eq!(mgr.in_flight_propagation.get(&hash), Some(&node));
        assert!(!mgr.auto_live_fallback.contains(&hash));
    }

    #[test]
    fn terminal_budget_does_not_abort_in_flight_link() {
        let mut mgr = test_manager();
        let dest = [0x59; 16];
        let node = [0x5A; 16];
        let dest_hex = hex::encode(dest);
        let node_hex = hex::encode(node);
        let remote = Identity::new();
        let relay = Identity::new();
        let (tx, _rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.known_identities
            .insert(node_hex, relay.get_public_key());
        mgr.router.set_stamp_cost(node, 0);
        mgr.propagation_transfer_limits_kb.insert(node, 256);
        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.configured_propagation_node = Some(node);
        mgr.client_propagation_enabled = true;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = mgr
            .create_final_message(
                &dest_hex,
                "fallback from stuck link",
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");
        let hash = msg.message_id();
        mgr.router.try_send(msg).unwrap();
        mgr.auto_live_fallback.insert(hash);

        assert_eq!(mgr.tick(), vec![(hex::encode(hash), "link_establishing")]);
        assert_eq!(mgr.link_delivery.as_ref().unwrap().pending_count(), 1);

        mgr.router.pending_outbound[0].delivery_attempts = MAX_DELIVERY_ATTEMPTS;
        let results = mgr.tick();

        assert!(
            !results
                .iter()
                .any(|(message, step)| message == &hex::encode(hash) && *step == "propagating"),
            "an attempt budget must not terminate an in-flight Link operation"
        );
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .direct_link_snapshot(dest)
                .is_some()
        );
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|message| message.message_id() == hash)
        );
        assert_eq!(mgr.in_flight_propagation.get(&hash), None);
        assert!(mgr.auto_live_fallback.contains(&hash));
    }

    #[test]
    fn terminal_budget_does_not_abort_active_resource_delivery() {
        let mut mgr = test_manager();
        let dest = [0x69; 16];
        let node = [0x6A; 16];
        let dest_hex = hex::encode(dest);
        let node_hex = hex::encode(node);
        let remote = Identity::new();
        let relay = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(512);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.known_identities
            .insert(node_hex, relay.get_public_key());
        mgr.router.set_stamp_cost(node, 0);
        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.configured_propagation_node = Some(node);
        mgr.client_propagation_enabled = true;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = mgr
            .create_final_message(
                &dest_hex,
                &"image-like payload".repeat(rns_protocol::resource::MAX_EFFICIENT_SIZE / 18),
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");

        let hash = msg.message_id();
        assert!(msg.pack().unwrap().len() > rns_protocol::resource::MAX_EFFICIENT_SIZE);
        mgr.router.try_send(msg).unwrap();

        assert_eq!(mgr.tick(), vec![(hex::encode(hash), "link_establishing")]);
        let request_raw = next_outbound(&mut rx);
        let (_request_header, request_offset) =
            rns_wire::header::PacketHeader::unpack(&request_raw).unwrap();
        let responder_key = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let (_responder_link, proof_data) = rns_link::link::Link::new_responder(
            &request_raw[request_offset..],
            &responder_key,
            dest,
            1,
        )
        .unwrap();
        let responder_pub = responder_key.public_key();
        let link_id = mgr
            .link_delivery
            .as_ref()
            .unwrap()
            .direct_link_snapshot(dest)
            .unwrap()
            .link_id;
        assert!(mgr.link_delivery.as_mut().unwrap().handle_link_proof(
            &link_id,
            &proof_data,
            &responder_pub,
            &responder_pub.to_bytes(),
            1,
        ));
        accept_initiator_endpoint_binding(&mut rx, link_id, 1);
        let _ = mgr.tick();
        let _rtt_raw = next_initiator_endpoint_outbound(&mut rx);

        let _ = mgr.tick();
        let snapshot = mgr
            .link_delivery
            .as_ref()
            .unwrap()
            .message_delivery_snapshot(hash)
            .expect("resource delivery snapshot");
        assert_eq!(snapshot.representation, DeliveryRepresentation::Resource);
        assert_eq!(snapshot.delivery_state, DeliveryState::Transferring);

        mgr.auto_live_fallback.insert(hash);
        mgr.router
            .pending_outbound
            .iter_mut()
            .find(|message| message.message_id() == hash)
            .expect("pending Resource message")
            .delivery_attempts = MAX_DELIVERY_ATTEMPTS;
        let results = mgr.tick();

        assert!(
            !results
                .iter()
                .any(|(msg, step)| msg == &hex::encode(hash) && *step == "propagating"),
            "active Resource delivery must not be aborted by the live retry budget"
        );
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .message_delivery_snapshot(hash)
                .is_some()
        );
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|m| m.message_id() == hash)
        );
        assert_eq!(mgr.in_flight_propagation.get(&hash), None);
        assert!(mgr.auto_live_fallback.contains(&hash));
    }

    #[test]
    fn cancel_outbound_message_stops_active_resource_without_propagation() {
        let mut mgr = test_manager();
        let dest = [0x6B; 16];
        let node = [0x6C; 16];
        let dest_hex = hex::encode(dest);
        let node_hex = hex::encode(node);
        let remote = Identity::new();
        let relay = Identity::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(512);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.known_identities
            .insert(node_hex, relay.get_public_key());
        mgr.router.set_stamp_cost(node, 0);
        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.configured_propagation_node = Some(node);
        mgr.client_propagation_enabled = true;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 1,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = mgr
            .create_final_message(
                &dest_hex,
                &"image-like payload".repeat(rns_protocol::resource::MAX_EFFICIENT_SIZE / 18),
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");

        let hash = msg.message_id();
        mgr.router.try_send(msg).unwrap();

        assert_eq!(mgr.tick(), vec![(hex::encode(hash), "link_establishing")]);
        let request_raw = next_outbound(&mut rx);
        let (_request_header, request_offset) =
            rns_wire::header::PacketHeader::unpack(&request_raw).unwrap();
        let responder_key = rns_crypto::ed25519::Ed25519PrivateKey::generate();
        let (_responder_link, proof_data) = rns_link::link::Link::new_responder(
            &request_raw[request_offset..],
            &responder_key,
            dest,
            1,
        )
        .unwrap();
        let responder_pub = responder_key.public_key();
        let link_id = mgr
            .link_delivery
            .as_ref()
            .unwrap()
            .direct_link_snapshot(dest)
            .unwrap()
            .link_id;
        assert!(mgr.link_delivery.as_mut().unwrap().handle_link_proof(
            &link_id,
            &proof_data,
            &responder_pub,
            &responder_pub.to_bytes(),
            1,
        ));
        accept_initiator_endpoint_binding(&mut rx, link_id, 1);
        let _ = mgr.tick();
        let _rtt_raw = next_initiator_endpoint_outbound(&mut rx);

        let _ = mgr.tick();
        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .message_delivery_snapshot(hash)
                .is_some()
        );

        mgr.auto_live_fallback.insert(hash);
        assert!(mgr.cancel_outbound_message(&hex::encode(hash)));

        assert!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .message_delivery_snapshot(hash)
                .is_none()
        );
        assert_eq!(
            mgr.link_delivery
                .as_ref()
                .unwrap()
                .direct_link_snapshot(dest)
                .unwrap()
                .delivery_state,
            DeliveryState::Idle
        );
        assert!(
            !mgr.router
                .pending_outbound
                .iter()
                .any(|m| m.message_id() == hash)
        );
        assert_eq!(mgr.in_flight_propagation.get(&hash), None);
        assert!(!mgr.auto_live_fallback.contains(&hash));

        let results = mgr.tick();
        assert!(
            !results
                .iter()
                .any(|(msg, step)| msg == &hex::encode(hash) && *step == "propagating")
        );
    }

    #[test]
    fn terminal_direct_without_ready_propagation_fails_message() {
        let mut mgr = test_manager();
        let dest = [0x4B; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());

        let msg = mgr
            .create_final_message(
                &dest_hex,
                "fallback unavailable",
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");
        let hash = msg.message_id();
        mgr.auto_live_fallback.insert(hash);

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::Failed(msg)]);

        assert_eq!(results, vec![(hex::encode(hash), "failed")]);
        assert!(!mgr.auto_live_fallback.contains(&hash));
    }

    #[tokio::test]
    async fn auto_opportunistic_terminal_failure_escalates_to_propagation() {
        let mut mgr = test_manager();
        let dest = [0x7A; 16];
        let node = [0x7B; 16];
        let dest_hex = hex::encode(dest);
        let _transport_rx = ready_auto_propagation_fallback(&mut mgr, dest, node);
        let message = mgr
            .create_final_message(
                &dest_hex,
                "opportunistic fallback",
                "",
                DeliveryMethod::Opportunistic,
            )
            .expect("message created");
        let hash = message.message_id();
        mgr.auto_live_fallback.insert(hash);

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::Failed(message)]);

        let results = finish_pending_propagation(&mut mgr, results).await;
        assert_eq!(results, vec![(hex::encode(hash), "propagating")]);
        assert_eq!(mgr.in_flight_propagation.get(&hash), Some(&node));
        assert!(!mgr.auto_live_fallback.contains(&hash));
    }

    #[test]
    fn explicit_opportunistic_terminal_failure_does_not_propagate() {
        let mut mgr = test_manager();
        let dest = [0x7C; 16];
        let node = [0x7D; 16];
        let dest_hex = hex::encode(dest);
        let _transport_rx = ready_auto_propagation_fallback(&mut mgr, dest, node);
        let message = mgr
            .create_final_message(
                &dest_hex,
                "manual opportunistic",
                "",
                DeliveryMethod::Opportunistic,
            )
            .expect("message created");
        let hash = message.message_id();

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::Failed(message)]);

        assert_eq!(results, vec![(hex::encode(hash), "failed")]);
        assert!(!mgr.in_flight_propagation.contains_key(&hash));
    }

    #[test]
    fn auto_live_rejection_is_terminal_without_propagation() {
        let mut mgr = test_manager();
        let dest = [0x7E; 16];
        let node = [0x7F; 16];
        let dest_hex = hex::encode(dest);
        let _transport_rx = ready_auto_propagation_fallback(&mut mgr, dest, node);
        let message = mgr
            .create_final_message(&dest_hex, "rejected live send", "", DeliveryMethod::Direct)
            .expect("message created");
        let hash = message.message_id();
        mgr.auto_live_fallback.insert(hash);

        let mut results = Vec::new();
        mgr.handle_link_delivery_result(
            DeliveryResult::Rejected {
                link_id: [0x70; 16],
                msg_hash: Some(hash),
                dest_hash: dest,
                message,
                reason: "recipient rejected transfer".to_string(),
            },
            &mut results,
        );

        assert_eq!(results, vec![(hex::encode(hash), "rejected")]);
        assert!(!mgr.in_flight_propagation.contains_key(&hash));
        assert!(!mgr.auto_live_fallback.contains(&hash));
    }

    /// D2: Python `handle_outbound` pre-emptively requests an unknown path for
    /// Opportunistic messages and defers PATH_REQUEST_WAIT (LXMRouter.py:1675).
    #[test]
    fn opportunistic_preempt_requests_path_and_defers() {
        let mut mgr = test_manager();
        let dest = [0x55; 16];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let mut msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        mgr.preempt_opportunistic_path(&mut msg);

        assert!(
            msg.next_delivery_attempt > now + PATH_REQUEST_WAIT as f64 - 2.0
                && msg.next_delivery_attempt < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "opportunistic pre-empt must defer PATH_REQUEST_WAIT (7s), got {}",
            msg.next_delivery_attempt - now
        );
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }

        // Non-opportunistic is a no-op.
        let direct = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "D",
            "hi",
            DeliveryMethod::Direct,
        )
        .unwrap();
        let mut direct = direct
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        mgr.preempt_opportunistic_path(&mut direct);
        assert_eq!(direct.next_delivery_attempt, 0.0);
    }

    /// D2: opportunistic pathless escalation (LXMRouter.py:2566-2592). With no
    /// path and delivery_attempts >= MAX_PATHLESS_TRIES, the dispatch branch
    /// requests a path (no drop), defers PATH_REQUEST_WAIT, and re-queues as
    /// "routing" instead of flooding another pathless packet.
    #[test]
    fn opportunistic_pathless_escalation_requests_path() {
        let mut mgr = test_manager();
        let dest = [0x56; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        let mut msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();
        msg.delivery_attempts = MAX_PATHLESS_TRIES;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverOpportunistic {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_PATHLESS_TRIES + 1
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "pathless escalation must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );
        // drop_existing = false for the pathless branch: RequestPath only.
        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "pathless branch must not drop the path"
        );
    }

    /// D2 Branch 2: after the first pathless deferral, a newly known path still
    /// causes Python to drop and rediscover once before resuming best-effort
    /// opportunistic sends (LXMRouter.py:2574-2583).
    #[test]
    fn opportunistic_rediscovery_without_attempt_owner_only_requests_and_defers() {
        let mut mgr = test_manager();
        let dest = [0x57; 16];
        let dest_hex = hex::encode(dest);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        mgr.known_identities
            .insert(dest_hex, Identity::new().get_public_key());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        mgr.replace_route_hops_from_path_table(&[PathTableRpcEntry {
            hash: dest,
            timestamp: now,
            via: None,
            hops: 2,
            expires: now + 60.0,
            interface: "test".to_string(),
            interface_id: 1,
            interface_mode: rns_transport::constants::InterfaceMode::Full,
            interface_role: rns_transport::messages::InterfaceRole::Normal,
        }]);

        let msg = lxmf_core::message::LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "Opp",
            "hi",
            DeliveryMethod::Opportunistic,
        )
        .unwrap();
        let mut msg = msg
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let msg_hash = msg.message_id();
        msg.delivery_attempts = MAX_PATHLESS_TRIES + 1;

        let results = mgr.execute_encrypted_actions(vec![OutboundAction::DeliverOpportunistic {
            message: msg,
            dest_hash: dest,
        }]);

        assert_eq!(results, vec![(hex::encode(msg_hash), "routing")]);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].delivery_attempts,
            MAX_PATHLESS_TRIES + 2
        );
        let nda = mgr.router.pending_outbound[0].next_delivery_attempt;
        assert!(
            nda > now + PATH_REQUEST_WAIT as f64 - 2.0
                && nda < now + PATH_REQUEST_WAIT as f64 + 2.0,
            "rediscovery branch must defer PATH_REQUEST_WAIT (7s), got {}",
            nda - now
        );

        match rx.try_recv().unwrap() {
            TransportMessage::RequestPath { destination_hash } => {
                assert_eq!(destination_hash, dest)
            }
            other => panic!("expected RequestPath, got {other:?}"),
        }
    }

    #[test]
    fn purge_identity_profile_removes_private_material_but_can_keep_files() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-purge-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let id_dir = tmp
            .join(".ratspeak")
            .join("identities")
            .join(&mgr.identity_hash);
        std::fs::create_dir_all(id_dir.join("files")).unwrap();
        std::fs::create_dir_all(id_dir.join("reticulum")).unwrap();
        std::fs::write(id_dir.join("files").join("message.bin"), b"body").unwrap();
        std::fs::write(id_dir.join("reticulum").join("config"), b"config").unwrap();
        let hub_dir = tmp.join(".ratspeak").join("channel_hub");
        std::fs::create_dir_all(&hub_dir).unwrap();
        let hub_key = hub_dir.join(format!("hub_identity_{}", mgr.identity_hash));
        std::fs::write(&hub_key, b"hubkey").unwrap();
        let other_hub_key = hub_dir.join("hub_identity_ffffffffffffffffffffffffffffffff");
        std::fs::write(&other_hub_key, b"other").unwrap();

        LxmfManager::purge_identity_profile(&tmp, &mgr.identity_hash, false).unwrap();

        assert!(!id_dir.join("identity").exists());
        assert!(!id_dir.join("reticulum").exists());
        assert!(id_dir.join("files").join("message.bin").exists());
        // A surviving hub key would resurrect the hub on its old destination
        // hash after the registry has been cascaded away.
        assert!(!hub_key.exists(), "the hub key is identity-scoped material");
        assert!(
            other_hub_key.exists(),
            "another identity's hub key must survive"
        );
    }

    #[test]
    fn purge_identity_profile_cascade_removes_profile_dir() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-purge-cascade-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let id_dir = tmp
            .join(".ratspeak")
            .join("identities")
            .join(&mgr.identity_hash);

        LxmfManager::purge_identity_profile(&tmp, &mgr.identity_hash, true).unwrap();

        assert!(!id_dir.exists());
    }

    // An identity is created with its row or not at all: a row that cannot
    // be written takes the key written before it along.
    #[test]
    fn an_imported_identity_whose_row_cannot_be_written_leaves_no_key() {
        let root = tempfile::tempdir().unwrap();
        initialize_storage_namespace(root.path()).unwrap();
        let pool = db::init_pool(root.path()).unwrap();
        db::init_schema(&pool).unwrap();
        pool.get()
            .unwrap()
            .execute_batch("DROP TABLE identities")
            .unwrap();
        let identity = Identity::new();
        let key = identity.get_private_key().unwrap();
        let ratspeak_dir = root.path().join(".ratspeak");

        let result = LxmfManager::import_identity_to_data_dir(&ratspeak_dir, &key[..], "A", &pool);

        assert!(result.is_err());
        let profile = ratspeak_dir
            .join("identities")
            .join(hex::encode(identity.hash));
        assert!(!profile.join("identity").exists());
        assert!(!profile.exists(), "the namespace it created stays");
    }

    #[test]
    fn auto_delivery_defaults_to_direct_for_user_messages() {
        let pool = test_pool();
        let mgr = test_manager();
        let dest = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Opportunistic,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Opportunistic
        );
    }

    #[test]
    fn compression_capability_does_not_choose_auto_delivery_method() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "abababababababababababababababab";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        db::touch_identity_activity(&pool, &[(dest.to_string(), now, None, None)]);
        assert!(db::set_identity_lxmf_compression_support(
            &pool,
            dest,
            db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED
        ));

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Direct,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Attachment
            ),
            DeliveryMethod::Direct
        );
        assert_eq!(
            mgr.pick_delivery_method(&pool, dest, DeliveryPreference::Auto, DeliveryProfile::Lrgp),
            DeliveryMethod::Direct
        );

        mgr.client_propagation_enabled = true;
        assert!(db::touch_identity_last_heard(&pool, dest, 1.0));
        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
    }

    #[test]
    fn persisted_no_bz2_peer_disables_direct_message_auto_compression() {
        let pool = test_pool();
        let mgr = test_manager();
        let dest = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let dest_bytes: [u8; 16] = hex::decode(dest).unwrap().try_into().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        db::touch_identity_activity(&pool, &[(dest.to_string(), now, None, None)]);
        assert!(db::set_identity_lxmf_compression_support(
            &pool,
            dest,
            db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED
        ));

        let mut msg = lxmf_core::message::LxMessageDraft::new(
            dest_bytes,
            mgr.lxmf_dest_hash,
            "",
            "manual direct",
            DeliveryMethod::Direct,
        )
        .unwrap();
        assert!(msg.auto_compress);
        mgr.apply_peer_lxmf_compression_support(&mut msg, Some(&pool), dest);
        assert!(!msg.auto_compress);
    }

    #[test]
    fn auto_delivery_starts_live_when_relay_is_configured_and_peer_is_unseen() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = Some([0xCC; 16]);
        assert!(
            !mgr.router.config.propagation_enabled,
            "client relay use must not require hosted propagation-node mode"
        );

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Attachment
            ),
            DeliveryMethod::Direct
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        db::touch_identity_activity(&pool, &[(dest.to_string(), now, None, None)]);

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Attachment
            ),
            DeliveryMethod::Direct
        );
    }

    #[test]
    fn auto_delivery_starts_live_before_relay_selection() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "bcbcbcbcbcbcbcbcbcbcbcbcbcbcbcbc";
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = None;

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct,
            "Auto must attempt live delivery without treating last-seen metadata as reachability"
        );
    }

    #[test]
    fn off_client_mode_blocks_auto_propagation_even_with_configured_node() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "cccccccccccccccccccccccccccccccc";
        mgr.client_propagation_enabled = false;
        mgr.configured_propagation_node = Some([0xDD; 16]);

        assert_eq!(
            mgr.pick_delivery_method(
                &pool,
                dest,
                DeliveryPreference::Auto,
                DeliveryProfile::Message
            ),
            DeliveryMethod::Direct
        );
    }

    #[test]
    fn disabling_client_propagation_preserves_hash_but_clears_runtime_node() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let node = [0xEE; 16];
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = Some(node);
        mgr.router.set_outbound_propagation_node(Some(node));

        mgr.enable_propagation(false, &pool, &identity_id);

        assert_eq!(mgr.configured_propagation_node, Some(node));
        assert_eq!(mgr.router.outbound_propagation_node, None);
        assert!(!mgr.client_propagation_enabled);
    }

    #[test]
    fn runtime_auto_node_does_not_replace_manual_node_preference() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        db::save_identity(&pool, &identity_id, &mgr.lxmf_hash, "Me", "Me");

        let manual = [0x12; 16];
        let auto = [0x34; 16];
        let manual_hex = hex::encode(manual);
        mgr.client_propagation_enabled = true;

        mgr.set_propagation_node(Some(&manual_hex), &pool, &identity_id);
        assert_eq!(mgr.configured_propagation_node, Some(manual));
        assert_eq!(mgr.router.outbound_propagation_node, Some(manual));

        mgr.set_runtime_propagation_node(Some(auto));
        assert_eq!(mgr.configured_propagation_node, Some(auto));
        assert_eq!(mgr.router.outbound_propagation_node, Some(auto));
        assert_eq!(
            db::get_identity(&pool, &identity_id).and_then(|v| v
                .get("propagation_node")
                .and_then(|h| h.as_str())
                .map(String::from)),
            Some(manual_hex.clone())
        );

        mgr.set_runtime_propagation_node(None);
        assert_eq!(mgr.configured_propagation_node, None);
        assert_eq!(mgr.router.outbound_propagation_node, None);
        assert_eq!(
            db::get_identity(&pool, &identity_id).and_then(|v| v
                .get("propagation_node")
                .and_then(|h| h.as_str())
                .map(String::from)),
            Some(manual_hex)
        );
    }

    #[test]
    fn propagation_status_reports_configured_node_without_sync_task() {
        let mut mgr = test_manager();
        let node = [0x44; 16];
        let node_hex = hex::encode(node);

        mgr.set_runtime_propagation_node(Some(node));
        let status = mgr.get_propagation_status();

        assert_eq!(
            status.get("propagation_node").and_then(|v| v.as_str()),
            Some(node_hex.as_str())
        );
        assert_eq!(
            status.get("sync_state").and_then(|v| v.as_str()),
            Some("disabled")
        );
    }

    #[test]
    fn auto_propagation_download_poll_requires_ready_relay() {
        let mut mgr = test_manager();
        let node = [0x55; 16];
        let node_hex = hex::encode(node);
        let (tx, _rx) = tokio::sync::mpsc::channel::<TransportMessage>(16);
        mgr.router.set_transport(tx);
        mgr.client_propagation_enabled = true;
        mgr.configured_propagation_node = Some(node);
        mgr.activate_client_propagation_node(node);
        mgr.install_download_state(
            SuppressionLedger::default(),
            AdmissionEpochs::default(),
            Vec::new(),
        );
        mgr.known_identities
            .insert(node_hex, Identity::new().get_public_key());
        mgr.last_propagation_check = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
            - AUTO_PROPAGATION_CHECK_INTERVAL_SECS
            - 1.0;

        assert!(!mgr.auto_propagation_check_due(false));
        assert!(mgr.auto_propagation_check_due(true));

        mgr.tick_with_auto_propagation_download_ready(false);
        assert_eq!(
            mgr.propagation_client.as_ref().map(|client| client.state()),
            Some(lxmf_core::propagation_client::PropagationClientState::Idle)
        );
        assert!(mgr.auto_propagation_check_due(true));

        mgr.tick_with_auto_propagation_download_ready(true);
        assert_eq!(
            mgr.propagation_client.as_ref().map(|client| client.state()),
            Some(lxmf_core::propagation_client::PropagationClientState::LinkEstablishing)
        );
    }

    #[test]
    fn enabling_client_propagation_does_not_activate_stored_hash_without_selection() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let node = [0xAB; 16];
        mgr.configured_propagation_node = Some(node);

        mgr.enable_propagation(true, &pool, &identity_id);

        assert!(mgr.client_propagation_enabled);
        assert_eq!(mgr.configured_propagation_node, Some(node));
        assert_eq!(mgr.router.outbound_propagation_node, None);
    }

    #[test]
    fn oversized_opportunistic_payload_normalizes_to_direct() {
        let mut mgr = test_manager();
        let dest = "cccccccccccccccccccccccccccccccc";
        let content = "x".repeat(ENCRYPTED_PACKET_MAX_CONTENT + 512);
        let mut msg = mgr
            .create_final_message(dest, &content, "", DeliveryMethod::Opportunistic)
            .unwrap();

        normalize_protocol_delivery_method(&mut msg);

        assert_eq!(msg.method, DeliveryMethod::Direct);
    }

    #[test]
    fn queued_report_uses_the_post_normalization_persisted_method() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let dest = "cd".repeat(16);
        let content = "x".repeat(ENCRYPTED_PACKET_MAX_CONTENT + 512);

        let queued = mgr
            .send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: &content,
                title: "",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Opportunistic,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("oversized opportunistic message should queue as Direct");

        assert_eq!(queued.method, DeliveryMethod::Direct);
        assert_eq!(
            db::get_message_delivery_method(&pool, &queued.message_id, &identity_id).as_deref(),
            Some("direct")
        );
    }

    /// Python LXMF's own `pack()` decisions on either side of the fallback to
    /// direct delivery, with no stamp, a ticket stamp and a proof of work
    /// (`tools/generate-lxmf-opportunistic-golden.py`).
    fn opportunistic_golden() -> Value {
        let golden: Value =
            serde_json::from_str(include_str!("../testdata/lxmf-opportunistic-golden.json"))
                .unwrap();
        assert_eq!(
            golden["encrypted_packet_max_content"],
            ENCRYPTED_PACKET_MAX_CONTENT
        );
        golden
    }

    /// The recipient stamp inputs of a golden case on a message being drafted.
    fn golden_stamp(golden: &Value, case: &Value) -> (Option<[u8; 16]>, Option<u8>) {
        match case["stamp"].as_str().unwrap() {
            "none" => (None, None),
            "ticket" => (Some([0x5A; 16]), None),
            "proof_of_work" => (None, Some(golden["stamp_cost"].as_u64().unwrap() as u8)),
            other => panic!("unknown stamp {other}"),
        }
    }

    #[test]
    fn the_stamp_room_is_what_finalization_attaches() {
        let ticket = Some([0x5A; 16]);
        for (outbound_ticket, stamp_cost) in [
            (None, None),
            (ticket, None),
            (None, Some(0)),
            (ticket, Some(8)),
        ] {
            let mut draft = LxMessageDraft::new(
                [0x11; 16],
                [0x22; 16],
                "",
                "hello",
                DeliveryMethod::Opportunistic,
            )
            .unwrap();
            draft.outbound_ticket = outbound_ticket;
            draft.stamp_cost = stamp_cost;
            let msg = draft
                .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                    &[7; 32],
                ))
                .unwrap();
            let predicted = OutboundMessageView::packed_len(&msg).unwrap();
            let stamp = msg.generate_recipient_stamp().unwrap();
            assert_eq!(
                msg.finalize(stamp).unwrap().packed_len().unwrap(),
                predicted,
                "ticket {outbound_ticket:?}, stamp cost {stamp_cost:?}"
            );
        }
    }

    #[test]
    fn the_opportunistic_fallback_matches_python_for_each_stamp() {
        let golden = opportunistic_golden();
        for case in golden["cases"].as_array().unwrap() {
            let length = case["content_length"].as_u64().unwrap() as usize;
            let mut draft = LxMessageDraft::new(
                [0x11; 16],
                [0x22; 16],
                golden["title"].as_str().unwrap(),
                &"x".repeat(length),
                DeliveryMethod::Opportunistic,
            )
            .unwrap();
            (draft.outbound_ticket, draft.stamp_cost) = golden_stamp(&golden, case);
            let mut msg = draft
                .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                    &[7; 32],
                ))
                .unwrap();
            assert_eq!(
                msg.packed_len().unwrap() as u64,
                case["unstamped_packed_size"].as_u64().unwrap(),
                "{case}"
            );

            normalize_protocol_delivery_method(&mut msg);

            assert_eq!(
                delivery_method_name(msg.method),
                case["method"].as_str().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn opportunistic_sends_are_recorded_as_python_sends_them() {
        // A title and content that make this manager's message, before its
        // stamp, `size` bytes on the wire.
        fn sized(mgr: &mut LxmfManager, dest: &str, size: usize) -> (String, String) {
            let mut wire = |title: usize, content: usize| {
                mgr.create_message(
                    dest,
                    &"x".repeat(content),
                    &"t".repeat(title),
                    DeliveryMethod::Opportunistic,
                )
                .unwrap()
                .packed_len()
                .unwrap()
            };
            let estimate = (200 + size).saturating_sub(wire(0, 200));
            (0..3)
                .flat_map(|title| {
                    (estimate.saturating_sub(3)..=estimate + 1).map(move |c| (title, c))
                })
                .find(|&(title, content)| wire(title, content) == size)
                .map(|(title, content)| ("t".repeat(title), "x".repeat(content)))
                .expect("a message of the golden size")
        }
        let golden = opportunistic_golden();
        let pool = test_pool();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        for case in golden["cases"].as_array().unwrap() {
            let mut mgr = test_manager();
            let identity_id = mgr.identity_hash.clone();
            let dest = [0xCE; 16];
            let dest_hex = hex::encode(dest);
            match golden_stamp(&golden, case) {
                (Some(token), _) => mgr
                    .router
                    .ticket_store
                    .remember_outbound(lxmf_core::ticket::Ticket::new(token, dest, now + 3600.0)),
                (None, Some(cost)) => mgr.router.set_stamp_cost(dest, cost),
                (None, None) => {}
            }
            let size = case["unstamped_packed_size"].as_u64().unwrap() as usize;
            let (title, content) = sized(&mut mgr, &dest_hex, size);

            let queued = mgr
                .send_message_with_preference_report(MessageSendRequest {
                    thread: "",
                    dest_hash_hex: &dest_hex,
                    content: &content,
                    title: &title,
                    db_pool: &pool,
                    identity_id: &identity_id,
                    preference: DeliveryPreference::Opportunistic,
                    profile: DeliveryProfile::Message,
                    renderer: None,
                })
                .unwrap();

            assert_eq!(
                db::get_message_delivery_method(&pool, &queued.message_id, &identity_id).as_deref(),
                case["method"].as_str(),
                "{case}"
            );
        }
    }

    #[test]
    fn reaction_sends_have_only_structured_fields_and_update_local_state() {
        let mut mgr = test_manager();
        let pool = test_pool();
        let dest = "cf".repeat(16);
        let target = "02".repeat(32);
        for (index, action) in ["add", "remove"].into_iter().enumerate() {
            assert!(mgr.send_reaction_with_preference(ReactionSendRequest {
                dest_hash_hex: &dest,
                message_id: &target,
                emoji: "🐀",
                action,
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Auto,
            }));
            let msg = &mgr.router.pending_outbound[index];
            assert!(msg.content_bytes().is_empty());
            assert!(msg.title_bytes().is_empty());
            assert_eq!(
                decode_ratspeak_chat_extension(msg),
                Some(RatspeakChatExtension::Reaction {
                    target: target.clone(),
                    emoji: "🐀".into(),
                    action: action.into(),
                })
            );
            assert_eq!(mgr.auto_live_fallback.len(), index + 1);
            let reactions = db::get_reactions_for_message(&pool, &target, "me");
            if action == "add" {
                assert_eq!(reactions.len(), 1);
                assert_eq!(reactions[0]["emoji"], "🐀");
            } else {
                assert!(reactions.is_empty());
            }
            assert!(db::get_conversation(&pool, &dest, "me", 10).is_empty());
        }
    }

    #[test]
    fn every_auto_send_builder_registers_live_fallback_after_router_admission() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let dest = "cf".repeat(16);

        let normal = mgr
            .send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "normal",
                title: "",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Auto,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("normal Auto send");
        assert_eq!(normal.method, DeliveryMethod::Direct);
        assert_eq!(mgr.auto_live_fallback.len(), 1);

        let reply = mgr
            .send_reply_with_preference(ReplyMessageSendRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "reply",
                title: "",
                reply_to_id: &"01".repeat(32),
                reply_to_preview: "original",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Auto,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("reply Auto send");
        let reply_hash: [u8; 32] = hex::decode(reply).unwrap().try_into().expect("reply hash");
        assert!(mgr.auto_live_fallback.contains(&reply_hash));
        assert_eq!(mgr.auto_live_fallback.len(), 2);

        let attachment = mgr
            .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "attachment",
                title: "",
                file_name: "proof.txt",
                file_bytes: b"attachment bytes",
                staged_path: None,
                is_image: false,
                image_mime: "application/octet-stream",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Auto,
                renderer: None,
            })
            .expect("attachment Auto send");
        assert_eq!(attachment.method, DeliveryMethod::Direct);
        assert_eq!(mgr.auto_live_fallback.len(), 3);

        let mut lrgp_fields = HashMap::new();
        lrgp_fields.insert(
            lrgp::protocol::FIELD_CUSTOM_TYPE,
            rmpv::Value::String(lrgp::protocol::PROTOCOL_TYPE.into()),
        );
        assert!(
            mgr.send_message_with_lrgp_fields_preference(
                &dest,
                "game action",
                &lrgp_fields,
                &pool,
                &identity_id,
                DeliveryPreference::Auto,
            )
            .is_some()
        );
        assert_eq!(mgr.auto_live_fallback.len(), 4);

        assert!(mgr.send_reaction_with_preference(ReactionSendRequest {
            dest_hash_hex: &dest,
            message_id: &"02".repeat(32),
            emoji: "👍",
            action: "add",
            db_pool: &pool,
            identity_id: &identity_id,
            preference: DeliveryPreference::Auto,
        }));
        assert_eq!(mgr.auto_live_fallback.len(), 5);

        assert!(
            mgr.send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "manual direct",
                title: "",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .is_ok()
        );
        assert_eq!(
            mgr.auto_live_fallback.len(),
            5,
            "explicit delivery must not inherit Auto fallback provenance"
        );

        let dest_hash: [u8; 16] = hex::decode(&dest).unwrap().try_into().unwrap();
        mgr.peer_lxmf_compression_support
            .insert(dest_hash, CompressionSupport::Unsupported);
        let no_compression = mgr
            .send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "short no-compression peer message",
                title: "",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Auto,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("no-compression Auto send");
        assert_eq!(no_compression.method, DeliveryMethod::Direct);
        assert_eq!(mgr.auto_live_fallback.len(), 6);
    }

    #[test]
    fn queued_report_uses_a_curated_failure_for_local_preparation_errors() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();

        let result = mgr.send_message_with_preference_report(MessageSendRequest {
            thread: "",
            dest_hash_hex: "not-a-destination",
            content: "hello",
            title: "",
            db_pool: &pool,
            identity_id: &identity_id,
            preference: DeliveryPreference::Direct,
            profile: DeliveryProfile::Message,
            renderer: None,
        });

        assert_eq!(result, Err(LxmfSubmissionFailure::PreparationFailed));
    }

    #[test]
    fn attachment_envelope_limit_reports_exact_encoded_size() {
        assert_eq!(
            validate_attachment_envelope_size(MAX_LXMF_RESOURCE_BYTES),
            Ok(())
        );
        assert_eq!(
            validate_attachment_envelope_size(MAX_LXMF_RESOURCE_BYTES + 1),
            Err(LxmfSubmissionFailure::ResourceLimitExceeded {
                actual_bytes: MAX_LXMF_RESOURCE_BYTES + 1,
                limit_bytes: MAX_LXMF_RESOURCE_BYTES,
            })
        );
    }

    #[test]
    fn audio_requires_full_lxmf_envelope_below_efficient_resource_size() {
        let limit = rns_protocol::resource::MAX_EFFICIENT_SIZE;
        assert_eq!(validate_audio_message_size(limit - 1), Ok(()));
        assert_eq!(
            validate_audio_message_size(limit),
            Err(LxmfSubmissionFailure::ResourceLimitExceeded {
                actual_bytes: limit,
                limit_bytes: limit,
            })
        );
    }

    #[test]
    fn the_stamp_room_matches_the_stamped_message_at_the_efficient_boundary() {
        fn message_with_audio_len(audio_len: usize) -> SignedLxMessage {
            let mut message = lxmf_core::message::LxMessageDraft::new(
                [0x11; 16],
                [0x22; 16],
                "",
                "Voice message",
                DeliveryMethod::Direct,
            )
            .unwrap();
            message
                .set_audio_field(lxmf_core::constants::AM_OPUS_OGG, &vec![0; audio_len])
                .unwrap();
            // A recipient that asks for a proof-of-work stamp.
            message.stamp_cost = Some(8);
            message
                .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                    &[7; 32],
                ))
                .unwrap()
        }

        let limit = rns_protocol::resource::MAX_EFFICIENT_SIZE;
        let mut audio_len = limit - 512;
        let at_limit = loop {
            let message = message_with_audio_len(audio_len);
            let predicted = finalized_packed_len(&message).unwrap();
            match predicted.cmp(&limit) {
                std::cmp::Ordering::Equal => break message,
                std::cmp::Ordering::Less => audio_len += limit - predicted,
                std::cmp::Ordering::Greater => audio_len -= predicted - limit,
            }
        };
        assert_eq!(
            validate_audio_message_size(finalized_packed_len(&at_limit).unwrap()),
            Err(LxmfSubmissionFailure::ResourceLimitExceeded {
                actual_bytes: limit,
                limit_bytes: limit,
            })
        );
        let stamped = at_limit
            .finalize(Some(lxmf_core::message::RecipientStamp::ProofOfWork(
                [0x44; 32],
            )))
            .unwrap();
        assert_eq!(stamped.packed_len().unwrap(), limit);

        let below = message_with_audio_len(audio_len - 1);
        assert_eq!(
            validate_audio_message_size(finalized_packed_len(&below).unwrap()),
            Ok(())
        );
        let below = below
            .finalize(Some(lxmf_core::message::RecipientStamp::ProofOfWork(
                [0x55; 32],
            )))
            .unwrap();
        assert_eq!(below.packed_len().unwrap(), limit - 1);
    }

    #[test]
    fn propagated_submission_without_a_router_node_leaves_no_sending_history() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let identity_id = mgr.identity_hash.clone();
        let dest = "ce".repeat(16);

        let result = mgr.send_message_with_method(
            &dest,
            "hello",
            "",
            &pool,
            &identity_id,
            DeliveryMethod::Propagated,
        );

        assert!(result.is_none());
        assert!(db::get_conversation(&pool, &dest, &identity_id, 100).is_empty());
    }

    #[tokio::test]
    async fn tick_drains_deferred_stamps_before_outbound_delivery() {
        let mut mgr = test_manager();
        let dest = [0xDD; 16];
        let dest_hex = hex::encode(dest);
        let remote = Identity::new();
        mgr.known_identities
            .insert(dest_hex.clone(), remote.get_public_key());
        mgr.router.set_stamp_cost(dest, 1);

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);
        let (proof_tx, mut proof_rx) = tokio::sync::mpsc::unbounded_channel();
        mgr.set_opportunistic_proof_sender(proof_tx);

        let message = mgr
            .create_message(&dest_hex, "needs stamp", "", DeliveryMethod::Opportunistic)
            .expect("message created");
        mgr.router.ticket_store.replace_legacy(Vec::new());
        let msg_id = hex::encode(message.message_id());
        mgr.router.submit_signed(message).unwrap();

        assert_eq!(mgr.router.pending_deferred_stamps.len(), 1);

        let states = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut states = Vec::new();
            loop {
                states.extend(mgr.tick());
                if states
                    .iter()
                    .any(|(id, state)| id == &msg_id && *state == "sent")
                {
                    break states;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("deferred stamp should complete on a loaded release runner");

        assert!(mgr.router.pending_deferred_stamps.is_empty());
        assert!(
            states
                .iter()
                .any(|(id, state)| id == &msg_id && *state == "sent"),
            "tick should move deferred stamped messages into outbound processing"
        );
        let (receipt, result_tx) = match rx.try_recv().unwrap() {
            TransportMessage::SendPacket {
                receipt: Some(receipt),
                result_tx,
                ..
            } => (receipt, result_tx),
            _ => panic!("expected atomically tracked packet"),
        };
        result_tx
            .send(rns_transport::messages::OutboundDispatchResult::Sent)
            .unwrap();
        receipt
            .status_tx
            .send_replace(rns_transport::messages::ReceiptUpdate::Delivered {
                rtt: Duration::from_millis(4),
            });
        assert!(rx.try_recv().is_err());
        assert!(
            proof_rx.try_recv().is_err(),
            "new packets do not use unscoped message-id proof events"
        );
        assert_eq!(mgr.opportunistic_in_flight.len(), 1);
        let hash: [u8; 32] = hex::decode(&msg_id).unwrap().try_into().unwrap();
        mgr.auto_live_fallback.insert(hash);
        assert!(mgr.complete_opportunistic_delivery(&msg_id));
        assert!(mgr.opportunistic_in_flight.is_empty());
        assert!(
            !mgr.auto_live_fallback.contains(&hash),
            "authenticated Opportunistic proof must end the Auto fallback lifecycle"
        );
        assert!(
            !mgr.complete_opportunistic_delivery(&msg_id),
            "duplicate proof completion must be ignored"
        );
        assert!(
            !mgr.complete_opportunistic_delivery(&"ff".repeat(32)),
            "an unmatched proof must not complete another message"
        );
    }

    #[test]
    fn lxmf_announce_app_data_caches_stamp_costs_by_aspect() {
        let mut mgr = test_manager();
        let delivery_dest = [0x11; 16];
        let propagation_dest = [0x22; 16];
        let unrelated_dest = [0x33; 16];

        let delivery_data = lxmf_core::handlers::get_announce_app_data(Some("peer"), Some(7));
        assert!(mgr.update_lxmf_announce_app_data(
            delivery_dest,
            rns_identity::name_hash::name_hash("lxmf.delivery"),
            Some(&delivery_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&delivery_dest), Some(7));
        assert!(!mgr.update_lxmf_announce_app_data(
            delivery_dest,
            rns_identity::name_hash::name_hash("lxmf.delivery"),
            Some(&delivery_data),
        ));

        let pn_data =
            lxmf_core::handlers::PropagationNodeAnnounceData::new(true, 1024, 1024, 23, 3, 0);
        let pn_app_data = lxmf_core::handlers::get_propagation_node_app_data(&pn_data);
        assert!(mgr.update_lxmf_announce_app_data(
            propagation_dest,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&pn_app_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&propagation_dest), Some(23));
        assert_eq!(
            mgr.propagation_transfer_limits_kb.get(&propagation_dest),
            Some(&1024)
        );
        // Even unchanged cost/limit renew the durable policy's announce time.
        assert!(mgr.update_lxmf_announce_app_data(
            propagation_dest,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&pn_app_data),
        ));

        let smaller_pn_data =
            lxmf_core::handlers::PropagationNodeAnnounceData::new(true, 1, 1, 23, 3, 0);
        assert!(mgr.update_lxmf_announce_app_data(
            propagation_dest,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&lxmf_core::handlers::get_propagation_node_app_data(
                &smaller_pn_data,
            )),
        ));
        assert_eq!(
            mgr.propagation_transfer_limits_kb.get(&propagation_dest),
            Some(&1)
        );

        assert!(!mgr.update_lxmf_announce_app_data(
            unrelated_dest,
            rns_identity::name_hash::name_hash("nomadnetwork.node"),
            Some(&delivery_data),
        ));
        assert_eq!(mgr.router.get_stamp_cost(&unrelated_dest), None);
    }

    #[tokio::test]
    async fn propagated_send_fails_locally_above_announced_decimal_limit() {
        let mut mgr = test_manager();
        let destination = Identity::new();
        let relay = Identity::new();
        let destination_hash = [0x31; 16];
        let relay_hash = [0x32; 16];
        mgr.known_identities
            .insert(hex::encode(destination_hash), destination.get_public_key());
        mgr.known_identities
            .insert(hex::encode(relay_hash), relay.get_public_key());

        let (tx, _rx) = mpsc::channel(16);
        mgr.router.set_transport(tx);
        ready_relay_policy(&mut mgr, relay_hash, 0, 0);
        let announce = lxmf_core::handlers::PropagationNodeAnnounceData::new(true, 0, 0, 0, 0, 0);
        assert!(mgr.update_lxmf_announce_app_data(
            relay_hash,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&lxmf_core::handlers::get_propagation_node_app_data(
                &announce,
            )),
        ));

        let message = mgr
            .create_final_message(
                &hex::encode(destination_hash),
                "cannot fit in a zero-byte relay transfer",
                "",
                DeliveryMethod::Propagated,
            )
            .unwrap();
        mgr.router.set_outbound_propagation_node(Some(relay_hash));
        let message_id = hex::encode(message.message_id());
        let mut results = Vec::new();
        mgr.start_propagation_delivery(message, relay_hash, &mut results);

        let results = finish_pending_propagation(&mut mgr, results).await;
        assert_eq!(results, vec![(message_id.clone(), "failed")]);
        let failures = mgr.take_delivery_failure_updates();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].msg_id, message_id);
        assert_eq!(failures[0].code, "propagation_limit_exceeded");
        assert!(failures[0].actual_bytes > 0);
        assert_eq!(failures[0].limit_bytes, 0);
        assert!(mgr.link_delivery.is_none());
    }

    #[test]
    fn lxmf_announce_app_data_caches_explicit_compression_support() {
        let mut mgr = test_manager();
        let dest = [0x44; 16];
        let delivery_name_hash = rns_identity::name_hash::name_hash("lxmf.delivery");

        // <=0.9.8 peer announce: msgpack [b"peer", nil] — 2 elements, no
        // feature list (get_announce_app_data emits the 3-element 1.0.1 form).
        let legacy_data = vec![0x92, 0xC4, 0x04, 0x70, 0x65, 0x65, 0x72, 0xC0];
        assert!(!mgr.update_lxmf_announce_app_data(dest, delivery_name_hash, Some(&legacy_data),));
        assert_eq!(
            mgr.peer_lxmf_compression_support.get(&dest),
            None,
            "legacy app_data must not be treated as an explicit constrained-peer signal"
        );

        let no_bz2_data = delivery_app_data_with_features(&[]);
        assert!(mgr.update_lxmf_announce_app_data(dest, delivery_name_hash, Some(&no_bz2_data),));
        assert_eq!(
            mgr.peer_lxmf_compression_support.get(&dest),
            Some(&CompressionSupport::Unsupported)
        );
        assert!(!mgr.update_lxmf_announce_app_data(dest, delivery_name_hash, Some(&legacy_data),));
        assert_eq!(
            mgr.peer_lxmf_compression_support.get(&dest),
            Some(&CompressionSupport::Unsupported),
            "unknown metadata should not erase explicit capability"
        );

        let supported_data =
            delivery_app_data_with_features(&[lxmf_core::constants::SF_COMPRESSION]);
        assert!(
            mgr.update_lxmf_announce_app_data(dest, delivery_name_hash, Some(&supported_data),)
        );
        assert_eq!(
            mgr.peer_lxmf_compression_support.get(&dest),
            Some(&CompressionSupport::Supported)
        );
    }

    #[test]
    fn load_or_create_restores_lxmf_router_state() {
        let unique = TEMP_LXMF_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-lxmf-router-state-test-{}-{}-{unique}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        initialize_storage_namespace(&tmp).unwrap();
        let mut mgr = LxmfManager::load_or_create(&tmp, None, None).unwrap();
        let identity_hash = mgr.identity_hash.clone();
        let propagation_node = [0x66; 16];
        let remote = Identity::new();

        mgr.router.set_stamp_cost(propagation_node, 19);
        mgr.known_identities
            .insert(hex::encode(propagation_node), remote.get_public_key());
        mgr.propagation_transfer_limits_kb
            .insert(propagation_node, 256);
        mgr.save_crypto_state().unwrap();
        drop(mgr);

        let mut restored = LxmfManager::load_or_create(&tmp, Some(&identity_hash), None).unwrap();
        assert_eq!(restored.router.get_stamp_cost(&propagation_node), Some(19));
        assert!(
            !restored.propagation_node_ready_for_send(&propagation_node),
            "restart must wait for a fresh transfer-limit announce instead of bypassing admission"
        );
        let announce =
            lxmf_core::handlers::PropagationNodeAnnounceData::new(true, 256, 10240, 19, 0, 0);
        assert!(restored.update_lxmf_announce_app_data(
            propagation_node,
            rns_identity::name_hash::name_hash("lxmf.propagation"),
            Some(&lxmf_core::handlers::get_propagation_node_app_data(
                &announce,
            )),
        ));
        assert!(
            !restored.propagation_node_ready_for_send(&propagation_node),
            "announce alone does not establish a usable node path"
        );
        ready_relay_policy(&mut restored, propagation_node, 19, 256);
        assert!(restored.propagation_node_ready_for_send(&propagation_node));
    }

    #[test]
    fn propagated_delivery_waits_for_propagation_node_stamp_cost() {
        let mut mgr = test_manager();
        let propagation_node = [0x44; 16];
        let recipient_dest = [0x55; 16];
        let prop_hex = hex::encode(propagation_node);
        let recipient_hex = hex::encode(recipient_dest);
        let prop_identity = Identity::new();
        let recipient_identity = Identity::new();

        mgr.known_identities
            .insert(prop_hex, prop_identity.get_public_key());
        mgr.known_identities
            .insert(recipient_hex.clone(), recipient_identity.get_public_key());
        mgr.router
            .set_outbound_propagation_node(Some(propagation_node));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let message = mgr
            .create_final_message(
                &recipient_hex,
                "relay once cost is known",
                "",
                DeliveryMethod::Propagated,
            )
            .expect("message created");
        let message_id = message.message_id();
        mgr.router.try_send(message).unwrap();

        let states = mgr.tick();
        assert!(states.is_empty());

        match rx.try_recv() {
            Ok(TransportMessage::RequestPath { destination_hash }) => {
                assert_eq!(destination_hash, propagation_node);
            }
            other => panic!("expected propagation-node path request, got {other:?}"),
        }
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|msg| msg.message_id() == message_id),
            "message should be requeued until propagation-node stamp cost is learned"
        );
        let queued = mgr
            .router
            .pending_outbound
            .iter()
            .find(|msg| msg.message_id() == message_id)
            .expect("message requeued");
        assert_eq!(
            queued.delivery_attempts, 0,
            "waiting for relay metadata must not consume delivery attempts"
        );
        assert!(
            queued.last_delivery_attempt > 0.0,
            "metadata waits still need retry backoff"
        );
    }

    #[test]
    fn propagated_delivery_waits_for_recipient_identity_key() {
        let mut mgr = test_manager();
        let propagation_node = [0x44; 16];
        let recipient_dest = [0x77; 16];
        let prop_hex = hex::encode(propagation_node);
        let recipient_hex = hex::encode(recipient_dest);
        let prop_identity = Identity::new();

        mgr.known_identities
            .insert(prop_hex, prop_identity.get_public_key());
        ready_relay_policy(&mut mgr, propagation_node, 19, 256);
        mgr.router.set_stamp_cost(propagation_node, 19);
        mgr.propagation_transfer_limits_kb
            .insert(propagation_node, 256);
        mgr.router
            .set_outbound_propagation_node(Some(propagation_node));

        let (tx, mut rx) = tokio::sync::mpsc::channel::<TransportMessage>(8);
        mgr.router.set_transport(tx);

        let message = mgr
            .create_final_message(
                &recipient_hex,
                "wait until recipient identity is known",
                "",
                DeliveryMethod::Propagated,
            )
            .expect("message created");
        let message_id = message.message_id();
        mgr.router.try_send(message).unwrap();

        let states = mgr.tick();
        assert!(states.is_empty());

        match rx.try_recv() {
            Ok(TransportMessage::RequestPath { destination_hash }) => {
                assert_eq!(destination_hash, recipient_dest);
            }
            other => panic!("expected recipient path request, got {other:?}"),
        }
        assert!(
            mgr.router
                .pending_outbound
                .iter()
                .any(|msg| msg.message_id() == message_id),
            "message should be requeued until recipient identity key is learned"
        );
        let queued = mgr
            .router
            .pending_outbound
            .iter()
            .find(|msg| msg.message_id() == message_id)
            .expect("message requeued");
        assert_eq!(
            queued.delivery_attempts, 0,
            "waiting for recipient identity metadata must not consume delivery attempts"
        );
        assert!(
            queued.last_delivery_attempt > 0.0,
            "identity waits still need retry backoff"
        );
        assert!(
            mgr.link_delivery.is_none(),
            "propagation link must not start until the message can be encrypted for the recipient"
        );
    }

    #[test]
    fn attempt_exhausted_outbound_surfaces_failed_state() {
        let mut mgr = test_manager();
        let dest_hex = hex::encode([0x88; 16]);
        let mut message = mgr
            .create_final_message(
                &dest_hex,
                "delivery attempts exhausted",
                "",
                DeliveryMethod::Direct,
            )
            .expect("message created");
        let message_id = message.message_id();
        message.delivery_attempts = u32::MAX;
        mgr.router.try_send(message).unwrap();

        let states = mgr.tick();

        assert_eq!(states, vec![(hex::encode(message_id), "failed")]);
        assert!(
            mgr.router.pending_outbound.is_empty(),
            "failed outbound messages should not stay queued indefinitely"
        );
    }

    #[test]
    fn expired_auto_live_send_does_not_fall_back_to_offline_inbox() {
        let mut mgr = test_manager();
        let dest = [0x89; 16];
        let node = [0x8A; 16];
        let _transport_rx = ready_auto_propagation_fallback(&mut mgr, dest, node);
        let mut draft = LxMessageDraft::new(
            dest,
            mgr.lxmf_dest_hash,
            "",
            "expired Auto send",
            DeliveryMethod::Direct,
        )
        .unwrap();
        draft.set_timestamp(0.0).unwrap();
        let message = draft
            .sign(&mgr.identity.get_signing_key().unwrap())
            .unwrap()
            .finalize(None)
            .unwrap();
        let message_id = message.message_id();
        mgr.router.try_send(message).unwrap();
        mgr.auto_live_fallback.insert(message_id);

        let states = mgr.tick();

        assert_eq!(states, vec![(hex::encode(message_id), "failed")]);
        assert!(!mgr.in_flight_propagation.contains_key(&message_id));
        assert!(!mgr.auto_live_fallback.contains(&message_id));
    }

    fn renderer_test_message(field: Option<lxmf_core::message::FieldValue>) -> LxMessage {
        let mut draft =
            LxMessageDraft::new([3; 16], [4; 16], "", "**hi**", DeliveryMethod::Direct).unwrap();
        if let Some(value) = field {
            draft
                .set_field_value(lxmf_core::constants::FIELD_RENDERER, value)
                .unwrap();
        }
        draft
            .sign(&rns_crypto::ed25519::Ed25519PrivateKey::from_bytes(
                &[7; 32],
            ))
            .unwrap()
            .finalize(None)
            .unwrap()
    }

    #[test]
    fn inbound_renderer_accepts_only_unsigned_msgpack_integers() {
        for (encoded, expected) in [
            (&[0x02][..], Some(2)),
            (&[0x00][..], Some(0)),
            (&[0xcc, 0xff][..], Some(255)),
            (&[0xcd, 0x00, 0x02][..], Some(2)),
            (&[0xd0, 0x02][..], Some(2)),
            (&[0xcd, 0x01, 0x00][..], None),
            (&[0xff][..], None),
            (&[0xd0, 0xfe][..], None),
            (&[0xc4, 0x01, 0x02][..], None),
            (&[0xa1, b'2'][..], None),
            (&[0xca, 0x40, 0x00, 0x00, 0x00][..], None),
            (&[0xc0][..], None),
            (&[0x02, 0x02][..], None),
            (&[][..], None),
        ] {
            assert_eq!(renderer_from_encoded(encoded), expected, "{encoded:02x?}");
        }

        let native = lxmf_core::message::FieldValue::MessagePack(
            lxmf_core::message::EncodedFieldValue::new(Bytes::from_static(&[0x02])).unwrap(),
        );
        assert_eq!(
            message_renderer(&renderer_test_message(Some(native))),
            Some(2)
        );
        let bin = lxmf_core::message::FieldValue::Binary(Bytes::from_static(&[0x02]));
        assert_eq!(message_renderer(&renderer_test_message(Some(bin))), None);
        let text = lxmf_core::message::FieldValue::MessagePack(
            lxmf_core::message::EncodedFieldValue::new(Bytes::from_static(&[0xa1, b'2'])).unwrap(),
        );
        assert_eq!(message_renderer(&renderer_test_message(Some(text))), None);
        assert_eq!(message_renderer(&renderer_test_message(None)), None);
    }

    /// Python LXMF `pack()` of a Markdown message
    /// (`tools/generate-lxmf-renderer-golden.py`).
    #[test]
    fn renderer_field_matches_python_lxmf_both_ways() {
        let golden: Value =
            serde_json::from_str(include_str!("../testdata/lxmf-renderer-golden.json")).unwrap();
        let packed = hex::decode(golden["packed"].as_str().unwrap()).unwrap();
        let public_key = hex::decode(golden["source_public_key"].as_str().unwrap()).unwrap();
        let signer = rns_crypto::ed25519::Ed25519PublicKey::from_bytes(
            &public_key[32..].try_into().unwrap(),
        )
        .unwrap();

        let message = LxMessage::unpack_owned(
            Bytes::from(packed.clone()),
            lxmf_core::message_api::MessageDecodeLimits::default(),
        )
        .unwrap();
        assert!(message.verify_signature(&signer));
        assert_eq!(message_renderer(&message), Some(2));

        let content = golden["content"].as_str().unwrap();
        let mut draft = LxMessageDraft::new(
            packed[..16].try_into().unwrap(),
            packed[16..32].try_into().unwrap(),
            "",
            content,
            DeliveryMethod::Direct,
        )
        .unwrap();
        draft
            .set_timestamp(golden["timestamp"].as_f64().unwrap())
            .unwrap();
        let (field_id, bytes) = renderer_custom_field(lxmf_core::constants::RENDERER_MARKDOWN);
        draft.set_msgpack_field(field_id, bytes).unwrap();
        assert_eq!(draft.pack_payload().unwrap(), packed[96..]);
    }

    /// Python LXMF `pack()` of a message in a thread
    /// (`tools/generate-lxmf-thread-golden.py`).
    #[test]
    fn thread_field_matches_python_lxmf_both_ways() {
        let golden: Value =
            serde_json::from_str(include_str!("../testdata/lxmf-thread-golden.json")).unwrap();
        let packed = hex::decode(golden["packed"].as_str().unwrap()).unwrap();
        let thread = golden["thread"].as_str().unwrap();
        let message = LxMessage::unpack_owned(
            Bytes::from(packed.clone()),
            lxmf_core::message_api::MessageDecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(message_thread(&message), thread);

        let mut draft = LxMessageDraft::new(
            packed[..16].try_into().unwrap(),
            packed[16..32].try_into().unwrap(),
            "",
            golden["content"].as_str().unwrap(),
            DeliveryMethod::Direct,
        )
        .unwrap();
        draft
            .set_timestamp(golden["timestamp"].as_f64().unwrap())
            .unwrap();
        let (field_id, bytes) = thread_custom_field(thread).unwrap();
        draft.set_field(field_id, bytes).unwrap();
        assert_eq!(draft.pack_payload().unwrap(), packed[96..]);
        assert_eq!(thread_custom_field(""), None);
    }

    #[test]
    fn thread_sends_carry_the_field_and_file_the_row_in_its_conversation() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let thread = "5a".repeat(32);
        db::create_conversation_thread(&pool, "me", dest, &thread, Some("Trip plan")).unwrap();
        let queued = mgr
            .send_message_with_preference_report(MessageSendRequest {
                dest_hash_hex: dest,
                content: "in a thread",
                title: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: None,
                thread: &thread,
            })
            .expect("thread message queued");
        let sent = mgr
            .router
            .pending_outbound
            .iter()
            .find(|msg| hex::encode(msg.message_id()) == queued.message_id)
            .expect("queued message");
        assert_eq!(message_thread(sent), thread);
        let rows = db::get_conversation_in_thread(&pool, dest, &thread, "me", 10);
        assert_eq!(rows.len(), 1);
        assert!(db::get_conversation_in_thread(&pool, dest, "", "me", 10).is_empty());
        assert_eq!(db::message_thread(&pool, &queued.message_id, "me"), thread);
    }

    #[test]
    fn markdown_sends_pack_renderer_as_native_int_and_persist_it() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let reply_target = "01".repeat(32);

        let text = mgr
            .send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: dest,
                content: "**bold**",
                title: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: Some(lxmf_core::constants::RENDERER_MARKDOWN),
            })
            .expect("text queued")
            .message_id;
        let reply = mgr
            .send_reply_with_preference(ReplyMessageSendRequest {
                thread: "",
                dest_hash_hex: dest,
                content: "_re_",
                title: "",
                reply_to_id: &reply_target,
                reply_to_preview: "original",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: Some(lxmf_core::constants::RENDERER_MARKDOWN),
            })
            .expect("reply queued");
        let attachment = mgr
            .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                thread: "",
                dest_hash_hex: dest,
                content: "`file`\n[File: note.txt]",
                title: "",
                file_name: "note.txt",
                file_bytes: b"hello",
                staged_path: None,
                is_image: false,
                image_mime: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                renderer: Some(lxmf_core::constants::RENDERER_MARKDOWN),
            })
            .expect("attachment queued")
            .message_id;
        let plain = mgr
            .send_message_with_preference_report(MessageSendRequest {
                thread: "",
                dest_hash_hex: dest,
                content: "plain",
                title: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                profile: DeliveryProfile::Message,
                renderer: None,
            })
            .expect("plain queued")
            .message_id;

        for id in [&text, &reply, &attachment] {
            let message = mgr
                .router
                .pending_outbound
                .iter()
                .find(|message| hex::encode(message.message_id()) == *id)
                .expect("pending outbound");
            let field = message
                .get_field(lxmf_core::constants::FIELD_RENDERER)
                .expect("renderer field");
            assert_eq!(field.encoded(), &[0x02]);
            assert!(
                message
                    .wire_bytes()
                    .windows(2)
                    .any(|pair| pair == [0x0f, 0x02])
            );
            assert_eq!(message_renderer(message), Some(2));
        }
        let message = mgr
            .router
            .pending_outbound
            .iter()
            .find(|message| hex::encode(message.message_id()) == plain)
            .expect("plain pending outbound");
        assert!(
            message
                .get_field(lxmf_core::constants::FIELD_RENDERER)
                .is_none()
        );

        let rows = db::get_conversation(&pool, dest, "me", 10);
        let renderer = |id: &str| {
            rows.iter()
                .find(|row| row["id"] == id)
                .map(|row| row["renderer"].clone())
                .expect("stored row")
        };
        for id in [&text, &reply, &attachment] {
            assert_eq!(renderer(id), 2);
        }
        assert!(renderer(&plain).is_null());
    }

    #[test]
    fn attachment_field_uses_lxmf_string_filename_and_binary_bytes() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

        let queued = mgr
            .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                thread: "",
                dest_hash_hex: dest,
                content: "file attached",
                title: "",
                file_name: "note.txt",
                file_bytes: b"hello",
                staged_path: None,
                is_image: false,
                image_mime: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                renderer: None,
            })
            .expect("message queued");
        assert_eq!(queued.method, DeliveryMethod::Direct);

        let message = mgr
            .router
            .pending_outbound
            .first()
            .expect("direct attachment should be pending outbound");
        let field = message
            .get_field(lxmf_core::constants::FIELD_FILE_ATTACHMENTS)
            .expect("attachment field");
        let value = rmpv::decode::read_value(&mut std::io::Cursor::new(field.encoded())).unwrap();
        let attachment = value.as_array().unwrap()[0].as_array().unwrap();

        assert_eq!(attachment[0].as_str(), Some("note.txt"));
        assert_eq!(attachment[1].as_slice(), Some(&b"hello"[..]));
    }

    #[cfg(feature = "lxst-voice")]
    #[test]
    fn semantic_audio_send_uses_only_field_audio_and_first_class_storage() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "da".repeat(16);
        let audio_bytes = valid_ogg_opus_fixture();

        let queued = mgr
            .send_audio_message_with_preference_report(AudioMessageRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "",
                title: "",
                audio_mode: lxmf_core::constants::AM_OPUS_OGG,
                audio_bytes: &audio_bytes,
                staged_path: None,
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
            })
            .expect("audio message queued");
        assert_eq!(queued.method, DeliveryMethod::Direct);

        let message = mgr.router.pending_outbound.first().unwrap();
        let audio = message.audio_field().unwrap().unwrap();
        assert_eq!(audio.mode, lxmf_core::constants::AM_OPUS_OGG);
        assert_eq!(audio.bytes, audio_bytes);
        assert!(
            !message
                .fields()
                .contains(lxmf_core::constants::FIELD_FILE_ATTACHMENTS)
        );

        let conversation = db::get_conversation(&pool, &dest, "me", 10);
        let row = conversation
            .iter()
            .find(|message| message["id"] == queued.message_id)
            .unwrap();
        assert_eq!(row["audio"]["mode"], lxmf_core::constants::AM_OPUS_OGG);
        assert_eq!(row["audio"]["supported"], true);
        assert_eq!(row["content"], "Voice message");
        assert!(row["attachments"].is_null());
        let stored = row["audio"]["stored_name"].as_str().unwrap();
        assert_eq!(
            std::fs::read(mgr.get_received_file(stored).unwrap()).unwrap(),
            audio_bytes
        );
    }

    #[test]
    fn voice_format_uses_only_explicit_live_or_cached_no_compression() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "de".repeat(16);
        let destination = [0xde; 16];
        assert_eq!(mgr.voice_message_format(&pool, &dest), 16);
        db::set_voice_message_format(&pool, "me", &dest, 3).unwrap();
        assert_eq!(
            mgr.voice_message_format(&pool, &dest),
            16,
            "legacy customization is ignored"
        );
        db::touch_identity_activity(&pool, &[(dest.clone(), 1.0, None, None)]);
        assert!(db::set_identity_lxmf_compression_support(
            &pool,
            &dest,
            db::LXMF_COMPRESSION_SUPPORT_UNSUPPORTED
        ));
        assert_eq!(
            mgr.voice_message_format(&pool, &dest),
            3,
            "restored explicit no-compression"
        );
        mgr.peer_lxmf_compression_support
            .insert(destination, CompressionSupport::Supported);
        assert_eq!(
            mgr.voice_message_format(&pool, &dest),
            16,
            "live support overrides cache"
        );
        mgr.peer_lxmf_compression_support
            .insert(destination, CompressionSupport::Unknown);
        assert_eq!(mgr.voice_message_format(&pool, &dest), 16);
        mgr.peer_lxmf_compression_support
            .insert(destination, CompressionSupport::Unsupported);
        assert_eq!(mgr.voice_message_format(&pool, &dest), 3);
        assert_eq!(
            mgr.voice_message_format(&pool, &"ab".repeat(16)),
            16,
            "another destination is independent"
        );
    }

    #[cfg(feature = "lxst-voice")]
    #[test]
    fn compact_audio_preserves_recorded_format_after_discovery_changes() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "de".repeat(16);
        let audio = vec![0x42; crate::voice_memo::compact::RECORD_BYTES];
        let submit = |mgr: &mut LxmfManager, peer: &str, body: &str, bytes: &[u8]| {
            mgr.send_audio_message_with_preference_report(AudioMessageRequest {
                thread: "",
                dest_hash_hex: peer,
                content: body,
                title: "",
                audio_mode: 3,
                audio_bytes: bytes,
                staged_path: None,
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
            })
        };
        let destination: [u8; 16] = hex::decode(&dest).unwrap().try_into().unwrap();
        mgr.peer_lxmf_compression_support
            .insert(destination, CompressionSupport::Unsupported);
        assert_eq!(mgr.voice_message_format(&pool, &dest), 3);
        // This recording was made compact. Later positive support changes only
        // future recordings, never the bytes currently being reviewed/retried.
        mgr.peer_lxmf_compression_support
            .insert(destination, CompressionSupport::Supported);
        assert_eq!(mgr.voice_message_format(&pool, &dest), 16);
        assert_eq!(
            submit(&mut mgr, &dest, "", &audio[..1499]),
            Err(LxmfSubmissionFailure::PreparationFailed)
        );
        assert_eq!(
            submit(&mut mgr, &dest, "", &vec![0; 1504]),
            Err(LxmfSubmissionFailure::PreparationFailed)
        );
        let queued = submit(&mut mgr, &dest, "", &audio).unwrap();
        let message = mgr.router.pending_outbound.first().unwrap();
        let field = message.audio_field().unwrap().unwrap();
        assert_eq!(field.mode, 3);
        assert_eq!(field.bytes, audio);
        assert!(message.packed_len().unwrap() <= 3659);
        let rows = db::get_conversation(&pool, &dest, "me", 10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], queued.message_id);
        assert_eq!(rows[0]["audio"]["mode"], 3);
        assert_eq!(rows[0]["audio"]["supported"], true);
        let name = rows[0]["audio"]["stored_name"].as_str().unwrap();
        assert_eq!(db::stored_audio_mode(&pool, "me", name).unwrap(), 3);
        assert_eq!(
            std::fs::read(mgr.get_received_file(name).unwrap()).unwrap(),
            audio
        );
        db::set_voice_message_format(&pool, "me", &dest, 16).unwrap();
        assert_eq!(db::get_conversation(&pool, &dest, "me", 10).len(), 1);
    }

    #[cfg(feature = "lxst-voice")]
    #[test]
    fn invalid_audio_is_rejected_before_storage_or_router_admission() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "db".repeat(16);

        let result = mgr.send_audio_message_with_preference_report(AudioMessageRequest {
            thread: "",
            dest_hash_hex: &dest,
            content: "Voice message",
            title: "",
            audio_mode: lxmf_core::constants::AM_OPUS_OGG,
            audio_bytes: b"not an Ogg stream",
            staged_path: None,
            db_pool: &pool,
            identity_id: "me",
            preference: DeliveryPreference::Direct,
        });

        assert_eq!(result, Err(LxmfSubmissionFailure::PreparationFailed));
        assert!(mgr.router.pending_outbound.is_empty());
        assert!(db::get_conversation(&pool, &dest, "me", 10).is_empty());
        assert_eq!(std::fs::read_dir(mgr.files_dir()).unwrap().count(), 0);
    }

    #[cfg(feature = "lxst-voice")]
    #[test]
    fn staged_audio_is_adopted_and_queue_failure_rolls_back_database_and_file() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "dc".repeat(16);
        let audio_bytes = valid_ogg_opus_fixture();
        let staging = mgr.data_dir.join("private-audio-staging-test");
        std::fs::write(&staging, &audio_bytes).unwrap();

        let result = mgr.send_audio_message_with_preference_report(AudioMessageRequest {
            thread: "",
            dest_hash_hex: &dest,
            content: "Voice message",
            title: "",
            audio_mode: lxmf_core::constants::AM_OPUS_OGG,
            audio_bytes: &audio_bytes,
            staged_path: Some(&staging),
            db_pool: &pool,
            identity_id: "me",
            preference: DeliveryPreference::Propagated,
        });

        assert_eq!(result, Err(LxmfSubmissionFailure::PreparationFailed));
        assert!(
            !staging.exists(),
            "runtime must consume the adopted staging file"
        );
        assert!(db::get_conversation(&pool, &dest, "me", 10).is_empty());
        assert_eq!(std::fs::read_dir(mgr.files_dir()).unwrap().count(), 0);
    }

    #[test]
    fn staged_attachment_is_atomically_adopted_without_a_second_file_copy() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "ab".repeat(16);
        let staging = mgr.data_dir.join("private-staging-test");
        std::fs::write(&staging, b"staged bytes").unwrap();

        let queued = mgr
            .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "staged file",
                title: "",
                file_name: "staged.txt",
                file_bytes: b"staged bytes",
                staged_path: Some(&staging),
                is_image: false,
                image_mime: "",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Direct,
                renderer: None,
            })
            .expect("staged message queued");

        assert!(!staging.exists());
        let conversation = db::get_conversation(&pool, &dest, "me", 10);
        let stored = conversation
            .iter()
            .find(|message| {
                message.get("id").and_then(|value| value.as_str()) == Some(&queued.message_id)
            })
            .and_then(|message| message.get("attachments"))
            .and_then(|attachments| attachments.as_array())
            .and_then(|attachments| attachments.first())
            .and_then(|attachment| attachment.get("stored_name"))
            .and_then(|value| value.as_str())
            .expect("stored attachment name");
        let path = mgr.get_received_file(stored).expect("adopted file");
        assert_eq!(std::fs::read(path).unwrap(), b"staged bytes");
    }

    #[test]
    fn rejected_attachment_queue_rolls_back_history_and_durable_file() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "bc".repeat(16);

        let result =
            mgr.send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                thread: "",
                dest_hash_hex: &dest,
                content: "must roll back",
                title: "",
                file_name: "rollback.bin",
                file_bytes: b"attachment bytes",
                staged_path: None,
                is_image: false,
                image_mime: "application/octet-stream",
                db_pool: &pool,
                identity_id: "me",
                preference: DeliveryPreference::Propagated,
                renderer: None,
            });

        assert_eq!(result, Err(LxmfSubmissionFailure::PreparationFailed));
        assert!(db::get_conversation(&pool, &dest, "me", 10).is_empty());
        assert_eq!(
            std::fs::read_dir(mgr.files_dir())
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            0
        );
    }

    fn send_for_resume(
        mgr: &mut LxmfManager,
        pool: &DbPool,
        dest: &str,
        preference: DeliveryPreference,
    ) -> [u8; 32] {
        let identity_id = mgr.identity_hash.clone();
        let queued = mgr
            .send_message_with_preference_report(MessageSendRequest {
                dest_hash_hex: dest,
                content: "outlives a restart",
                title: "",
                db_pool: pool,
                identity_id: &identity_id,
                preference,
                profile: DeliveryProfile::Message,
                renderer: None,
                thread: "",
            })
            .unwrap();
        hex::decode(queued.message_id).unwrap().try_into().unwrap()
    }

    fn reloaded(mgr: LxmfManager) -> LxmfManager {
        let root = mgr.data_dir.parent().unwrap().to_path_buf();
        let identity_id = mgr.identity_hash.clone();
        drop(mgr);
        initialize_storage_namespace(&root).unwrap();
        LxmfManager::load_or_create(&root, Some(&identity_id), None).unwrap()
    }

    fn restarted(mgr: LxmfManager, pool: &DbPool) -> LxmfManager {
        let mut mgr = reloaded(mgr);
        let rows = db::outbox_rows(pool, &mgr.identity_hash).unwrap();
        assert!(mgr.resume_outbox(rows, pool).is_empty());
        mgr
    }

    fn unix_now() -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    #[test]
    fn a_restart_queues_the_outbox_again_as_the_same_message() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "ab".repeat(16);
        let id = send_for_resume(&mut mgr, &pool, &dest, DeliveryPreference::Direct);
        let wire = mgr.router.pending_outbound[0].wire_bytes().to_vec();
        let identity_id = mgr.identity_hash.clone();
        assert_eq!(db::outbox_rows(&pool, &identity_id).unwrap().len(), 1);

        let mut mgr = restarted(mgr, &pool);
        assert!(
            mgr.router.pending_outbound.is_empty(),
            "nothing until a tick"
        );
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(results.is_empty(), "{results:?}");
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].message_id(), id);
        assert_eq!(
            mgr.router.pending_outbound[0].wire_bytes(),
            wire,
            "the same signed message"
        );
        assert_eq!(
            db::get_conversation(&pool, &dest, &identity_id, 10).len(),
            1
        );
    }

    #[test]
    fn a_restart_requeues_a_send_still_waiting_for_its_stamp() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "ac".repeat(16);
        let dest_bytes: [u8; 16] = hex::decode(&dest).unwrap().try_into().unwrap();
        mgr.router.set_stamp_cost(dest_bytes, 1);
        let id = send_for_resume(&mut mgr, &pool, &dest, DeliveryPreference::Direct);
        assert!(mgr.router.pending_deferred_stamps.contains_key(&id));

        let mut mgr = restarted(mgr, &pool);
        mgr.router.set_stamp_cost(dest_bytes, 1);
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(results.is_empty(), "{results:?}");
        assert!(mgr.router.pending_deferred_stamps.contains_key(&id));
    }

    #[test]
    fn an_outbox_record_that_rebuilds_another_message_is_not_sent() {
        let pool = test_pool();
        let mut mgr = test_manager();
        send_for_resume(
            &mut mgr,
            &pool,
            &"ad".repeat(16),
            DeliveryPreference::Direct,
        );
        let mut mgr = reloaded(mgr);
        let mut rows = db::outbox_rows(&pool, &mgr.identity_hash).unwrap();
        rows[0].message_id = "ee".repeat(32);
        assert!(mgr.resume_outbox(rows, &pool).is_empty());
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert_eq!(results, [("ee".repeat(32), "failed")]);
        assert!(mgr.router.pending_outbound.is_empty());

        let unreadable = db::OutboxRow {
            message_id: "ff".repeat(32),
            preference: "auto".into(),
            profile: "message".into(),
            message: b"not a record".to_vec(),
        };
        assert_eq!(
            mgr.resume_outbox(vec![unreadable], &pool),
            ["ff".repeat(32)]
        );
    }

    #[test]
    fn a_resumed_offline_inbox_send_waits_a_while_for_its_relay() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let node = [0x42; 16];
        mgr.router.set_outbound_propagation_node(Some(node));
        let id = send_for_resume(
            &mut mgr,
            &pool,
            &"ae".repeat(16),
            DeliveryPreference::Propagated,
        );

        let mut mgr = restarted(mgr, &pool);
        mgr.client_propagation_enabled = true;
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(results.is_empty());
        assert_eq!(mgr.resumable.len(), 1, "waits for a relay");

        mgr.router.set_outbound_propagation_node(Some(node));
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(results.is_empty(), "{results:?}");
        assert!(mgr.resumable.is_empty());
        assert_eq!(mgr.router.pending_outbound[0].message_id(), id);
        assert_eq!(
            mgr.router.pending_outbound[0].method,
            DeliveryMethod::Propagated
        );

        let mut mgr = restarted(mgr, &pool);
        mgr.client_propagation_enabled = true;
        mgr.queue_resumable(unix_now() + RESUME_RELAY_WAIT_SECS + 1.0, &mut results);
        assert_eq!(results, [(hex::encode(id), "failed")], "not forever");
    }

    #[test]
    fn a_send_whose_history_cannot_be_written_leaves_the_queue() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "b0".repeat(16);
        let identity_id = mgr.identity_hash.clone();
        pool.get()
            .unwrap()
            .execute_batch("DROP TABLE lxmf_outbox;")
            .unwrap();

        let text = mgr.send_message_with_preference_report(MessageSendRequest {
            dest_hash_hex: &dest,
            content: "no history, no send",
            title: "",
            db_pool: &pool,
            identity_id: &identity_id,
            preference: DeliveryPreference::Direct,
            profile: DeliveryProfile::Message,
            renderer: None,
            thread: "",
        });
        assert_eq!(text, Err(LxmfSubmissionFailure::PreparationFailed));
        let attachment =
            mgr.send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                dest_hash_hex: &dest,
                content: "",
                title: "",
                file_name: "notes.txt",
                file_bytes: b"notes",
                staged_path: None,
                is_image: false,
                image_mime: "",
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Direct,
                renderer: None,
                thread: "",
            });
        assert_eq!(attachment, Err(LxmfSubmissionFailure::StorageFailed));
        assert!(mgr.router.pending_outbound.is_empty());
        assert!(db::get_conversation(&pool, &dest, &identity_id, 10).is_empty());
        assert_eq!(
            std::fs::read_dir(mgr.files_dir()).unwrap().count(),
            0,
            "the attachment's file goes with it"
        );
    }

    #[test]
    fn a_cancel_reaches_a_send_still_waiting_to_resume() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let node = [0x42; 16];
        mgr.router.set_outbound_propagation_node(Some(node));
        let id = send_for_resume(
            &mut mgr,
            &pool,
            &"af".repeat(16),
            DeliveryPreference::Propagated,
        );

        let mut mgr = restarted(mgr, &pool);
        mgr.client_propagation_enabled = true;
        assert!(mgr.cancel_outbound_message(&hex::encode(id)));
        mgr.router.set_outbound_propagation_node(Some(node));
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(
            mgr.router.pending_outbound.is_empty(),
            "a cancelled send stays cancelled"
        );
        assert!(results.is_empty());
    }

    #[test]
    fn retry_sends_the_same_message_by_the_chosen_method() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let dest = "b1".repeat(16);
        let identity_id = mgr.identity_hash.clone();
        let id = send_for_resume(&mut mgr, &pool, &dest, DeliveryPreference::Direct);
        let msg_id = hex::encode(id);
        assert!(
            db::retryable_outbox_row(&pool, &identity_id, &msg_id)
                .unwrap()
                .is_none(),
            "nothing to retry while it is on its way"
        );
        mgr.cancel_outbound_message(&msg_id);
        assert!(db::update_message_state(
            &pool,
            &msg_id,
            &identity_id,
            "failed",
            None
        ));
        assert!(mgr.router.pending_outbound.is_empty());

        let row = db::retryable_outbox_row(&pool, &identity_id, &msg_id)
            .unwrap()
            .expect("a Not delivered message keeps its outbox entry");
        mgr.router.set_outbound_propagation_node(Some([0x43; 16]));
        let method = mgr
            .retry_outbound(&pool, &identity_id, &row, DeliveryPreference::Propagated)
            .unwrap();
        assert_eq!(method, DeliveryMethod::Propagated);
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(
            mgr.router.pending_outbound[0].message_id(),
            id,
            "the identical message"
        );
        let conversation = db::get_conversation(&pool, &dest, &identity_id, 10);
        assert_eq!(conversation.len(), 1, "the same bubble");
        assert_eq!(conversation[0]["state"], "sending");
        assert_eq!(conversation[0]["delivery_method"], "propagated");
        let rows = db::outbox_rows(&pool, &identity_id).unwrap();
        assert_eq!(
            rows[0].preference, "propagated",
            "a restart resumes it the same way"
        );

        assert!(
            mgr.retry_outbound(&pool, &identity_id, &row, DeliveryPreference::Direct)
                .is_err(),
            "a message on its way is not retried twice"
        );
        assert_eq!(
            mgr.router.pending_outbound.len(),
            1,
            "the refused retry neither queues a copy nor stops the first"
        );
    }

    /// A playable Ogg Opus clip of `frames` 20 ms frames, large enough to be
    /// referenced rather than copied.
    #[cfg(feature = "lxst-voice")]
    fn long_ogg_opus_fixture(frames: usize) -> Vec<u8> {
        use ogg::writing::{PacketWriteEndInfo, PacketWriter};

        let serial = 0x5253_4156;
        let mut writer = PacketWriter::new(Vec::new());
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, 1]);
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&24_000u32.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        writer
            .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&0u32.to_le_bytes());
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .unwrap();
        for frame in 0..frames {
            let mut packet = vec![0xf8];
            packet.extend((0..150u32).map(|i| (i * 31 + frame as u32 * 7) as u8));
            let end = if frame + 1 == frames {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            writer
                .write_packet(packet, serial, end, 960 * (frame as u64 + 1))
                .unwrap();
        }
        writer.into_inner()
    }

    fn media_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 131 % 251) as u8).collect()
    }

    fn assert_outbox_points_at_its_file(pool: &DbPool, identity_id: &str, len: usize) {
        let rows = db::outbox_rows(pool, identity_id).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            rows[0].message.len() < 1024,
            "the outbox holds a reference, not the {len}-byte file ({} bytes)",
            rows[0].message.len()
        );
        assert!(LxmfManager::outbox_send_size(&rows[0]).unwrap() >= len);
    }

    fn assert_resumes_identically(mgr: LxmfManager, pool: &DbPool, id: [u8; 32], wire: &[u8]) {
        let mut mgr = restarted(mgr, pool);
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert!(results.is_empty(), "{results:?}");
        assert_eq!(mgr.router.pending_outbound.len(), 1);
        assert_eq!(mgr.router.pending_outbound[0].message_id(), id);
        assert_eq!(
            mgr.router.pending_outbound[0].wire_bytes(),
            wire,
            "rebuilt from the stored file, byte for byte"
        );
    }

    fn send_attachment(
        mgr: &mut LxmfManager,
        pool: &DbPool,
        bytes: &[u8],
        is_image: bool,
        staged_path: Option<&Path>,
    ) -> [u8; 32] {
        let identity_id = mgr.identity_hash.clone();
        let queued = mgr
            .send_message_with_attachment_fields_preference_report(AttachmentMessageRequest {
                dest_hash_hex: &"c1".repeat(16),
                content: "see attached",
                title: "",
                file_name: if is_image { "photo.webp" } else { "notes.pdf" },
                file_bytes: bytes,
                staged_path,
                is_image,
                image_mime: if is_image { "image/webp" } else { "" },
                db_pool: pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Direct,
                renderer: None,
                thread: "",
            })
            .unwrap();
        hex::decode(queued.message_id).unwrap().try_into().unwrap()
    }

    #[test]
    fn file_and_image_sends_resume_from_their_stored_files() {
        for is_image in [false, true] {
            let pool = test_pool();
            let mut mgr = test_manager();
            let bytes = media_bytes(48_000);
            let id = send_attachment(&mut mgr, &pool, &bytes, is_image, None);
            let wire = mgr.router.pending_outbound[0].wire_bytes().to_vec();
            assert_outbox_points_at_its_file(&pool, &mgr.identity_hash.clone(), bytes.len());
            assert_resumes_identically(mgr, &pool, id, &wire);
        }
    }

    #[test]
    fn a_staged_attachment_resumes_from_the_file_it_became() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let bytes = media_bytes(64_000);
        let staged = mgr.data_dir.join("staged-photo");
        std::fs::write(&staged, &bytes).unwrap();
        let id = send_attachment(&mut mgr, &pool, &bytes, true, Some(&staged));
        assert!(!staged.exists(), "adopted into the files folder");
        let wire = mgr.router.pending_outbound[0].wire_bytes().to_vec();
        assert_outbox_points_at_its_file(&pool, &mgr.identity_hash.clone(), bytes.len());
        assert_resumes_identically(mgr, &pool, id, &wire);
    }

    #[cfg(feature = "lxst-voice")]
    #[test]
    fn a_voice_message_resumes_from_its_stored_clip() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let clip = long_ogg_opus_fixture(40);
        assert!(clip.len() > 4096);
        let identity_id = mgr.identity_hash.clone();
        let queued = mgr
            .send_audio_message_with_preference_report(AudioMessageRequest {
                thread: "",
                dest_hash_hex: &"c2".repeat(16),
                content: "",
                title: "",
                audio_mode: lxmf_core::constants::AM_OPUS_OGG,
                audio_bytes: &clip,
                staged_path: None,
                db_pool: &pool,
                identity_id: &identity_id,
                preference: DeliveryPreference::Direct,
            })
            .expect("voice message queued");
        let id: [u8; 32] = hex::decode(queued.message_id).unwrap().try_into().unwrap();
        let wire = mgr.router.pending_outbound[0].wire_bytes().to_vec();
        assert_outbox_points_at_its_file(&pool, &identity_id, clip.len());
        assert_resumes_identically(mgr, &pool, id, &wire);
    }

    #[test]
    fn a_send_whose_file_is_gone_fails_instead_of_sending_another_message() {
        let pool = test_pool();
        let mut mgr = test_manager();
        let bytes = media_bytes(20_000);
        let id = send_attachment(&mut mgr, &pool, &bytes, false, None);
        for entry in std::fs::read_dir(mgr.files_dir()).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        let mut mgr = restarted(mgr, &pool);
        let mut results = Vec::new();
        mgr.queue_resumable(unix_now(), &mut results);
        assert_eq!(results, [(hex::encode(id), "failed")]);
        assert!(mgr.router.pending_outbound.is_empty());
    }
}
