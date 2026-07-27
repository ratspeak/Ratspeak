//! `#[tauri::command]` functions — the IPC surface to the WebView.
//! Commands must not hold `std::sync::{Mutex, RwLock}` guards across `.await`;
//! delegate blocking work to `db::spawn_db` or a worker task.

pub mod activity;
pub mod ble;
pub mod channel_hub;
pub mod channels;
pub mod contact_card;
pub mod contacts;
pub mod games;
#[cfg(feature = "hardware")]
pub mod hardware;
pub mod identity;
pub(crate) mod interface_activity;
pub mod interfaces;
pub mod messaging;
pub mod network;
pub mod peers;
#[cfg(any(
    test,
    feature = "ble",
    feature = "serial",
    feature = "rnode-tcp",
    target_os = "android"
))]
pub(crate) mod rnode_readiness;
pub mod shared;
pub mod system;
#[cfg(feature = "lxst-voice")]
pub mod voice;
