//! Direct mobile lifecycle and network bridge.
//!
//! Native state must reach Rust while the WebView is paused or absent. The
//! bridge accepts only a closed network vocabulary, retains at most one
//! pre-runtime value, and rejects stale Android callback sequences.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

#[cfg(all(test, not(target_os = "android")))]
use ratspeak_tauri::mobile_platform::NativeBleRnodeRequest;
#[cfg(target_os = "android")]
use ratspeak_tauri::mobile_platform::{
    MobilePlatformBridge, NativeBleRnodeDisconnect, NativeBleRnodeRequest,
};
use ratspeak_tauri::state::AppState;
#[cfg(target_os = "android")]
use std::sync::OnceLock as AndroidOnceLock;

static APP_STATE: OnceLock<RwLock<Weak<AppState>>> = OnceLock::new();
static LAST_NETWORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
#[cfg(target_os = "android")]
static LAST_USB_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static NATIVE_NETWORK_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static NETWORK_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
static PENDING_NETWORK: Mutex<Option<PendingNetwork>> = Mutex::new(None);
#[cfg(target_os = "android")]
static PENDING_BLE_RNODE: Mutex<Option<NativeBleRnodeRequest>> = Mutex::new(None);
#[cfg(target_os = "android")]
static BLE_REPLACEMENT_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug)]
struct PendingNetwork {
    sequence: u64,
    network_type: NetworkType,
    transition: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NetworkType {
    Wifi,
    Cellular,
    Ethernet,
    None,
    Unknown,
}

impl NetworkType {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "wifi" => Some(Self::Wifi),
            "cellular" => Some(Self::Cellular),
            "ethernet" => Some(Self::Ethernet),
            "none" => Some(Self::None),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Cellular => "cellular",
            Self::Ethernet => "ethernet",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) fn install(state: &Arc<AppState>) {
    let slot = APP_STATE.get_or_init(|| RwLock::new(Weak::new()));
    if let Ok(mut installed) = slot.write() {
        *installed = Arc::downgrade(state);
    }

    if let Ok(mut pending) = PENDING_NETWORK.lock() {
        if let Some(pending) = pending.as_mut() {
            pending.transition = Some(state.begin_network_transition());
        }
    }
    ensure_network_worker();

    #[cfg(target_os = "android")]
    {
        state.install_mobile_platform_bridge(Arc::new(AndroidPlatformBridge));
        state.mobile_platform_bridge().replay_platform_state();
    }
}

#[cfg(target_os = "android")]
struct AndroidPlatformBridge;

#[cfg(target_os = "android")]
impl MobilePlatformBridge for AndroidPlatformBridge {
    fn start_or_replace_ble_rnode(&self, request: NativeBleRnodeRequest) -> bool {
        if !valid_ble_native_request(&request) {
            return false;
        }
        // Serialize Rust owner transitions with Kotlin's replacementLock. The
        // candidate must be visible before JNI because LISTENER_READY can call
        // Rust synchronously from startOrReplaceBleRnode().
        let Ok(_replacement_guard) = BLE_REPLACEMENT_LOCK.lock() else {
            return false;
        };
        let previous = match PENDING_BLE_RNODE.lock() {
            Ok(mut pending) => pending.replace(request.clone()),
            Err(_) => return false,
        };
        let started = android_start_ble_rnode(&request);
        if !started {
            if let Ok(mut pending) = PENDING_BLE_RNODE.lock() {
                // A terminal callback may already have consumed the candidate;
                // never resurrect ownership in that case.
                let current = pending.take();
                *pending = owner_after_rejected_ble_replacement(current, previous, request);
            }
        }
        started
    }

