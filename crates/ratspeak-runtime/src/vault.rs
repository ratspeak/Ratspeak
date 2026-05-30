//! At-rest passcode encryption for software identities.
//!
//! Seals the 64-byte Reticulum private key with the user's passcode:
//! ```text
//!   PRK = Argon2id(passcode, salt, {m,t,p})                    (32 B, memory-hard)
//!   KEK = HKDF-SHA256(ikm = PRK, info = canonical(ver,kdf,m,t,p,salt))  (64 B)
//!   blob = token::encrypt(key64, KEK)        (AES-256-CBC + HMAC-SHA256)
//! ```
//! A wrong passcode — or any tampered KDF param (m/t/p/salt are Argon2 inputs and
//! are *also* bound into the KEK via the HKDF `info`, defeating param downgrade) —
//! yields a different KEK, so `token::decrypt` fails authentication. The on-disk
//! `identity.enc` carries the params so a future device can re-derive; only the
//! passcode is secret. The key never leaves memory once unlocked.

use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use zeroize::Zeroizing;

use rns_crypto::{hkdf, random, token};
use rns_identity::identity::Identity;

const VERSION: u32 = 1;
const PRK_LEN: usize = 32;
const KEK_LEN: usize = 64; // token: 32 B HMAC + 32 B AES-256

/// Argon2id cost parameters. Chosen per-platform at encrypt time; decrypt always
/// honors the params stored in the file.
#[derive(Debug, Clone, Copy)]
pub struct VaultParams {
    pub m_cost: u32, // KiB
    pub t_cost: u32,
    pub p_cost: u32,
}

impl VaultParams {
    /// Platform-tuned defaults for *encrypting*. Mobile uses the OWASP floor to
    /// avoid OOM/stall on low-end devices; desktop goes higher.
    pub fn recommended() -> Self {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            VaultParams {
                m_cost: 19 * 1024,
                t_cost: 2,
                p_cost: 1,
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            VaultParams {
                m_cost: 47 * 1024,
                t_cost: 3,
                p_cost: 1,
            }
        }
    }
}

/// On-disk `identity.enc` (JSON). Contains no secret — only the passcode unlocks it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedVault {
    pub version: u32,
    pub kdf: String, // "argon2id"
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,  // hex
    pub token: String, // hex (AES-CBC + HMAC blob) — the 64-byte private key
    /// Optional sealed BIP-39 recovery phrase, encrypted under the same KEK so it
    /// can be revealed later (re-auth with the passcode). Absent for v1 vaults and
    /// for identities imported from a raw key (no phrase to store).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnemonic_token: Option<String>, // hex
}

/// Plaintext recovery-phrase sidecar for *unprotected* software identities. The
/// phrase is crypto-equivalent to the already-plaintext `identity` key file, so it
/// adds no new at-rest exposure; setting a passcode folds it into the vault.
const SEED_FILE: &str = "identity.seed";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("key expansion failed")]
    Hkdf,
    #[error("incorrect passcode or corrupt vault")]
    Auth,
    #[error("invalid vault: {0}")]
    Invalid(String),
    #[error("vault io: {0}")]
    Io(String),
}

/// Bind version + kdf + params + salt into the KEK so unauthenticated file fields
/// cannot be downgraded without breaking decryption.
fn canonical_info(p: VaultParams, salt: &[u8]) -> Vec<u8> {
    let mut v = format!(
        "ratspeak-vault-v{VERSION}|argon2id|{}|{}|{}|",
        p.m_cost, p.t_cost, p.p_cost
    )
    .into_bytes();
    v.extend_from_slice(salt);
    v
}

