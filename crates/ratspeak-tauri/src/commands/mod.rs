//! `#[tauri::command]` functions — the IPC surface to the WebView.
//! Commands must not hold `std::sync::{Mutex, RwLock}` guards across `.await`;
//! delegate blocking work to `db::spawn_db` or a worker task.

pub mod ble;
pub mod contacts;
pub mod games;
pub mod identity;
pub mod interfaces;
pub mod messaging;
pub mod network;
pub mod peers;
pub mod shared;
pub mod system;
#[cfg(feature = "lxst-voice")]
pub mod voice;