    fn disconnect_ble_rnode(&self, owner: NativeBleRnodeDisconnect<'_>) -> bool {
        let Ok(_replacement_guard) = BLE_REPLACEMENT_LOCK.lock() else {
            return false;
        };
        let activity_operation = match owner {
            NativeBleRnodeDisconnect::Current => None,
            NativeBleRnodeDisconnect::ExactOperation(token) => Some(token),
        };
        let Some(generation) = PENDING_BLE_RNODE.lock().ok().and_then(|pending| {
            pending
                .as_ref()
                .filter(|pending| {
                    activity_operation.is_none_or(|token| token == pending.activity_operation)
                })
                .map(|pending| pending.native_generation)
        }) else {
            return false;
        };
        let disconnected = android_disconnect_ble_rnode(activity_operation, generation);
        if disconnected {
            if let Ok(mut pending) = PENDING_BLE_RNODE.lock() {
                if pending.as_ref().is_some_and(|pending| {
                    activity_operation.is_none_or(|token| token == pending.activity_operation)
                        && generation == pending.native_generation
                }) {
                    *pending = None;
                }
            }
        }
        disconnected
    }

    fn replay_platform_state(&self) {
        let _ = with_android_bridge(|env, class| {
            env.call_static_method(class, "replayPlatformState", "()V", &[])
                .map(|_| true)
        });
    }

    fn request_android_usb_permission(
        &self,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<&str>,
    ) -> bool {
        android_request_usb_permission(vendor_id, product_id, serial_number)
    }

    fn request_android_usb_permission_legacy(&self, device_name: &str) -> bool {
        if device_name.is_empty() || device_name.len() > 256 {
            return false;
        }
        with_android_class(
            "org.ratspeak.android.RatspeakPlatformSupervisor",
            |env, class| {
                use jni::objects::{JObject, JValue};
                let device_name = env.new_string(device_name)?;
                env.call_static_method(
                    class,
                    "requestUsbPermissionForLegacyPath",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(JObject::from(device_name))],
                )?;
                Ok(true)
            },
        )
        .unwrap_or(false)
    }
}

#[cfg(any(target_os = "android", test))]
fn valid_ble_native_request(request: &NativeBleRnodeRequest) -> bool {
    request.tcp_port != 0
        && request.native_generation <= i64::MAX as u64
        && request.activity_operation.len() == 32
        && request
            .activity_operation
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        && valid_ble_mac_address(&request.address)
}