fn derive_kek(
    passcode: &str,
    salt: &[u8],
    p: VaultParams,
) -> Result<Zeroizing<[u8; KEK_LEN]>, VaultError> {
    let params = Params::new(p.m_cost, p.t_cost, p.p_cost, Some(PRK_LEN))
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut prk = Zeroizing::new([0u8; PRK_LEN]);
    argon
        .hash_password_into(passcode.as_bytes(), salt, prk.as_mut())
        .map_err(|e| VaultError::Kdf(e.to_string()))?;
    let okm = hkdf::hkdf_sha256(KEK_LEN, prk.as_ref(), None, Some(&canonical_info(p, salt)))
        .map_err(|_| VaultError::Hkdf)?;
    let mut kek = Zeroizing::new([0u8; KEK_LEN]);
    kek.copy_from_slice(&okm);
    Ok(kek)
}

/// Seal a 64-byte private identity key under `passcode`.
pub fn encrypt_key(passcode: &str, key: &[u8; 64]) -> Result<EncryptedVault, VaultError> {
    encrypt_identity(passcode, key, None)
}

/// Seal a 64-byte key and, optionally, its BIP-39 recovery phrase under one KEK.
pub fn encrypt_identity(
    passcode: &str,
    key: &[u8; 64],
    mnemonic: Option<&str>,
) -> Result<EncryptedVault, VaultError> {
    let p = VaultParams::recommended();
    let salt = random::random_16();
    let kek = derive_kek(passcode, &salt, p)?;
    let blob = token::encrypt(key, kek.as_ref()).map_err(|e| VaultError::Invalid(e.to_string()))?;
    let mnemonic_token = match mnemonic {
        Some(m) => {
            let mb = token::encrypt(m.as_bytes(), kek.as_ref())
                .map_err(|e| VaultError::Invalid(e.to_string()))?;
            Some(hex::encode(mb))
        }
        None => None,
    };
    Ok(EncryptedVault {
        version: VERSION,
        kdf: "argon2id".into(),
        m_cost: p.m_cost,
        t_cost: p.t_cost,
        p_cost: p.p_cost,
        salt: hex::encode(salt),
        token: hex::encode(blob),
        mnemonic_token,
    })
}

/// Recover the 64-byte private key from a vault. Wrong passcode / tamper → `Auth`.
pub fn decrypt_key(passcode: &str, v: &EncryptedVault) -> Result<Zeroizing<[u8; 64]>, VaultError> {
    if v.version != VERSION {
        return Err(VaultError::Invalid(format!(
            "unsupported version {}",
            v.version
        )));
    }
    if v.kdf != "argon2id" {
        return Err(VaultError::Invalid(format!("unsupported kdf {}", v.kdf)));
    }
    let salt = hex::decode(&v.salt).map_err(|_| VaultError::Invalid("salt".into()))?;
    let blob = hex::decode(&v.token).map_err(|_| VaultError::Invalid("token".into()))?;
    let p = VaultParams {
        m_cost: v.m_cost,
        t_cost: v.t_cost,
        p_cost: v.p_cost,
    };
    let kek = derive_kek(passcode, &salt, p)?;
    let pt = token::decrypt(&blob, kek.as_ref()).map_err(|_| VaultError::Auth)?;
    if pt.len() != 64 {
        return Err(VaultError::Invalid("decrypted key length".into()));
    }
    let mut key = Zeroizing::new([0u8; 64]);
    key.copy_from_slice(&pt);
    Ok(key)
}

