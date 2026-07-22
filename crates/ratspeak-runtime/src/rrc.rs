//! Reticulum Relay Chat protocol 0.1.3 wire codec.
//!
//! This module deliberately contains no persistence or UI concerns. RRC
//! envelopes are compact CBOR maps with unsigned integer keys; unknown keys
//! and message types remain forward-compatible.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use ciborium::value::Value;

pub const RRC_VERSION: u64 = 1;
pub const RRC_HUB_ASPECT: &str = "rrc.hub";

pub const K_VERSION: u64 = 0;
pub const K_TYPE: u64 = 1;
pub const K_ID: u64 = 2;
pub const K_TIMESTAMP: u64 = 3;
pub const K_SOURCE: u64 = 4;
pub const K_ROOM: u64 = 5;
pub const K_BODY: u64 = 6;
pub const K_NICKNAME: u64 = 7;
pub const K_DESTINATION: u64 = 8;

pub const HELLO_CLIENT_NAME: u64 = 0;
pub const HELLO_CLIENT_VERSION: u64 = 1;
pub const HELLO_CAPABILITIES: u64 = 2;

pub const WELCOME_HUB_NAME: u64 = 0;
pub const WELCOME_HUB_VERSION: u64 = 1;
pub const WELCOME_CAPABILITIES: u64 = 2;
pub const WELCOME_LIMITS: u64 = 3;

pub const LIMIT_MAX_NICK_BYTES: u64 = 0;
pub const LIMIT_MAX_ROOM_NAME_BYTES: u64 = 1;
pub const LIMIT_MAX_MESSAGE_BODY_BYTES: u64 = 2;
pub const LIMIT_MAX_ROOMS_PER_SESSION: u64 = 3;
pub const LIMIT_RATE_MESSAGES_PER_MINUTE: u64 = 4;

pub const CAP_RESOURCE_ENVELOPE: u64 = 0;
pub const CAP_ACTION: u64 = 1;
pub const CAP_DIRECT_NOTICE: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Hello,
    Welcome,
    Join,
    Joined,
    Part,
    Parted,
    Message,
    Notice,
    Action,
    Ping,
    Pong,
    Error,
    ResourceEnvelope,
    Unknown(u64),
}

impl MessageType {
    pub fn from_wire(value: u64) -> Self {
        match value {
            1 => Self::Hello,
            2 => Self::Welcome,
            10 => Self::Join,
            11 => Self::Joined,
            12 => Self::Part,
            13 => Self::Parted,
            20 => Self::Message,
            21 => Self::Notice,
            22 => Self::Action,
            30 => Self::Ping,
            31 => Self::Pong,
            40 => Self::Error,
            50 => Self::ResourceEnvelope,
            other => Self::Unknown(other),
        }
    }

