//! Versioned, owned Activity event-query responses.
//!
//! Masked detail and sanitized copy values are ordinary serializable DTOs.
//! Explicitly revealed values use a non-cloneable, non-debuggable zeroizing
//! wrapper so raw identifiers and endpoints cannot be retained accidentally by
//! generic logging or response plumbing.

use serde::{Serialize, Serializer};
use tokio::sync::OwnedRwLockReadGuard;
use zeroize::Zeroizing;

use super::replay::ActivityStatusV1;
use super::schema::{
    ACTIVITY_SCHEMA_VERSION, ActivityAttributeKey, ActivityEventV1, EndpointClass, IdentifierKind,
};

/// Tauri-facing query response that keeps the recorder's privacy read lease
/// alive until serialization has consumed the owned value. This prevents
/// Clear or hard reset from acknowledging while an older response can still
/// cross IPC.
pub struct ActivityIpcResponse<T> {
    value: T,
    _privacy_lease: OwnedRwLockReadGuard<()>,
}

impl<T> ActivityIpcResponse<T> {
    pub(super) fn new(value: T, privacy_lease: OwnedRwLockReadGuard<()>) -> Self {
        Self {
            value,
            _privacy_lease: privacy_lease,
        }
    }
}

impl<T: Serialize> Serialize for ActivityIpcResponse<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ActivityDetailResultV1 {
    Found {
        version: u8,
        event: Box<ActivityEventV1>,
    },
    NotFound {
        version: u8,
    },
    SessionMismatch {
        version: u8,
        status: Box<ActivityStatusV1>,
    },
}

impl ActivityDetailResultV1 {
    pub(super) fn found(event: ActivityEventV1) -> Self {
        Self::Found {
            version: ACTIVITY_SCHEMA_VERSION,
            event: Box::new(event),
        }
    }

    pub(super) const fn not_found() -> Self {
        Self::NotFound {
            version: ACTIVITY_SCHEMA_VERSION,
        }
    }

    pub(super) fn session_mismatch(status: ActivityStatusV1) -> Self {
        Self::SessionMismatch {
            version: ACTIVITY_SCHEMA_VERSION,
            status: Box::new(status),
        }
    }
}

/// Transient explicit-reveal value. It deliberately has no `Clone` or `Debug`
/// implementation and zeroizes its owned string when the command response is
/// dropped after serialization.
pub struct ActivityRevealedValue(Zeroizing<String>);

impl ActivityRevealedValue {
    pub(super) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
}

impl Serialize for ActivityRevealedValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

/// Raw-value responses are intentionally not cloneable or debug-formattable.
#[derive(Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ActivityRevealResultV1 {
    Identifier {
        version: u8,
        key: ActivityAttributeKey,
        kind: IdentifierKind,
        value: ActivityRevealedValue,
    },
    Endpoint {
        version: u8,
        key: ActivityAttributeKey,
        class: EndpointClass,
        value: ActivityRevealedValue,
    },
    NotRevealable {
        version: u8,
    },
    NotFound {
        version: u8,
    },
    SessionMismatch {
        version: u8,
        status: Box<ActivityStatusV1>,
    },
}

impl ActivityRevealResultV1 {
    pub(super) fn identifier(
        key: ActivityAttributeKey,
        kind: IdentifierKind,
        value: String,
    ) -> Self {
        Self::Identifier {
            version: ACTIVITY_SCHEMA_VERSION,
            key,
            kind,
            value: ActivityRevealedValue::new(value),
        }
    }

    pub(super) fn endpoint(key: ActivityAttributeKey, class: EndpointClass, value: String) -> Self {
        Self::Endpoint {
            version: ACTIVITY_SCHEMA_VERSION,
            key,
            class,
            value: ActivityRevealedValue::new(value),
        }
    }

    pub(super) const fn not_revealable() -> Self {
        Self::NotRevealable {
            version: ACTIVITY_SCHEMA_VERSION,
        }
    }

    pub(super) const fn not_found() -> Self {
        Self::NotFound {
            version: ACTIVITY_SCHEMA_VERSION,
        }
    }

    pub(super) fn session_mismatch(status: ActivityStatusV1) -> Self {
        Self::SessionMismatch {
            version: ACTIVITY_SCHEMA_VERSION,
            status: Box::new(status),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ActivitySafeCopyResultV1 {
    Found {
        version: u8,
        json: String,
    },
    NotFound {
        version: u8,
    },
    SessionMismatch {
        version: u8,
        status: Box<ActivityStatusV1>,
    },
}

impl ActivitySafeCopyResultV1 {
    pub(super) fn found(json: String) -> Self {
        Self::Found {
            version: ACTIVITY_SCHEMA_VERSION,
            json,
        }
    }

    pub(super) const fn not_found() -> Self {
        Self::NotFound {
            version: ACTIVITY_SCHEMA_VERSION,
        }
    }

    pub(super) fn session_mismatch(status: ActivityStatusV1) -> Self {
        Self::SessionMismatch {
            version: ACTIVITY_SCHEMA_VERSION,
            status: Box::new(status),
        }
    }
}
