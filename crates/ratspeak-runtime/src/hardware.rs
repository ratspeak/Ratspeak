//! YubiKey/Nitrokey (PIV) hardware-identity provisioning + app registration.
//! Backs the ratspeak-tauri `hw_*` commands. Gated behind the `hardware` feature.
//!
//! **Desktop only at release.** `hardware` is off on mobile (pcsc is desktop-only);
//! the hw_* commands and frontend entry points are gated to desktop too. A mobile
//! hardware identity needs a different design — a wrapped software session unlocked
//! via on-card ECDH — because transient NFC can't do always-on signing.
//! TODO(ratkey-mobile): see rns-ratkey/HARDWARE_STATUS.md.

use std::path::Path;

use rns_identity::destination::Destination;
use rns_ratkey::mock::TouchPolicy;
use rns_ratkey::provision::{self, ProvisionConfig};
use rns_ratkey::{PcscPivSession, RatkeyError};

use crate::state::DbPool;

const LXMF_APP_NAME: &str = "lxmf.delivery";
const DEFAULT_PIN: &str = "123456";
/// Factory-default PIV management key (AES-192 on YubiKey 5.7+).
const DEFAULT_MGMT_KEY: [u8; 24] = [
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
];
pub const NOT_DETECTED: &str =
    "YubiKey not detected. Please make sure it's a YubiKey 5+ running the latest firmware.";

#[derive(serde::Serialize)]
pub struct HwDetect {
    pub detected: bool,
    pub device_type: String,
    pub serial: Option<u32>,
    pub firmware: Option<String>,
    pub firmware_ok: bool,
    pub error: Option<String>,
    /// An app identity already backed by this physical key (matched by serial).
    /// Present means provisioning would overwrite registered or on-card keys.
    pub existing: Option<HwExisting>,
}

#[derive(serde::Serialize)]
pub struct HwExisting {
    pub hash: String,
    pub nickname: String,
    /// True when slots are occupied but no local app identity matches them.
    #[serde(default)]
    pub on_card_only: bool,
}

#[derive(serde::Serialize)]
pub struct HwProvisioned {
    pub hash: String,
    pub lxmf_hash: String,
    /// Present only for recoverable provisioning — shown to the user once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
}

/// PIV native Ed25519/X25519 needs YubiKey firmware >= 5.7.0.
fn firmware_ok(fw: Option<&str>) -> bool {
    let Some(fw) = fw else { return false };
    let mut parts = fw.split('.').filter_map(|p| p.parse::<u32>().ok());
    let maj = parts.next().unwrap_or(0);
    let min = parts.next().unwrap_or(0);
    maj > 5 || (maj == 5 && min >= 7)
}

pub fn detect(data_dir: &Path) -> HwDetect {
    let device = rns_ratkey::detect::detect_devices()
        .ok()
        .and_then(|d| d.into_iter().next());
    match device {
        Some(d) => {
            let ok = firmware_ok(d.firmware.as_deref());
            let existing = d
                .serial
                .and_then(|s| find_identity_by_serial(data_dir, s))
                .or_else(|| {
                    (d.has_signing_key || d.has_encryption_key).then(|| HwExisting {
                        hash: String::new(),
                        nickname: String::new(),
                        on_card_only: true,
                    })
                });
            HwDetect {
                detected: true,
                device_type: d.device_type,
                serial: d.serial,
                firmware: d.firmware,
                firmware_ok: ok,
                error: (!ok).then(|| NOT_DETECTED.to_string()),
                existing,
            }
        }
        None => HwDetect {
            detected: false,
            device_type: String::new(),
            serial: None,
            firmware: None,
            firmware_ok: false,
            error: Some(NOT_DETECTED.to_string()),
            existing: None,
        },
    }
}

/// Find an app identity already backed by this physical key (matched by PIV
/// serial). Used to warn before a provision would overwrite the on-card keys.
fn find_identity_by_serial(data_dir: &Path, serial: u32) -> Option<HwExisting> {
    let dir = data_dir.join("identities");
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let hwid_path = entry.path().join("identity.hwid");
        if !hwid_path.exists() {
            continue;
        }
        if let Ok(cfg) = rns_ratkey::HwidConfig::from_file(&hwid_path)
            && cfg.device.serial == serial
        {
            return Some(HwExisting {
                hash: cfg.identity.hash,
                nickname: cfg.identity.nickname,
                on_card_only: false,
            });
        }
    }
    None
}