/// Recover the sealed BIP-39 phrase from a vault, or `None` if the vault stores no
/// phrase. Wrong passcode / tamper → `Auth` (the phrase shares the key's KEK).
pub fn decrypt_mnemonic(
    passcode: &str,
    v: &EncryptedVault,
) -> Result<Option<Zeroizing<String>>, VaultError> {
    let Some(ref mt) = v.mnemonic_token else {
        return Ok(None);
    };
    if v.version != VERSION {
        return Err(VaultError::Invalid(format!(
            "unsupported version {}",
            v.version
        )));
    }
    if v.kdf != "argon2id" {
        return Err(VaultError::Invalid(format!("unsupported kdf {}", v.kdf)));
    }
    let salt = hex::decode(&v.salt).map_err(|_| VaultError::Invalid("salt".into()))?;
    let blob = hex::decode(mt).map_err(|_| VaultError::Invalid("mnemonic_token".into()))?;
    let p = VaultParams {
        m_cost: v.m_cost,
        t_cost: v.t_cost,
        p_cost: v.p_cost,
    };
    let kek = derive_kek(passcode, &salt, p)?;
    let pt = token::decrypt(&blob, kek.as_ref()).map_err(|_| VaultError::Auth)?;
    let phrase = String::from_utf8(pt).map_err(|_| VaultError::Invalid("mnemonic utf8".into()))?;
    Ok(Some(Zeroizing::new(phrase)))
}

pub fn write_vault(path: &Path, v: &EncryptedVault) -> Result<(), VaultError> {
    let json = serde_json::to_vec_pretty(v).map_err(|e| VaultError::Io(e.to_string()))?;
    // Atomic: write to a temp then rename, so a crash never leaves a partial vault.
    let tmp = path.with_extension("enc.tmp");
    atomic_secret_write(path, &tmp, &json)
}

