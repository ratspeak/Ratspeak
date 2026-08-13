//! Narrow native-mobile control surface.
//!
//! Platform code owns OS lifecycle objects (Android GATT/service state), while
//! Rust remains the authority for protocol/runtime generations. The default
//! implementation is inert so headless and non-mobile builds have no ambient
//! native dependency.

#[derive(Clone)]
pub struct NativeBleRnodeRequest {
    pub address: String,
    pub tcp_port: u16,
    pub activity_operation: String,
    pub native_generation: u64,
    pub name: String,
    pub port: String,
    pub frequency: u64,
    pub bandwidth: u64,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
    pub mode: Option<String>,
    pub mode_value: u8,
    pub airtime_limit_short: Option<f64>,
    pub airtime_limit_long: Option<f64>,
    pub id_interval: Option<u64>,
    pub id_callsign: Option<String>,
    pub saved_startup: bool,
}

pub enum NativeBleRnodeDisconnect<'a> {
    Current,
    ExactOperation(&'a str),
}

pub trait MobilePlatformBridge: Send + Sync {
    fn start_or_replace_ble_rnode(&self, _request: NativeBleRnodeRequest) -> bool {
        false
    }

    fn disconnect_ble_rnode(&self, _owner: NativeBleRnodeDisconnect<'_>) -> bool {
        false
    }

    fn request_android_usb_permission(
        &self,
        _vendor_id: u16,
        _product_id: u16,
        _serial_number: Option<&str>,
    ) -> bool {
        false
    }

    fn request_android_usb_permission_legacy(&self, _device_name: &str) -> bool {
        false
    }

    fn replay_platform_state(&self) {}
}

pub struct NoopMobilePlatformBridge;

impl MobilePlatformBridge for NoopMobilePlatformBridge {}