    pub fn wire(self) -> u64 {
        match self {
            Self::Hello => 1,
            Self::Welcome => 2,
            Self::Join => 10,
            Self::Joined => 11,
            Self::Part => 12,
            Self::Parted => 13,
            Self::Message => 20,
            Self::Notice => 21,
            Self::Action => 22,
            Self::Ping => 30,
            Self::Pong => 31,
            Self::Error => 40,
            Self::ResourceEnvelope => 50,
            Self::Unknown(value) => value,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub version: u64,
    pub message_type: MessageType,
    pub message_id: [u8; 8],
    pub timestamp_ms: u64,
    pub source: [u8; 16],
    pub room: Option<String>,
    pub body: Option<Value>,
    pub nickname: Option<String>,
    pub destination: Option<[u8; 16]>,
}

impl Envelope {
    pub fn new(message_type: MessageType, source: [u8; 16]) -> Self {
        let uuid = uuid::Uuid::new_v4();
        let mut message_id = [0u8; 8];
        message_id.copy_from_slice(&uuid.as_bytes()[..8]);
        Self {
            version: RRC_VERSION,
            message_type,
            message_id,
            timestamp_ms: now_ms(),
            source,
            room: None,
            body: None,
            nickname: None,
            destination: None,
        }
    }

    pub fn hello(source: [u8; 16], nickname: &str, client_version: &str) -> Self {
        let capabilities = integer_map(vec![
            (CAP_ACTION, Value::Bool(true)),
            (CAP_DIRECT_NOTICE, Value::Bool(true)),
        ]);
        let mut envelope = Self::new(MessageType::Hello, source);
        envelope.nickname = Some(nickname.to_string());
        envelope.body = Some(integer_map(vec![
            (HELLO_CLIENT_NAME, Value::Text("Ratspeak".into())),
            (
                HELLO_CLIENT_VERSION,
                Value::Text(client_version.to_string()),
            ),
            (HELLO_CAPABILITIES, capabilities),
        ]));
        envelope
    }

    pub fn room_command(
        message_type: MessageType,
        source: [u8; 16],
        room: &str,
        nickname: &str,
    ) -> Self {
        let mut envelope = Self::new(message_type, source);
        envelope.room = Some(room.to_string());
        envelope.nickname = Some(nickname.to_string());
        envelope
    }

    pub fn room_text(
        message_type: MessageType,
        source: [u8; 16],
        room: &str,
        nickname: &str,
        text: &str,
    ) -> Self {
        let mut envelope = Self::room_command(message_type, source, room, nickname);
        envelope.body = Some(Value::Text(text.to_string()));
        envelope
    }

    pub fn pong(source: [u8; 16], ping: &Envelope) -> Self {
        let mut envelope = Self::new(MessageType::Pong, source);
        envelope.body = ping.body.clone();
        envelope
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HubLimits {
    pub max_nick_bytes: Option<usize>,
    pub max_room_name_bytes: Option<usize>,
    pub max_message_body_bytes: Option<usize>,
    pub max_rooms_per_session: Option<usize>,
    pub rate_messages_per_minute: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WelcomeInfo {
    pub hub_name: Option<String>,
    pub hub_version: Option<String>,
    pub capabilities: BTreeMap<u64, bool>,
    pub limits: HubLimits,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("invalid CBOR: {0}")]
    InvalidCbor(String),
    #[error("RRC envelope must be a CBOR map")]
    NotAMap,
    #[error("RRC envelope keys must be unsigned integers")]
    InvalidKey,
    #[error("missing RRC envelope field {0}")]
    MissingField(u64),
    #[error("invalid RRC envelope field {0}")]
    InvalidField(u64),
    #[error("unsupported RRC version {0}")]
    UnsupportedVersion(u64),
    #[error("RRC value exceeds this platform's size")]
    SizeOverflow,
    #[error("nickname is empty or contains an invalid control character")]
    InvalidNickname,
    #[error("nickname exceeds the hub's {0}-byte limit")]
    NicknameTooLong(usize),
    #[error("room name is empty")]
    InvalidRoom,
    #[error("room name exceeds the hub's {0}-byte limit")]
    RoomTooLong(usize),
}

pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
    let mut fields = vec![
        (K_VERSION, unsigned(envelope.version)),
        (K_TYPE, unsigned(envelope.message_type.wire())),
        (K_ID, Value::Bytes(envelope.message_id.to_vec())),
        (K_TIMESTAMP, unsigned(envelope.timestamp_ms)),
        (K_SOURCE, Value::Bytes(envelope.source.to_vec())),
    ];
    if let Some(room) = envelope.room.as_ref() {
        fields.push((K_ROOM, Value::Text(room.clone())));
    }
    if let Some(body) = envelope.body.as_ref() {
        fields.push((K_BODY, body.clone()));
    }
    if let Some(nickname) = envelope.nickname.as_ref() {
        fields.push((K_NICKNAME, Value::Text(nickname.clone())));
    }
    if let Some(destination) = envelope.destination {
        fields.push((K_DESTINATION, Value::Bytes(destination.to_vec())));
    }

    fields.sort_by_key(|(key, _)| *key);
    let value = integer_map(fields);
    let mut encoded = Vec::new();
    ciborium::ser::into_writer(&value, &mut encoded)
        .map_err(|error| ProtocolError::InvalidCbor(error.to_string()))?;
    Ok(encoded)
}

pub fn decode(data: &[u8]) -> Result<Envelope, ProtocolError> {
    let mut cursor = Cursor::new(data);
    let value: Value = ciborium::de::from_reader(&mut cursor)
        .map_err(|error| ProtocolError::InvalidCbor(error.to_string()))?;
    if cursor.position() as usize != data.len() {
        return Err(ProtocolError::InvalidCbor(
            "trailing bytes after envelope".into(),
        ));
    }
    let Value::Map(fields) = value else {
        return Err(ProtocolError::NotAMap);
    };
    for (key, _) in &fields {
        if as_unsigned(key).is_none() {
            return Err(ProtocolError::InvalidKey);
        }
    }

    let version = required_unsigned(&fields, K_VERSION)?;
    if version != RRC_VERSION {
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    let message_type = MessageType::from_wire(required_unsigned(&fields, K_TYPE)?);
    let message_id = fixed_bytes::<8>(&fields, K_ID)?;
    let timestamp_ms = required_unsigned(&fields, K_TIMESTAMP)?;
    let source = fixed_bytes::<16>(&fields, K_SOURCE)?;
    let room = optional_text(&fields, K_ROOM)?;
    let body = map_get(&fields, K_BODY).cloned();
    let nickname = optional_text(&fields, K_NICKNAME)?;
    let destination = match map_get(&fields, K_DESTINATION) {
        Some(_) => Some(fixed_bytes::<16>(&fields, K_DESTINATION)?),
        None => None,
    };

    Ok(Envelope {
        version,
        message_type,
        message_id,
        timestamp_ms,
        source,
        room,
        body,
        nickname,
        destination,
    })
}

pub fn parse_welcome(envelope: &Envelope) -> WelcomeInfo {
    let Some(Value::Map(body)) = envelope.body.as_ref() else {
        return WelcomeInfo::default();
    };
    let capabilities = map_get(body, WELCOME_CAPABILITIES)
        .and_then(as_integer_bool_map)
        .unwrap_or_default();
    let limits = map_get(body, WELCOME_LIMITS)
        .and_then(|value| match value {
            Value::Map(map) => Some(HubLimits {
                max_nick_bytes: map_usize(map, LIMIT_MAX_NICK_BYTES),
                max_room_name_bytes: map_usize(map, LIMIT_MAX_ROOM_NAME_BYTES),
                max_message_body_bytes: map_usize(map, LIMIT_MAX_MESSAGE_BODY_BYTES),
                max_rooms_per_session: map_usize(map, LIMIT_MAX_ROOMS_PER_SESSION),
                rate_messages_per_minute: map_usize(map, LIMIT_RATE_MESSAGES_PER_MINUTE),
            }),
            _ => None,
        })
        .unwrap_or_default();
    WelcomeInfo {
        hub_name: map_get(body, WELCOME_HUB_NAME)
            .and_then(as_text)
            .map(str::to_string),
        hub_version: map_get(body, WELCOME_HUB_VERSION)
            .and_then(as_text)
            .map(str::to_string),
        capabilities,
        limits,
    }
}

pub fn member_identities(envelope: &Envelope) -> Vec<[u8; 16]> {
    let Some(Value::Array(values)) = envelope.body.as_ref() else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| match value {
            Value::Bytes(bytes) => bytes.as_slice().try_into().ok(),
            _ => None,
        })
        .collect()
}

pub fn text_body(envelope: &Envelope) -> Option<&str> {
    envelope.body.as_ref().and_then(as_text)
}

pub fn normalize_nickname(value: &str, max_bytes: usize) -> Result<String, ProtocolError> {
    let nickname = value.trim();
    if nickname.is_empty() || nickname.chars().any(|ch| matches!(ch, '\n' | '\r' | '\0')) {
        return Err(ProtocolError::InvalidNickname);
    }
    if max_bytes > 0 && nickname.len() > max_bytes {
        return Err(ProtocolError::NicknameTooLong(max_bytes));
    }
    Ok(nickname.to_string())
}

pub fn normalize_room(value: &str, max_bytes: usize) -> Result<String, ProtocolError> {
    // The reference hub treats room names as trimmed, case-insensitive UTF-8
    // strings. It does not impose IRC's leading-# convention and permits
    // spaces, so the UI must not silently rewrite either form.
    let room = value.trim().to_lowercase();
    if room.is_empty() {
        return Err(ProtocolError::InvalidRoom);
    }
    if max_bytes > 0 && room.len() > max_bytes {
        return Err(ProtocolError::RoomTooLong(max_bytes));
    }
    Ok(room)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn integer_map(entries: Vec<(u64, Value)>) -> Value {
    Value::Map(
        entries
            .into_iter()
            .map(|(key, value)| (unsigned(key), value))
            .collect(),
    )
}

fn unsigned(value: u64) -> Value {
    Value::Integer(value.into())
}

fn as_unsigned(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(integer) => u64::try_from(*integer).ok(),
        _ => None,
    }
}

fn as_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) => Some(text),
        _ => None,
    }
}