pub fn read_vault(path: &Path) -> Result<EncryptedVault, VaultError> {
    let bytes = std::fs::read(path).map_err(|e| VaultError::Io(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| VaultError::Invalid(e.to_string()))
}

/// Add or change a passcode on a software identity at `id_dir`. `current` (the old
/// passcode) is required when the identity is already protected. Hardware (`.hwid`)
/// identities are rejected. Writes `identity.enc` and verifies it decrypts before
/// removing the plaintext `identity`, so an interrupted call can't lose the key.
pub fn protect_identity(
    id_dir: &Path,
    passcode: &str,
    current: Option<&str>,
) -> Result<(), VaultError> {
    if passcode.len() < 6 {
        return Err(VaultError::Invalid(
            "passcode must be at least 6 characters".into(),
        ));
    }
    if id_dir.join("identity.hwid").exists() {
        return Err(VaultError::Invalid(
            "hardware identity is unlocked with its PIN, not a passcode".into(),
        ));
    }
    let id_file = id_dir.join("identity");
    let enc_file = id_dir.join("identity.enc");
    let seed_file = id_dir.join(SEED_FILE);

    // Recover the key and any recovery phrase, carrying the phrase forward so a
    // passcode change doesn't lose it.
    let (key, mnemonic): (Zeroizing<[u8; 64]>, Option<Zeroizing<String>>) = if enc_file.exists() {
        let cur = current
            .ok_or_else(|| VaultError::Invalid("current passcode required to change".into()))?;
        let v = read_vault(&enc_file)?;
        (decrypt_key(cur, &v)?, decrypt_mnemonic(cur, &v)?)
    } else if id_file.exists() {
        let id = Identity::from_file(&id_file)
            .map_err(|e| VaultError::Invalid(format!("read identity: {e}")))?;
        let key = id
            .get_private_key()
            .ok_or_else(|| VaultError::Invalid("identity has no private key".into()))?;
        let mnemonic = std::fs::read_to_string(&seed_file)
            .ok()
            .map(|s| Zeroizing::new(s.trim().to_string()));
        (key, mnemonic)
    } else {
        return Err(VaultError::Invalid("identity not found".into()));
    };

    let vault = encrypt_identity(passcode, &key, mnemonic.as_ref().map(|m| m.as_str()))?;
    write_vault(&enc_file, &vault)?;
    // Read it back from disk and confirm both secrets decrypt before destroying the
    // plaintext sources.
    let stored = read_vault(&enc_file)?;
    let check = decrypt_key(passcode, &stored)?;
    let mnemonic_ok = decrypt_mnemonic(passcode, &stored)?.map(|m| m.as_str().to_string())
        == mnemonic.as_ref().map(|m| m.as_str().to_string());
    if check.as_ref() != key.as_ref() || !mnemonic_ok {
        let _ = std::fs::remove_file(&enc_file);
        return Err(VaultError::Invalid("vault verification failed".into()));
    }
    if id_file.exists() {
        std::fs::remove_file(&id_file).map_err(|e| VaultError::Io(e.to_string()))?;
    }
    if seed_file.exists() {
        std::fs::remove_file(&seed_file).map_err(|e| VaultError::Io(e.to_string()))?;
    }
    Ok(())
}

/// Remove a passcode: decrypt `identity.enc` back to a plaintext `identity` file.
pub fn unprotect_identity(id_dir: &Path, passcode: &str) -> Result<(), VaultError> {
    let enc_file = id_dir.join("identity.enc");
    let id_file = id_dir.join("identity");
    let seed_file = id_dir.join(SEED_FILE);
    if !enc_file.exists() {
        return Err(VaultError::Invalid(
            "identity is not passcode-protected".into(),
        ));
    }
    let vault = read_vault(&enc_file)?;
    let key = decrypt_key(passcode, &vault)?;
    let mnemonic = decrypt_mnemonic(passcode, &vault)?;
    let id = Identity::from_private_key(key.as_ref())
        .map_err(|e| VaultError::Invalid(format!("rebuild identity: {e}")))?;
    id.to_file(&id_file)
        .map_err(|e| VaultError::Io(format!("write identity: {e}")))?;
    // Confirm the plaintext loads before removing the vault.
    Identity::from_file(&id_file)
        .map_err(|e| VaultError::Invalid(format!("verify identity: {e}")))?;
    // Restore the plaintext phrase sidecar so re-display keeps working unprotected.
    if let Some(m) = &mnemonic {
        write_seed_file(&seed_file, m)?;
    }
    std::fs::remove_file(&enc_file).map_err(|e| VaultError::Io(e.to_string()))?;
    Ok(())
}

/// Write the plaintext recovery-phrase sidecar (atomic; 0600 on unix).
fn write_seed_file(path: &Path, phrase: &str) -> Result<(), VaultError> {
    let tmp = path.with_extension("seed.tmp");
    atomic_secret_write(path, &tmp, phrase.as_bytes())
}

fn atomic_secret_write(path: &Path, tmp: &Path, bytes: &[u8]) -> Result<(), VaultError> {
    let _ = std::fs::remove_file(tmp);

    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(tmp)
            .map_err(|e| VaultError::Io(e.to_string()))?
    };

    #[cfg(not(unix))]
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| VaultError::Io(e.to_string()))?;

    file.write_all(bytes)
        .map_err(|e| VaultError::Io(e.to_string()))?;
    file.sync_all().map_err(|e| VaultError::Io(e.to_string()))?;
    drop(file);

    std::fs::rename(tmp, path).map_err(|e| VaultError::Io(e.to_string()))?;
    sync_parent_dir(path);
    Ok(())
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
}

/// Store a freshly-created/imported software identity's recovery phrase so it can
/// be re-displayed later. Plaintext sidecar when unprotected (crypto-equivalent to
/// the plaintext key file); refuses hardware identities. If the identity is already
/// passcode-protected, the phrase belongs in the vault — call at create/import time
/// (before any passcode) and `protect_identity` folds it in afterward.
pub fn store_plaintext_seed(id_dir: &Path, mnemonic: &str) -> Result<(), VaultError> {
    if id_dir.join("identity.hwid").exists() {
        return Err(VaultError::Invalid(
            "hardware identity has no stored phrase".into(),
        ));
    }
    if id_dir.join("identity.enc").exists() {
        // Already protected: don't drop a plaintext copy beside the sealed one.
        return Ok(());
    }
    write_seed_file(&id_dir.join(SEED_FILE), mnemonic.trim())
}