#[cfg(any(target_os = "android", test))]
fn valid_ble_mac_address(address: &str) -> bool {
    let bytes = address.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[cfg(any(target_os = "android", test))]
fn ble_request_matches(
    current: Option<&NativeBleRnodeRequest>,
    candidate: &NativeBleRnodeRequest,
) -> bool {
    current.is_some_and(|current| {
        current.activity_operation == candidate.activity_operation
            && current.native_generation == candidate.native_generation
    })
}

/// Kotlin binds a different-port replacement before displacing the prior
/// owner, but a same-port replacement must tombstone the prior listener before
/// rebinding. Mirror that exact admission contract after synchronous reject.
#[cfg(any(target_os = "android", test))]
fn failed_ble_replacement_owner(
    previous: Option<NativeBleRnodeRequest>,
    rejected: NativeBleRnodeRequest,
) -> Option<NativeBleRnodeRequest> {
    previous.filter(|previous| previous.tcp_port != rejected.tcp_port)
}

#[cfg(any(target_os = "android", test))]
fn owner_after_rejected_ble_replacement(
    current: Option<NativeBleRnodeRequest>,
    previous: Option<NativeBleRnodeRequest>,
    rejected: NativeBleRnodeRequest,
) -> Option<NativeBleRnodeRequest> {
    if ble_request_matches(current.as_ref(), &rejected) {
        failed_ble_replacement_owner(previous, rejected)
    } else {
        current
    }
}

#[cfg(target_os = "android")]
const ANDROID_BRIDGE_CLASS: &str = "org.ratspeak.android.RatspeakNativeBridge";
#[cfg(target_os = "android")]
static ANDROID_CLASS_LOADER: AndroidOnceLock<jni::objects::GlobalRef> = AndroidOnceLock::new();

#[cfg(target_os = "android")]
fn android_start_ble_rnode(request: &NativeBleRnodeRequest) -> bool {
    with_android_bridge(|env, class| {
        use jni::objects::{JObject, JValue};
        let address = env.new_string(&request.address)?;
        let token = env.new_string(&request.activity_operation)?;
        env.call_static_method(
            class,
            "startOrReplaceBleRnode",
            "(Ljava/lang/String;ILjava/lang/String;J)Z",
            &[
                JValue::Object(JObject::from(address)),
                JValue::Int(i32::from(request.tcp_port)),
                JValue::Object(JObject::from(token)),
                JValue::Long(request.native_generation as i64),
            ],
        )?
        .z()
    })
    .unwrap_or(false)
}

#[cfg(target_os = "android")]
fn android_disconnect_ble_rnode(activity_operation: Option<&str>, generation: u64) -> bool {
    with_android_bridge(|env, class| {
        use jni::objects::{JObject, JValue};
        let token = activity_operation
            .map(|token| env.new_string(token))
            .transpose()?;
        let token = token.map(JObject::from).unwrap_or_else(JObject::null);
        env.call_static_method(
            class,
            "disconnectBleRnode",
            "(Ljava/lang/String;J)Z",
            &[JValue::Object(token), JValue::Long(generation as i64)],
        )?
        .z()
    })
    .unwrap_or(false)
}

#[cfg(target_os = "android")]
fn android_request_usb_permission(
    vendor_id: u16,
    product_id: u16,
    serial_number: Option<&str>,
) -> bool {
    with_android_class(
        "org.ratspeak.android.RatspeakPlatformSupervisor",
        |env, class| {
            use jni::objects::{JObject, JValue};
            let serial = serial_number
                .map(|serial| env.new_string(serial))
                .transpose()?
                .map(JObject::from)
                .unwrap_or_else(JObject::null);
            env.call_static_method(
                class,
                "requestUsbPermissionForSelector",
                "(IILjava/lang/String;)V",
                &[
                    JValue::Int(i32::from(vendor_id)),
                    JValue::Int(i32::from(product_id)),
                    JValue::Object(serial),
                ],
            )?;
            Ok(true)
        },
    )
    .unwrap_or(false)
}

#[cfg(target_os = "android")]
fn with_android_bridge<F, T>(call: F) -> Option<T>
where
    F: FnOnce(&jni::JNIEnv, jni::objects::JClass) -> jni::errors::Result<T>,
{
    let vm = rns_interface::android_usb::java_vm()?;
    let env = vm.attach_current_thread().ok()?;
    let class = find_android_class(&env, ANDROID_BRIDGE_CLASS).ok()?;
    match call(&env, class) {
        Ok(value) => Some(value),
        Err(_) => {
            clear_android_exception(&env);
            tracing::warn!(
                reason = "android_native_bridge_call_failed",
                "Android platform operation could not be applied"
            );
            None
        }
    }
}

#[cfg(target_os = "android")]
fn with_android_class<F, T>(class_name: &str, call: F) -> Option<T>
where
    F: FnOnce(&jni::JNIEnv, jni::objects::JClass) -> jni::errors::Result<T>,
{
    let vm = rns_interface::android_usb::java_vm()?;
    let env = vm.attach_current_thread().ok()?;
    let class = find_android_class(&env, class_name).ok()?;
    match call(&env, class) {
        Ok(value) => Some(value),
        Err(_) => {
            clear_android_exception(&env);
            None
        }
    }
}

#[cfg(target_os = "android")]
fn find_android_class<'a>(
    env: &'a jni::JNIEnv,
    class_name: &str,
) -> jni::errors::Result<jni::objects::JClass<'a>> {
    use jni::objects::{JClass, JValue};
    if ANDROID_CLASS_LOADER.get().is_none() {
        let thread = env.find_class("android/app/ActivityThread")?;
        let app = env
            .call_static_method(
                thread,
                "currentApplication",
                "()Landroid/app/Application;",
                &[],
            )?
            .l()?;
        let loader = env
            .call_method(app, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let global = env.new_global_ref(loader)?;
        let _ = ANDROID_CLASS_LOADER.set(global);
    }
    let loader = ANDROID_CLASS_LOADER
        .get()
        .expect("Android class loader initialized");
    let name = env.new_string(class_name)?;
    let class = env
        .call_method(
            loader.as_obj(),
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(name.into())],
        )?
        .l()?;
    Ok(JClass::from(class))
}