fn map_get(fields: &[(Value, Value)], key: u64) -> Option<&Value> {
    fields
        .iter()
        .find_map(|(candidate, value)| (as_unsigned(candidate) == Some(key)).then_some(value))
}

fn required_unsigned(fields: &[(Value, Value)], key: u64) -> Result<u64, ProtocolError> {
    map_get(fields, key)
        .ok_or(ProtocolError::MissingField(key))
        .and_then(|value| as_unsigned(value).ok_or(ProtocolError::InvalidField(key)))
}

fn fixed_bytes<const N: usize>(
    fields: &[(Value, Value)],
    key: u64,
) -> Result<[u8; N], ProtocolError> {
    let bytes = match map_get(fields, key).ok_or(ProtocolError::MissingField(key))? {
        Value::Bytes(bytes) => bytes,
        _ => return Err(ProtocolError::InvalidField(key)),
    };
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| ProtocolError::InvalidField(key))
}

fn optional_text(fields: &[(Value, Value)], key: u64) -> Result<Option<String>, ProtocolError> {
    match map_get(fields, key) {
        Some(Value::Text(text)) => Ok(Some(text.clone())),
        Some(_) => Err(ProtocolError::InvalidField(key)),
        None => Ok(None),
    }
}

fn as_integer_bool_map(value: &Value) -> Option<BTreeMap<u64, bool>> {
    let Value::Map(entries) = value else {
        return None;
    };
    let mut result = BTreeMap::new();
    for (key, value) in entries {
        let key = as_unsigned(key)?;
        let Value::Bool(enabled) = value else {
            continue;
        };
        result.insert(key, *enabled);
    }
    Some(result)
}