/// Whether a software identity has a recovery phrase available to re-display
/// (plaintext sidecar, or a phrase sealed in its vault). No passcode needed — only
/// presence is checked, not the phrase itself.
pub fn has_stored_mnemonic(id_dir: &Path) -> bool {
    if id_dir.join(SEED_FILE).exists() {
        return true;
    }
    let enc = id_dir.join("identity.enc");
    enc.exists()
        && read_vault(&enc)
            .map(|v| v.mnemonic_token.is_some())
            .unwrap_or(false)
}

/// Reveal a software identity's stored recovery phrase. `passcode` is required only
/// when the phrase lives in the vault (passcode-protected identity).
pub fn reveal_mnemonic(
    id_dir: &Path,
    passcode: Option<&str>,
) -> Result<Zeroizing<String>, VaultError> {
    if id_dir.join("identity.hwid").exists() {
        return Err(VaultError::Invalid(
            "hardware identity has no stored recovery phrase".into(),
        ));
    }
    // The vault is authoritative when present: if the user passcode-protected the
    // identity, reveal must honor the passcode even if a stale plaintext `.seed`
    // survives the verify-before-delete window in `protect_identity`.
    let enc = id_dir.join("identity.enc");
    if enc.exists() {
        let v = read_vault(&enc)?;
        if v.mnemonic_token.is_none() {
            return Err(VaultError::Invalid(
                "no recovery phrase stored for this identity".into(),
            ));
        }
        let pc = passcode.ok_or_else(|| VaultError::Invalid("passcode required".into()))?;
        return decrypt_mnemonic(pc, &v)?
            .ok_or_else(|| VaultError::Invalid("no recovery phrase stored".into()));
    }
    let seed_file = id_dir.join(SEED_FILE);
    if seed_file.exists() {
        let s = std::fs::read_to_string(&seed_file).map_err(|e| VaultError::Io(e.to_string()))?;
        return Ok(Zeroizing::new(s.trim().to_string()));
    }
    Err(VaultError::Invalid(
        "no recovery phrase stored for this identity".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cheap params so tests run fast (real defaults are memory-hard).
    fn fast() -> VaultParams {
        VaultParams {
            m_cost: 8 * 1024,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn encrypt_with(passcode: &str, key: &[u8; 64], p: VaultParams) -> EncryptedVault {
        encrypt_with_mnemonic(passcode, key, None, p)
    }

    fn encrypt_with_mnemonic(
        passcode: &str,
        key: &[u8; 64],
        mnemonic: Option<&str>,
        p: VaultParams,
    ) -> EncryptedVault {
        let salt = random::random_16();
        let kek = derive_kek(passcode, &salt, p).unwrap();
        let blob = token::encrypt(key, kek.as_ref()).unwrap();
        let mnemonic_token =
            mnemonic.map(|m| hex::encode(token::encrypt(m.as_bytes(), kek.as_ref()).unwrap()));
        EncryptedVault {
            version: VERSION,
            kdf: "argon2id".into(),
            m_cost: p.m_cost,
            t_cost: p.t_cost,
            p_cost: p.p_cost,
            salt: hex::encode(salt),
            token: hex::encode(blob),
            mnemonic_token,
        }
    }

    #[test]
    fn roundtrip() {
        let key = [7u8; 64];
        let v = encrypt_with("correct horse battery", &key, fast());
        let out = decrypt_key("correct horse battery", &v).unwrap();
        assert_eq!(out.as_ref(), &key);
    }

    #[test]
    fn wrong_passcode_fails() {
        let v = encrypt_with("right-passcode", &[3u8; 64], fast());
        assert!(matches!(
            decrypt_key("wrong-passcode", &v),
            Err(VaultError::Auth)
        ));
    }

    #[test]
    fn param_tamper_fails() {
        // Downgrading the stored params must not yield a usable KEK, even with the
        // correct passcode (proves params are bound to the KEK).
        let v = encrypt_with("pw", &[9u8; 64], fast());
        let mut tampered = v.clone();
        tampered.t_cost = v.t_cost + 1; // valid but different
        assert!(matches!(
            decrypt_key("pw", &tampered),
            Err(VaultError::Auth)
        ));
        let mut tampered2 = v.clone();
        tampered2.m_cost = v.m_cost * 2;
        assert!(matches!(
            decrypt_key("pw", &tampered2),
            Err(VaultError::Auth)
        ));
    }

    const PHRASE: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon abandon abandon abandon abandon art";

    #[test]
    fn mnemonic_roundtrip() {
        let v = encrypt_with_mnemonic("pw", &[5u8; 64], Some(PHRASE), fast());
        let out = decrypt_mnemonic("pw", &v).unwrap().unwrap();
        assert_eq!(out.as_str(), PHRASE);
        // The key still decrypts independently.
        assert_eq!(decrypt_key("pw", &v).unwrap().as_ref(), &[5u8; 64]);
    }

    #[test]
    fn mnemonic_absent_is_none() {
        let v = encrypt_with("pw", &[5u8; 64], fast());
        assert!(decrypt_mnemonic("pw", &v).unwrap().is_none());
    }

    #[test]
    fn mnemonic_wrong_passcode_fails() {
        let v = encrypt_with_mnemonic("right", &[5u8; 64], Some(PHRASE), fast());
        assert!(matches!(
            decrypt_mnemonic("wrong", &v),
            Err(VaultError::Auth)
        ));
    }

    #[test]
    fn corrupt_blob_fails() {
        let v = encrypt_with("pw", &[1u8; 64], fast());
        let mut t = v.clone();
        // flip a byte in the ciphertext
        let mut blob = hex::decode(&t.token).unwrap();
        let n = blob.len();
        blob[n / 2] ^= 0xFF;
        t.token = hex::encode(blob);
        assert!(matches!(decrypt_key("pw", &t), Err(VaultError::Auth)));
    }

    use std::sync::atomic::{AtomicU64, Ordering};
    static REVEAL_TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_id_dir(tag: &str) -> std::path::PathBuf {
        let n = REVEAL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("ratspeak-vault-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A passcode-protected identity must reveal from the vault even if a stale
    // plaintext `.seed` survives the verify-before-delete window: the vault is
    // authoritative, so reveal honors the passcode and never leaks the decoy seed.
    #[test]
    fn reveal_prefers_vault_over_stale_seed() {
        let dir = temp_id_dir("reveal");
        let v = encrypt_with_mnemonic("pw", &[1u8; 64], Some(PHRASE), fast());
        write_vault(&dir.join("identity.enc"), &v).unwrap();
        std::fs::write(dir.join(SEED_FILE), b"decoy decoy decoy").unwrap();

        assert!(reveal_mnemonic(&dir, None).is_err()); // .enc wins → passcode required
        assert_eq!(reveal_mnemonic(&dir, Some("pw")).unwrap().as_str(), PHRASE);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reveal_reads_plaintext_seed_when_unprotected() {
        let dir = temp_id_dir("seed");
        std::fs::write(dir.join(SEED_FILE), PHRASE.as_bytes()).unwrap();
        assert!(has_stored_mnemonic(&dir));
        assert_eq!(reveal_mnemonic(&dir, None).unwrap().as_str(), PHRASE);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn secret_files_are_created_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_id_dir("mode");
        let v = encrypt_with_mnemonic("pw", &[7u8; 64], Some(PHRASE), fast());
        let enc_path = dir.join("identity.enc");
        write_vault(&enc_path, &v).unwrap();
        write_seed_file(&dir.join(SEED_FILE), PHRASE).unwrap();

        let enc_mode = std::fs::metadata(&enc_path).unwrap().permissions().mode() & 0o777;
        let seed_mode = std::fs::metadata(dir.join(SEED_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(enc_mode, 0o600);
        assert_eq!(seed_mode, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