#[cfg(target_os = "android")]
fn clear_android_exception(env: &jni::JNIEnv) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

#[cfg(target_os = "android")]
fn decode_android_string(env: &jni::JNIEnv, value: jni::objects::JString) -> Option<String> {
    if value.is_null() {
        return None;
    }
    match env.get_string(value) {
        Ok(value) => Some(value.to_string_lossy().into_owned()),
        Err(_) => {
            clear_android_exception(env);
            None
        }
    }
}

#[cfg(target_os = "android")]
fn pending_ble_request(token: &str, generation: u64) -> Option<NativeBleRnodeRequest> {
    PENDING_BLE_RNODE.lock().ok().and_then(|pending| {
        pending
            .as_ref()
            .filter(|pending| {
                pending.activity_operation == token && pending.native_generation == generation
            })
            .cloned()
    })
}

#[cfg(target_os = "android")]
fn take_pending_ble_request(token: &str, generation: u64) -> Option<NativeBleRnodeRequest> {
    let mut pending = PENDING_BLE_RNODE.lock().ok()?;
    if pending.as_ref().is_some_and(|pending| {
        pending.activity_operation == token && pending.native_generation == generation
    }) {
        pending.take()
    } else {
        None
    }
}

#[cfg(target_os = "android")]
const fn native_ble_failure_code(
    code: &str,
) -> ratspeak_tauri::commands::ble::BleRnodeNativeFailureCode {
    use ratspeak_tauri::commands::ble::BleRnodeNativeFailureCode;
    match code {
        "bond_timeout" => BleRnodeNativeFailureCode::BondTimeout,
        "bridge_unavailable" => BleRnodeNativeFailureCode::SetupTimeout,
        _ => BleRnodeNativeFailureCode::ConnectFailed,
    }
}