fn map_usize(entries: &[(Value, Value)], key: u64) -> Option<usize> {
    let value = map_get(entries, key).and_then(as_unsigned)?;
    value.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips_with_numeric_keys_and_capabilities() {
        let hello = Envelope::hello([0x11; 16], "rat", "1.0.25");
        let encoded = encode(&hello).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, hello);
        assert!(encoded.len() < 128);
    }

    #[test]
    fn unknown_envelope_keys_and_types_are_forward_compatible() {
        let mut fields = match integer_map(vec![
            (K_VERSION, unsigned(1)),
            (K_TYPE, unsigned(63)),
            (K_ID, Value::Bytes(vec![1; 8])),
            (K_TIMESTAMP, unsigned(1)),
            (K_SOURCE, Value::Bytes(vec![2; 16])),
            (50, Value::Text("future".into())),
        ]) {
            Value::Map(fields) => fields,
            _ => unreachable!(),
        };
        fields.reverse();
        let mut encoded = Vec::new();
        ciborium::ser::into_writer(&Value::Map(fields), &mut encoded).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.message_type, MessageType::Unknown(63));
    }

    #[test]
    fn rejects_string_and_negative_envelope_keys() {
        for invalid_key in [Value::Text("0".into()), Value::Integer((-1).into())] {
            let value = Value::Map(vec![(invalid_key, unsigned(1))]);
            let mut encoded = Vec::new();
            ciborium::ser::into_writer(&value, &mut encoded).unwrap();
            assert!(matches!(decode(&encoded), Err(ProtocolError::InvalidKey)));
        }
    }

    #[test]
    fn parses_rrcd_welcome_limits_and_capabilities() {
        let mut welcome = Envelope::new(MessageType::Welcome, [0x22; 16]);
        welcome.body = Some(integer_map(vec![
            (WELCOME_HUB_NAME, Value::Text("Field Hub".into())),
            (WELCOME_HUB_VERSION, Value::Text("0.1.3".into())),
            (
                WELCOME_CAPABILITIES,
                integer_map(vec![(CAP_ACTION, Value::Bool(true))]),
            ),
            (
                WELCOME_LIMITS,
                integer_map(vec![
                    (LIMIT_MAX_NICK_BYTES, unsigned(32)),
                    (LIMIT_MAX_ROOM_NAME_BYTES, unsigned(64)),
                    (LIMIT_MAX_MESSAGE_BODY_BYTES, unsigned(350)),
                    (LIMIT_MAX_ROOMS_PER_SESSION, unsigned(16)),
                    (LIMIT_RATE_MESSAGES_PER_MINUTE, unsigned(240)),
                ]),
            ),
        ]));
        let parsed = parse_welcome(&welcome);
        assert_eq!(parsed.hub_name.as_deref(), Some("Field Hub"));
        assert_eq!(parsed.limits.max_message_body_bytes, Some(350));
        assert_eq!(parsed.capabilities.get(&CAP_ACTION), Some(&true));
    }

    #[test]
    fn nickname_and_room_limits_are_utf8_byte_limits() {
        assert!(normalize_nickname("  radio rat  ", 32).is_ok());
        assert!(matches!(
            normalize_nickname("学习", 5),
            Err(ProtocolError::NicknameTooLong(5))
        ));
        assert_eq!(normalize_room("General", 64).unwrap(), "general");
        assert_eq!(normalize_room(" Two Rooms ", 64).unwrap(), "two rooms");
    }
}
