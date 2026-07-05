use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub const RATSPEAK_RNS_CONFIG_DIR_ENV: &str = "RATSPEAK_RNS_CONFIG_DIR";
pub const RATSPEAK_RNS_SHARED_INSTANCE_PORT: u16 = 37_430;
pub const RATSPEAK_RNS_INSTANCE_CONTROL_PORT: u16 = 37_431;
pub const LEGACY_RNS_SHARED_INSTANCE_PORT: u16 = 37_428;
pub const LEGACY_RNS_INSTANCE_CONTROL_PORT: u16 = 37_429;

/// Reticulum shared-instance identity derived for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RnsInstanceIdentity {
    /// Whether this profile joins a machine-local shared instance. Headless CLI
    /// profiles default to `false` (Standalone) so a bot never becomes a Client
    /// of the desktop app or of Python rnsd.
    pub share_instance: bool,
    /// Rendezvous name. Derived `rsk-<hex>` names can never equal Python's
    /// `default`, so a bot cannot adopt rnsd's abstract socket on Linux.
    pub instance_name: String,
    pub shared_instance_port: u16,
    pub instance_control_port: u16,
}

/// Derive a per-profile Reticulum instance identity from the profile data root.
///
/// - `share` selects Standalone (`false`) vs a shared instance.
/// - `name` is an operator-provided literal instance name (used verbatim); when
///   `None`, a stable `rsk-<hex>` name is derived from the canonical data root.
/// - `derive_ports` selects per-profile ports (CLI) vs the fixed app constants
///   (desktop app, so a second app window still finds the first).
///
/// The derivation is deterministic: the same data root always yields the same
/// identity across restarts and upgrades.
pub fn derive_rns_instance_identity(
    data_root: &Path,
    share: bool,
    name: Option<&str>,
    derive_ports: bool,
) -> RnsInstanceIdentity {
    let canonical = std::fs::canonicalize(data_root).unwrap_or_else(|_| data_root.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());

    let instance_name = match name {
        Some(literal) => literal.to_string(),
        // Derived CLI identity → stable, collision-resistant, never "default".
        None if derive_ports => format!("rsk-{}", hex_lower(&digest[0..6])),
        // App-legacy identity keeps the historical shared-instance name.
        None => "default".to_string(),
    };

    let (shared_instance_port, instance_control_port) = if derive_ports {
        // 40000-59999 avoids the legacy 3742x band and typical ephemeral ranges.
        let base = 40_000u16 + (u16::from_be_bytes([digest[6], digest[7]]) % 20_000);
        (base, base.saturating_add(1))
    } else {
        (
            RATSPEAK_RNS_SHARED_INSTANCE_PORT,
            RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
        )
    };

    RnsInstanceIdentity {
        share_instance: share,
        instance_name,
        shared_instance_port,
        instance_control_port,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[derive(Debug, Clone)]
pub struct DashboardConfig {
    pub data_root: PathBuf,
    pub data_dir: PathBuf,
    pub rns_config_dir: PathBuf,
    pub rns_config_dir_overridden: bool,
    pub max_log_entries: usize,
    /// Reticulum instance policy. Defaults preserve the desktop app's shared
    /// instance on the fixed constants; the headless CLI overrides these via
    /// [`DashboardConfig::with_headless_rns_policy`].
    pub rns_share_instance: bool,
    pub rns_instance_name: Option<String>,
    pub rns_derive_ports: bool,
    /// Seed a default LAN AutoInterface when creating a fresh headless config so
    /// a Standalone bot can still reach the mesh. Off for the desktop app.
    pub rns_seed_default_interface: bool,
}

impl DashboardConfig {
    pub fn from_env_and_defaults(data_root: PathBuf) -> Self {
        let data_dir = data_root.join(".ratspeak");
        std::fs::create_dir_all(&data_dir).ok();

        let rns_config_dir_override = std::env::var_os(RATSPEAK_RNS_CONFIG_DIR_ENV);
        let rns_config_dir_overridden = rns_config_dir_override.is_some();
        let rns_config_dir = rns_config_dir_override
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("reticulum"));

        Self {
            data_root,
            data_dir,
            rns_config_dir,
            rns_config_dir_overridden,
            max_log_entries: 200,
            // Defaults match the desktop app: shared instance on fixed ports.
            rns_share_instance: true,
            rns_instance_name: None,
            rns_derive_ports: false,
            rns_seed_default_interface: false,
        }
    }

    /// Configure the headless CLI/daemon Reticulum instance policy: derive
    /// per-profile ports/name and default to Standalone unless the operator
    /// opts into a shared instance.
    pub fn with_headless_rns_policy(
        mut self,
        share_instance: bool,
        instance_name: Option<String>,
    ) -> Self {
        self.rns_share_instance = share_instance;
        self.rns_instance_name = instance_name;
        self.rns_derive_ports = true;
        self.rns_seed_default_interface = true;
        self
    }

    /// The Reticulum instance identity derived from this profile's data root and
    /// policy fields.
    pub fn rns_instance_identity(&self) -> RnsInstanceIdentity {
        derive_rns_instance_identity(
            &self.data_root,
            self.rns_share_instance,
            self.rns_instance_name.as_deref(),
            self.rns_derive_ports,
        )
    }

    pub fn uses_app_private_rns_config_dir(&self) -> bool {
        !self.rns_config_dir_overridden
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("ratspeak.db")
    }

    pub fn files_dir(&self) -> PathBuf {
        let d = self.data_dir.join("files");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn identities_dir(&self) -> PathBuf {
        let d = self.data_dir.join("identities");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn identity_profile_dir(&self, identity_hash: &str) -> PathBuf {
        let d = self.identities_dir().join(identity_hash);
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn identity_files_dir(&self, identity_hash: &str) -> PathBuf {
        let d = self.identity_profile_dir(identity_hash).join("files");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn identity_rns_config_dir(&self, identity_hash: &str) -> PathBuf {
        let d = self.identity_profile_dir(identity_hash).join("reticulum");
        std::fs::create_dir_all(&d).ok();
        d
    }

    pub fn identity_cache_dir(&self, identity_hash: &str) -> PathBuf {
        let d = self.identity_profile_dir(identity_hash).join("cache");
        std::fs::create_dir_all(&d).ok();
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rns_config_dir_lives_under_ratspeak_data_dir() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let data_root = std::env::temp_dir().join(format!(
            "ratspeak-config-test-{}-{nanos}",
            std::process::id()
        ));
        let config = DashboardConfig::from_env_and_defaults(data_root.clone());

        assert_eq!(config.data_dir, data_root.join(".ratspeak"));
        assert_eq!(config.rns_config_dir, data_root.join(".ratspeak/reticulum"));
        assert!(config.uses_app_private_rns_config_dir());
    }

    #[test]
    fn identity_profile_paths_live_under_identity_dir() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let data_root = std::env::temp_dir().join(format!(
            "ratspeak-config-identity-test-{}-{nanos}",
            std::process::id()
        ));
        let config = DashboardConfig::from_env_and_defaults(data_root.clone());
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        assert_eq!(
            config.identity_profile_dir(hash),
            data_root.join(".ratspeak/identities").join(hash)
        );
        assert_eq!(
            config.identity_files_dir(hash),
            data_root
                .join(".ratspeak/identities")
                .join(hash)
                .join("files")
        );
        assert_eq!(
            config.identity_rns_config_dir(hash),
            data_root
                .join(".ratspeak/identities")
                .join(hash)
                .join("reticulum")
        );
        assert_eq!(
            config.identity_cache_dir(hash),
            data_root
                .join(".ratspeak/identities")
                .join(hash)
                .join("cache")
        );
    }

    #[test]
    fn instance_identity_is_deterministic_per_root() {
        let a = PathBuf::from("/tmp/ratspeak-bot-a");
        let id1 = derive_rns_instance_identity(&a, false, None, true);
        let id2 = derive_rns_instance_identity(&a, false, None, true);
        assert_eq!(id1, id2, "same root must yield the same identity");
    }

    #[test]
    fn instance_identity_differs_across_roots() {
        let a = derive_rns_instance_identity(Path::new("/tmp/ratspeak-bot-a"), true, None, true);
        let b = derive_rns_instance_identity(Path::new("/tmp/ratspeak-bot-b"), true, None, true);
        assert_ne!(a.instance_name, b.instance_name);
        assert_ne!(a.shared_instance_port, b.shared_instance_port);
    }

    #[test]
    fn derived_name_never_equals_python_default() {
        let id = derive_rns_instance_identity(Path::new("/tmp/whatever"), false, None, true);
        assert!(id.instance_name.starts_with("rsk-"));
        assert_ne!(id.instance_name, "default");
        assert_ne!(id.shared_instance_port, id.instance_control_port);
    }

    #[test]
    fn explicit_name_passes_through_and_ports_can_be_fixed() {
        let id = derive_rns_instance_identity(
            Path::new("/tmp/whatever"),
            true,
            Some("my-share"),
            false,
        );
        assert_eq!(id.instance_name, "my-share");
        // derive_ports = false → fixed app constants.
        assert_eq!(id.shared_instance_port, RATSPEAK_RNS_SHARED_INSTANCE_PORT);
        assert_eq!(id.instance_control_port, RATSPEAK_RNS_INSTANCE_CONTROL_PORT);
    }

    #[test]
    fn app_legacy_identity_keeps_default_name_and_constants() {
        // from_env_and_defaults yields (share=true, name=None, derive_ports=false).
        let id = derive_rns_instance_identity(Path::new("/tmp/whatever"), true, None, false);
        assert!(id.share_instance);
        assert_eq!(id.instance_name, "default");
        assert_eq!(id.shared_instance_port, RATSPEAK_RNS_SHARED_INSTANCE_PORT);
        assert_eq!(id.instance_control_port, RATSPEAK_RNS_INSTANCE_CONTROL_PORT);
    }
}