#[cfg(target_os = "android")]
const fn native_ble_hardware_reason(code: &str) -> &'static str {
    match code {
        "bluetooth_off" => "bluetooth_off",
        "permission_needed" => "permission_needed",
        "pairing_required" => "pairing_required",
        "bond_timeout" => "bond_timeout",
        "stale_bond" => "stale_bond",
        "bridge_unavailable" => "bridge_unavailable",
        "radio_disconnected" => "radio_disconnected",
        _ => "connect_failed",
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeBleRnodeState(
    env: jni::JNIEnv,
    _class: jni::objects::JClass,
    activity_operation: jni::objects::JString,
    generation: jni::sys::jlong,
    state_code: jni::sys::jint,
    tcp_port: jni::sys::jint,
    error_code: jni::objects::JString,
) {
    let Some(activity_operation) = decode_android_string(&env, activity_operation) else {
        return;
    };
    let Ok(generation) = u64::try_from(generation) else {
        return;
    };
    let Some(request) = pending_ble_request(&activity_operation, generation) else {
        return;
    };
    let Some(app_state) = installed_state() else {
        return;
    };
    match state_code {
        0 | 1 | 3 => {
            // Product status is emitted as a closed state without address,
            // device name, or platform error detail.
            app_state.publish_mobile_hardware_state(
                "ble_rnode",
                match state_code {
                    0 => "connecting",
                    1 => "reconnecting",
                    _ => "connected",
                },
                None,
            );
        }
        2 => {
            let Ok(tcp_port) = u16::try_from(tcp_port) else {
                return;
            };
            if tcp_port == 0 || tcp_port != request.tcp_port {
                return;
            }
            tauri::async_runtime::spawn(async move {
                let args = ratspeak_tauri::commands::ble::BleRnodeBridgeArgs {
                    activity_operation: request.activity_operation,
                    tcp_port,
                    name: request.name,
                    port: request.port,
                    frequency: request.frequency,
                    bandwidth: request.bandwidth,
                    spreading_factor: request.spreading_factor,
                    coding_rate: request.coding_rate,
                    tx_power: request.tx_power,
                    mode: request.mode,
                    native_mode: Some(request.mode_value),
                    airtime_limit_short: request.airtime_limit_short,
                    airtime_limit_long: request.airtime_limit_long,
                    id_interval: request.id_interval,
                    id_callsign: request.id_callsign,
                    saved_startup: request.saved_startup,
                };
                if ratspeak_tauri::commands::ble::apply_ble_rnode_bridge_ready(app_state, args)
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        reason = "native_ble_rnode_handoff_failed",
                        "Android BLE RNode handoff could not be applied"
                    );
                }
            });
        }
        4 => {
            // Exact terminal callback owns removal; a callback from an older
            // generation can never clear its replacement.
            if take_pending_ble_request(&activity_operation, generation).is_none() {
                return;
            }
            let code = decode_android_string(&env, error_code)
                .filter(|code| {
                    matches!(
                        code.as_str(),
                        "bluetooth_off"
                            | "permission_needed"
                            | "pairing_required"
                            | "bond_timeout"
                            | "stale_bond"
                            | "bridge_unavailable"
                            | "connect_failed"
                            | "radio_disconnected"
                    )
                })
                .unwrap_or_else(|| "connect_failed".to_string());
            app_state.publish_mobile_hardware_state(
                "ble_rnode",
                "failed",
                Some(native_ble_hardware_reason(&code)),
            );
            tauri::async_runtime::spawn(async move {
                let args = ratspeak_tauri::commands::ble::BleRnodeBridgeFailureArgs {
                    activity_operation,
                    failure_code: native_ble_failure_code(&code),
                };
                let _ =
                    ratspeak_tauri::commands::ble::apply_ble_rnode_bridge_failed(app_state, args)
                        .await;
            });
        }
        5 => {
            app_state.publish_mobile_hardware_state("ble_rnode", "disabled", None);
            let _ = take_pending_ble_request(&activity_operation, generation);
        }
        _ => {}
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeUsbDeviceEvent(
    env: jni::JNIEnv,
    _class: jni::objects::JClass,
    action: jni::sys::jint,
    device_name: jni::objects::JString,
    vendor_id: jni::sys::jint,
    product_id: jni::sys::jint,
    serial: jni::objects::JString,
    permission: jni::sys::jboolean,
    sequence: jni::sys::jlong,
) {
    if !matches!(action, 1..=4) || vendor_id < 0 || product_id < 0 || permission > 1 {
        return;
    }
    let Ok(sequence) = u64::try_from(sequence) else {
        return;
    };
    if sequence == 0 || !accept_monotonic_sequence(&LAST_USB_SEQUENCE, sequence) {
        return;
    }
    let device_name = decode_android_string(&env, device_name)
        .filter(|value| !value.is_empty() && value.len() <= 256);
    // Decode solely to bound and validate the ambient boundary. Stable
    // selectors are resolved/persisted by the Rust configuration path; raw
    // serial and device names never enter Activity or product logs here.
    let serial_valid = decode_android_string(&env, serial)
        .is_none_or(|serial| !serial.is_empty() && serial.len() <= 256);
    if !serial_valid {
        return;
    }
    match action {
        2 => {
            if let Some(device_name) = device_name {
                rns_interface::android_usb::notify_android_usb_device_detached(&device_name);
            }
        }
        1 | 3 | 4 => rns_interface::android_usb::notify_android_usb_devices_changed(),
        _ => {}
    }
    if let Some(state) = installed_state() {
        let hardware_state = match action {
            1 => "attached",
            2 => "detached",
            3 if permission != 0 => "permission_granted",
            3 => "permission_needed",
            4 => "inventory_changed",
            _ => return,
        };
        state.publish_mobile_hardware_state("usb_rnode", hardware_state, None);
    }
}

pub(crate) fn submit_lifecycle(foreground: bool) {
    let Some(state) = installed_state() else {
        return;
    };
    // Tauri RunEvents are delivered only after setup has installed AppState.
    // Allocate authority before spawning so scheduling cannot invert two edges.
    let transition = state.begin_foreground_transition();
    tauri::async_runtime::spawn(async move {
        if ratspeak_tauri::commands::system::apply_foreground_transition(
            state, foreground, transition,
        )
        .await
        .is_err()
        {
            tracing::warn!(
                reason = "native_lifecycle_apply_failed",
                "native lifecycle transition could not be applied"
            );
        }
    });
}

/// Submit a trusted in-process platform transition (currently iOS
/// Network.framework) with a process-monotonic sequence.
pub(crate) fn submit_native_network(network_type: &str) {
    let sequence = NATIVE_NETWORK_SEQUENCE
        .fetch_add(1, Ordering::AcqRel)
        .saturating_add(1);
    submit_network(network_type, sequence);
}

pub(crate) fn submit_network(network_type: &str, sequence: u64) {
    let Some(network_type) = NetworkType::parse(network_type) else {
        tracing::warn!(
            reason = "invalid_native_network_type",
            "ignored invalid native network transition"
        );
        return;
    };
    if sequence == 0 || !accept_sequence(sequence) {
        return;
    }

    let state = installed_state();
    if let Ok(mut pending) = PENDING_NETWORK.lock() {
        if pending
            .as_ref()
            .is_none_or(|pending| sequence > pending.sequence)
        {
            *pending = Some(PendingNetwork {
                sequence,
                network_type,
                transition: state.as_ref().map(|state| state.begin_network_transition()),
            });
        }
    }
    if state.is_some() {
        ensure_network_worker();
    }
}

fn installed_state() -> Option<Arc<AppState>> {
    APP_STATE
        .get()
        .and_then(|slot| slot.read().ok())
        .and_then(|state| state.upgrade())
}

fn accept_sequence(sequence: u64) -> bool {
    accept_monotonic_sequence(&LAST_NETWORK_SEQUENCE, sequence)
}

fn accept_monotonic_sequence(counter: &AtomicU64, sequence: u64) -> bool {
    let mut observed = counter.load(Ordering::Acquire);
    loop {
        if sequence <= observed {
            return false;
        }
        match counter.compare_exchange_weak(observed, sequence, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return true,
            Err(current) => observed = current,
        }
    }
}

fn ensure_network_worker() {
    if NETWORK_WORKER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            let pending = PENDING_NETWORK
                .lock()
                .ok()
                .and_then(|mut pending| pending.take());
            let Some(mut pending) = pending else {
                NETWORK_WORKER_RUNNING.store(false, Ordering::Release);
                // Close the hand-off race with a callback that arrived between
                // the empty read and clearing the running flag.
                let has_pending = PENDING_NETWORK
                    .lock()
                    .is_ok_and(|pending| pending.is_some());
                if has_pending
                    && NETWORK_WORKER_RUNNING
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                return;
            };
            let Some(state) = installed_state() else {
                if let Ok(mut slot) = PENDING_NETWORK.lock() {
                    *slot = Some(pending);
                }
                NETWORK_WORKER_RUNNING.store(false, Ordering::Release);
                return;
            };
            let transition = pending
                .transition
                .take()
                .unwrap_or_else(|| state.begin_network_transition());
            if ratspeak_tauri::commands::interfaces::apply_network_type_change_transition(
                state,
                pending.network_type.as_str().to_string(),
                transition,
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    reason = "native_network_apply_failed",
                    "native network transition could not be applied"
                );
            }
        }
    });
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_org_ratspeak_android_RatspeakNativeBridge_nativeSetNetworkType(
    mut env: jni::JNIEnv,
    _class: jni::objects::JClass,
    network_type: jni::objects::JString,
    sequence: jni::sys::jlong,
) {
    let value = match env.get_string(network_type) {
        Ok(value) => value.to_string_lossy().into_owned(),
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            tracing::warn!(
                reason = "native_network_decode_failed",
                "ignored unreadable native network transition"
            );
            return;
        }
    };
    let Ok(sequence) = u64::try_from(sequence) else {
        return;
    };
    submit_network(&value, sequence);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{
        accept_sequence, failed_ble_replacement_owner, owner_after_rejected_ble_replacement,
        valid_ble_native_request, NetworkType,
    };
    use ratspeak_tauri::mobile_platform::NativeBleRnodeRequest;

    fn ble_request(token: char, generation: u64, tcp_port: u16) -> NativeBleRnodeRequest {
        NativeBleRnodeRequest {
            address: "00:11:22:33:44:55".to_string(),
            tcp_port,
            activity_operation: token.to_string().repeat(32),
            native_generation: generation,
            name: "RNode".to_string(),
            port: "ble://00:11:22:33:44:55".to_string(),
            frequency: 915_000_000,
            bandwidth: 125_000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 14,
            mode: Some("roaming".to_string()),
            mode_value: 2,
            airtime_limit_short: None,
            airtime_limit_long: None,
            id_interval: None,
            id_callsign: None,
            saved_startup: false,
        }
    }

    #[test]
    fn network_type_is_a_closed_vocabulary() {
        for value in ["wifi", "cellular", "ethernet", "none", "unknown"] {
            assert!(NetworkType::parse(value).is_some(), "{value}");
        }
        for value in ["", "WIFI", "vpn", "wifi;alert(1)"] {
            assert!(NetworkType::parse(value).is_none(), "{value}");
        }
    }

    #[test]
    fn network_sequence_rejects_stale_and_duplicate_callbacks() {
        let base = super::LAST_NETWORK_SEQUENCE.fetch_add(10, Ordering::AcqRel) + 10;
        assert!(accept_sequence(base + 1));
        assert!(!accept_sequence(base + 1));
        assert!(!accept_sequence(base));
        assert!(accept_sequence(base + 2));
    }

    #[test]
    fn rejected_different_port_replacement_retains_prior_owner() {
        let prior = ble_request('a', 1, 31_000);
        let rejected = ble_request('b', 2, 31_001);
        let retained = failed_ble_replacement_owner(Some(prior.clone()), rejected)
            .expect("different-port rejection retains the live prior owner");
        assert_eq!(retained.activity_operation, prior.activity_operation);
        assert_eq!(retained.native_generation, prior.native_generation);
        assert_eq!(retained.tcp_port, prior.tcp_port);
    }

    #[test]
    fn rejected_same_port_replacement_leaves_no_owner() {
        let prior = ble_request('a', 1, 31_000);
        let rejected = ble_request('b', 2, 31_000);
        assert!(failed_ble_replacement_owner(Some(prior), rejected).is_none());
        assert!(failed_ble_replacement_owner(None, ble_request('b', 2, 31_000)).is_none());
    }

    #[test]
    fn rejected_replacement_never_resurrects_after_terminal_callback() {
        let prior = ble_request('a', 1, 31_000);
        let rejected = ble_request('b', 2, 31_001);
        assert!(owner_after_rejected_ble_replacement(None, Some(prior), rejected).is_none());
    }

    #[test]
    fn native_ble_request_validator_matches_kotlin_boundary() {
        let valid = ble_request('a', i64::MAX as u64, 31_000);
        assert!(valid_ble_native_request(&valid));

        let mut invalid = valid.clone();
        invalid.activity_operation = "g".repeat(32);
        assert!(!valid_ble_native_request(&invalid));
        invalid = valid.clone();
        invalid.activity_operation = "a".repeat(31);
        assert!(!valid_ble_native_request(&invalid));
        invalid = valid.clone();
        invalid.address = "001122334455".to_string();
        assert!(!valid_ble_native_request(&invalid));
        invalid = valid.clone();
        invalid.address = "00:11:22:33:44:gg".to_string();
        assert!(!valid_ble_native_request(&invalid));
        invalid = valid.clone();
        invalid.tcp_port = 0;
        assert!(!valid_ble_native_request(&invalid));
        invalid = valid;
        invalid.native_generation = i64::MAX as u64 + 1;
        assert!(!valid_ble_native_request(&invalid));
    }
}
