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
use rns_ratkey::PcscPivSession;
use rns_ratkey::mock::TouchPolicy;
use rns_ratkey::provision::{self, ProvisionConfig};

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
    /// Present means provisioning would overwrite it.
    pub existing: Option<HwExisting>,
}

#[derive(serde::Serialize)]
pub struct HwExisting {
    pub hash: String,
    pub nickname: String,
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
            let existing = d.serial.and_then(|s| find_identity_by_serial(data_dir, s));
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
            });
        }
    }
    None
}

/// Refuse to overwrite a key that already backs an app identity unless forced.
fn guard_overwrite(data_dir: &Path, session: &PcscPivSession) -> Result<(), String> {
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
    Ok(())
}

pub fn provision_recoverable(
    data_dir: &Path,
    db: &DbPool,
    pin: &str,
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &session)?;
    }
    let cfg = base_config(data_dir, nickname);
    let (result, mnemonic) =
        provision::provision_recoverable(&mut session, &DEFAULT_MGMT_KEY, &cfg)
            .map_err(|e| e.to_string())?;
    set_pin(&mut session, pin)?;
    let lxmf_hash = register(data_dir, db, &result.identity_hash_hex, &result.identity_hash, nickname)?;
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
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &session)?;
    }
    let cfg = base_config(data_dir, nickname);
    let result = provision::provision_hardware_only(&mut session, &DEFAULT_MGMT_KEY, &cfg)
        .map_err(|e| e.to_string())?;
    set_pin(&mut session, pin)?;
    let lxmf_hash = register(data_dir, db, &result.identity_hash_hex, &result.identity_hash, nickname)?;
    Ok(HwProvisioned {
        hash: result.identity_hash_hex,
        lxmf_hash,
        mnemonic: None,
    })
}

/// Register a YubiKey that is already provisioned (no key creation, no PIN change).
pub fn import_existing(data_dir: &Path, db: &DbPool, nickname: &str) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    let cfg = base_config(data_dir, nickname);
    let result = provision::read_existing(&mut session, &cfg).map_err(|e| e.to_string())?;
    let lxmf_hash = register(data_dir, db, &result.identity_hash_hex, &result.identity_hash, nickname)?;
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
    nickname: &str,
    force: bool,
) -> Result<HwProvisioned, String> {
    let mut session = connect()?;
    if !force {
        guard_overwrite(data_dir, &session)?;
    }
    let cfg = base_config(data_dir, nickname);
    let result = provision::restore(&mut session, &DEFAULT_MGMT_KEY, &cfg, phrase)
        .map_err(|e| e.to_string())?;
    set_pin(&mut session, pin)?;
    let lxmf_hash = register(data_dir, db, &result.identity_hash_hex, &result.identity_hash, nickname)?;
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

fn set_pin(session: &mut PcscPivSession, pin: &str) -> Result<(), String> {
    if pin == DEFAULT_PIN {
        return Ok(());
    }
    session
        .change_pin(DEFAULT_PIN, pin)
        .map_err(|e| format!("could not set PIN (use a factory-reset YubiKey): {e}"))
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
    Ok(lxmf_hex)
}