fn slot_occupied(session: &mut PcscPivSession, slot: u8) -> bool {
    session.read_metadata(slot).is_ok()
}

/// Refuse to overwrite a key that already backs an app identity unless forced.
fn guard_overwrite(data_dir: &Path, session: &mut PcscPivSession) -> Result<(), String> {
    if let Some(serial) = session.serial()
        && let Some(existing) = find_identity_by_serial(data_dir, serial)
    {
        let who = if existing.nickname.is_empty() {
            "an existing identity".to_string()
        } else {
            format!("identity '{}'", existing.nickname)
        };
        return Err(format!(
            "This YubiKey already holds {who}. Provisioning permanently erases it — confirm to overwrite."
        ));
    }

    let signing_occupied = slot_occupied(session, rns_ratkey::apdu::SLOT_AUTHENTICATION);
    let encryption_occupied = slot_occupied(session, rns_ratkey::apdu::SLOT_KEY_MANAGEMENT);
    if signing_occupied || encryption_occupied {
        return Err(
            "This YubiKey already contains keys in the Ratspeak PIV identity slots. \
             Provisioning permanently erases those keys — confirm to overwrite."
                .to_string(),
        );
    }
    Ok(())
}

pub fn provision_recoverable(
    data_dir: &Path,
    db: &DbPool,
    pin: &str,
    current_pin: Option<&str>,
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &mut session)?;
    }
    prepare_for_provisioning(&mut session, pin, current_pin)?;
    let cfg = base_config(data_dir, nickname);
    let (result, mnemonic) =
        provision::provision_recoverable(&mut session, &DEFAULT_MGMT_KEY, &cfg)
            .map_err(|e| e.to_string())?;
    let lxmf_hash = register(
        data_dir,
        db,
        &result.identity_hash_hex,
        &result.identity_hash,
        nickname,
    )?;
    Ok(HwProvisioned {
        hash: result.identity_hash_hex,
        lxmf_hash,
        mnemonic: Some(mnemonic),
    })
}

pub fn provision_hardware_only(
    data_dir: &Path,
    db: &DbPool,
    pin: &str,
    current_pin: Option<&str>,
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &mut session)?;
    }
    prepare_for_provisioning(&mut session, pin, current_pin)?;
    let cfg = base_config(data_dir, nickname);
    let result = provision::provision_hardware_only(&mut session, &DEFAULT_MGMT_KEY, &cfg)
        .map_err(|e| e.to_string())?;
    let lxmf_hash = register(
        data_dir,
        db,
        &result.identity_hash_hex,
        &result.identity_hash,
        nickname,
    )?;
    Ok(HwProvisioned {
        hash: result.identity_hash_hex,
        lxmf_hash,
        mnemonic: None,
    })
}

/// Register a YubiKey that is already provisioned (no key creation, no PIN change).
pub fn import_existing(
    data_dir: &Path,
    db: &DbPool,
    nickname: &str,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    let cfg = base_config(data_dir, nickname);
    let result = provision::read_existing(&mut session, &cfg).map_err(|e| e.to_string())?;
    let lxmf_hash = register(
        data_dir,
        db,
        &result.identity_hash_hex,
        &result.identity_hash,
        nickname,
    )?;
    Ok(HwProvisioned {
        hash: result.identity_hash_hex,
        lxmf_hash,
        mnemonic: None,
    })
}

pub fn restore(
    data_dir: &Path,
    db: &DbPool,
    phrase: &str,
    pin: &str,
    current_pin: Option<&str>,
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &mut session)?;
    }
    prepare_for_provisioning(&mut session, pin, current_pin)?;
    let cfg = base_config(data_dir, nickname);
    let result = provision::restore(&mut session, &DEFAULT_MGMT_KEY, &cfg, phrase)
        .map_err(|e| e.to_string())?;
    let lxmf_hash = register(
        data_dir,
        db,
        &result.identity_hash_hex,
        &result.identity_hash,
        nickname,
    )?;
    Ok(HwProvisioned {
        hash: result.identity_hash_hex,
        lxmf_hash,
        mnemonic: None,
    })
}

