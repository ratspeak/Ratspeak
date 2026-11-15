//! Ratspeak domain types, emitter trait, and errors. No Tauri, SQLite, or RNS.

pub mod config;
pub mod delivery;
pub mod emitter;
pub mod errors;
pub mod names;
pub mod notification;
pub mod radio;
pub mod types;

pub use emitter::{EmitError, Emitter, NoopEmitter};
pub use errors::CoreError;
pub use names::default_display_name;
pub use notification::{
    NativeNotification, NativeNotificationKind, NativeNotifier, NoopNotifier, NotificationPreview,
};
pub use types::{LXMF_DELIVERY_APP_NAME, LXMF_PROPAGATION_APP_NAME, hex_to_array16};