/// Remove a hardware identity from the app (DB row + `.hwid`). The key remains
/// on the token — this only forgets it locally.
pub fn remove(data_dir: &Path, db: &DbPool, hash_hex: &str) -> Result<(), String> {
    ratspeak_db::delete_identity(db, hash_hex, true)?;
    let id_dir = data_dir.join("identities").join(hash_hex);
    let _ = std::fs::remove_dir_all(&id_dir);
    Ok(())
}

pub fn reset_piv_application() -> Result<(), String> {
    let mut session = connect()?;
    block_pin_for_reset(&mut session)?;
    block_recovery_counter_for_reset(&mut session)?;
    session.reset_piv().map_err(format_reset_piv_error)
}

pub fn change_pin(
    data_dir: &Path,
    hash_hex: &str,
    current_pin: &str,
    new_pin: &str,
) -> Result<(), String> {
    let hwid_path = data_dir
        .join("identities")
        .join(hash_hex)
        .join("identity.hwid");
    let cfg = rns_ratkey::HwidConfig::from_file(&hwid_path)
        .map_err(|_| "Hardware identity not found on this device.".to_string())?;
    let mut session = connect()?;
    if session.serial() != Some(cfg.device.serial) {
        return Err("Inserted YubiKey does not match this identity.".to_string());
    }
    let expected_ed = cfg
        .ed25519_pub_bytes()
        .map_err(|e| format!("Invalid hardware identity metadata: {e}"))?;
    let expected_x = cfg
        .x25519_pub_bytes()
        .map_err(|e| format!("Invalid hardware identity metadata: {e}"))?;
    let ed = session
        .read_public_key(rns_ratkey::apdu::SLOT_AUTHENTICATION)
        .map_err(|e| format!("could not verify YubiKey signing key: {e}"))?;
    let x = session
        .read_public_key(rns_ratkey::apdu::SLOT_KEY_MANAGEMENT)
        .map_err(|e| format!("could not verify YubiKey encryption key: {e}"))?;
    if ed != expected_ed || x != expected_x {
        return Err("Inserted YubiKey does not match this identity.".to_string());
    }
    session
        .change_pin(current_pin, new_pin)
        .map_err(format_pin_change_error)
}

fn connect() -> Result<PcscPivSession, String> {
    PcscPivSession::connect().map_err(|_| NOT_DETECTED.to_string())
}

fn base_config(data_dir: &Path, nickname: &str) -> ProvisionConfig {
    ProvisionConfig {
        pin: String::new(),
        touch_signing: TouchPolicy::Never,
        touch_encryption: TouchPolicy::Never,
        nickname: nickname.to_string(),
        identities_dir: Some(data_dir.join("identities")),
    }
}

fn prepare_for_provisioning(
    session: &mut PcscPivSession,
    pin: &str,
    current_pin: Option<&str>,
) -> Result<(), String> {
    session
        .authenticate_management_key(&DEFAULT_MGMT_KEY)
        .map_err(|e| format!("could not authenticate YubiKey management key: {e}"))?;
    set_pin(session, pin, current_pin)
}

fn set_pin(
    session: &mut PcscPivSession,
    pin: &str,
    current_pin: Option<&str>,
) -> Result<(), String> {
    if let Some(current_pin) = current_pin.filter(|p| !p.is_empty()) {
        session
            .verify_pin(current_pin)
            .map_err(format_pin_setup_error)?;
        if current_pin != pin {
            session
                .change_pin(current_pin, pin)
                .map_err(format_pin_setup_error)?;
        }
        return Ok(());
    }

    if pin == DEFAULT_PIN {
        return session
            .verify_pin(DEFAULT_PIN)
            .map_err(format_pin_setup_error);
    }
    session
        .change_pin(DEFAULT_PIN, pin)
        .map_err(format_pin_setup_error)
}

fn format_pin_setup_error(e: RatkeyError) -> String {
    match e {
        RatkeyError::PinLocked => {
            "YubiKey PIV PIN is locked. Reset the PIV application before provisioning. Resetting PIV erases the Ratspeak keys on that YubiKey.".to_string()
        }
        RatkeyError::PinFailed { remaining } => format!(
            "YubiKey PIV PIN is not at the factory default ({} attempt{} remaining). Reset the security key to set up a new Ratspeak identity, or use it as an existing key with its current PIN.",
            remaining,
            if remaining == 1 { "" } else { "s" }
        ),
        other => format!("could not prepare YubiKey PIN: {other}"),
    }
}

fn format_pin_change_error(e: RatkeyError) -> String {
    match e {
        RatkeyError::PinLocked => {
            "YubiKey PIV PIN is locked. Reset the security key to use it again.".to_string()
        }
        RatkeyError::PinFailed { remaining } => format!(
            "Incorrect current YubiKey PIN ({} attempt{} remaining).",
            remaining,
            if remaining == 1 { "" } else { "s" }
        ),
        other => format!("could not change YubiKey PIN: {other}"),
    }
}

fn block_pin_for_reset(session: &mut PcscPivSession) -> Result<(), String> {
    for _ in 0..260 {
        match session.verify_pin(&wrong_piv_code()) {
            Err(RatkeyError::PinLocked) => return Ok(()),
            Err(RatkeyError::PinFailed { .. }) => continue,
            Ok(()) => continue,
            Err(e) => return Err(format!("could not prepare YubiKey PIN for reset: {e}")),
        }
    }
    Err("could not prepare YubiKey PIN for reset".to_string())
}

fn block_recovery_counter_for_reset(session: &mut PcscPivSession) -> Result<(), String> {
    for _ in 0..260 {
        let wrong_recovery_code = wrong_piv_code();
        let new_pin = wrong_piv_code();
        match session.unblock_pin(&wrong_recovery_code, &new_pin) {
            Err(RatkeyError::PukLocked) => return Ok(()),
            Err(RatkeyError::PukFailed { .. }) => continue,
            Ok(()) => {
                // A random value unexpectedly matched the recovery code and reset
                // the PIN. Re-block the PIN, then continue preparing for reset.
                block_pin_for_reset(session)?;
            }
            Err(e) => {
                return Err(format!(
                    "could not prepare YubiKey recovery counter for reset: {e}"
                ));
            }
        }
    }
    Err("could not prepare YubiKey recovery counter for reset".to_string())
}

fn wrong_piv_code() -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn format_reset_piv_error(e: RatkeyError) -> String {
    match e {
        RatkeyError::ResetRequiresBlockedPinAndPuk => {
            "YubiKey refused PIV reset because the PIN and recovery counter were not locked."
                .to_string()
        }
        RatkeyError::PukLocked => {
            "YubiKey PIV recovery counter is locked, but reset did not complete.".to_string()
        }
        other => format!("could not reset YubiKey PIV application: {other}"),
    }
}

/// Compute the LXMF destination hash + insert the `identities` DB row. The `.hwid`
/// is already on disk (written during provisioning).
fn register(
    data_dir: &Path,
    db: &DbPool,
    hash_hex: &str,
    identity_hash: &[u8; 16],
    nickname: &str,
) -> Result<String, String> {
    let lxmf_dest = Destination::hash_from_name_and_identity(LXMF_APP_NAME, Some(identity_hash));
    let lxmf_hex = hex::encode(lxmf_dest);
    let id_dir = data_dir.join("identities").join(hash_hex);
    std::fs::create_dir_all(id_dir.join("lxmf")).map_err(|e| format!("identity dir: {e}"))?;
    let display_name = if nickname.is_empty() {
        format!("!Ratspeak.org-{}", &lxmf_hex[..6])
    } else {
        nickname.to_string()
    };
    ratspeak_db::save_identity(db, hash_hex, &lxmf_hex, nickname, &display_name);
    // Activate it so a first-setup restart loads the new hardware identity
    // (otherwise no active identity exists and a software one is generated).
    ratspeak_db::set_active_identity(db, hash_hex).map_err(|e| format!("activate: {e}"))?;
    let active_hash = ratspeak_db::get_active_identity(db)
        .and_then(|identity| {
            identity
                .get("hash")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| "activate: active identity did not persist".to_string())?;
    if active_hash != hash_hex {
        return Err(format!(
            "activate: active identity did not persist (expected {hash_hex}, got {active_hash})"
        ));
    }
    Ok(lxmf_hex)
}
