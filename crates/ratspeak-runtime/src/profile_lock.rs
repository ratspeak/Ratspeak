//! Cooperative profile ownership lock for headless/runtime entry points.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileLockInfo {
    pub owner: String,
    pub pid: u32,
    pub created_at_unix: f64,
}

#[derive(Debug)]
pub enum ProfileLockError {
    Busy {
        path: PathBuf,
        owner: Option<ProfileLockInfo>,
    },
    Io(std::io::Error),
    Encode(serde_json::Error),
}

impl std::fmt::Display for ProfileLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy { path, owner } => {
                write!(f, "profile is already locked at {}", path.display())?;
                if let Some(owner) = owner {
                    write!(
                        f,
                        " by {} (pid {}, created_at_unix {})",
                        owner.owner, owner.pid, owner.created_at_unix
                    )?;
                }
                Ok(())
            }
            Self::Io(error) => write!(f, "{error}"),
            Self::Encode(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ProfileLockError {}

pub struct ProfileLockGuard {
    path: PathBuf,
}

impl ProfileLockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProfileLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn try_acquire_profile_lock(
    ratspeak_data_dir: &Path,
    owner: &str,
) -> Result<ProfileLockGuard, ProfileLockError> {
    fs::create_dir_all(ratspeak_data_dir).map_err(ProfileLockError::Io)?;
    let path = lock_path(ratspeak_data_dir);
    let info = ProfileLockInfo {
        owner: owner.to_string(),
        pid: std::process::id(),
        created_at_unix: unix_now_secs(),
    };
    let encoded = serde_json::to_vec_pretty(&info).map_err(ProfileLockError::Encode)?;

    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(&encoded).map_err(ProfileLockError::Io)?;
            file.write_all(b"\n").map_err(ProfileLockError::Io)?;
            Ok(ProfileLockGuard { path })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(ProfileLockError::Busy {
                owner: read_profile_lock(ratspeak_data_dir),
                path,
            })
        }
        Err(error) => Err(ProfileLockError::Io(error)),
    }
}

pub fn read_profile_lock(ratspeak_data_dir: &Path) -> Option<ProfileLockInfo> {
    let bytes = fs::read(lock_path(ratspeak_data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn lock_path(ratspeak_data_dir: &Path) -> PathBuf {
    ratspeak_data_dir.join("profile.lock")
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lock_is_exclusive_and_removed_on_drop() {
        let root = std::env::temp_dir().join(format!(
            "ratspeak-profile-lock-test-{}-{}",
            std::process::id(),
            unix_now_secs()
        ));
        let guard = try_acquire_profile_lock(&root, "test-owner").unwrap();
        assert!(lock_path(&root).exists());
        assert!(matches!(
            try_acquire_profile_lock(&root, "second-owner"),
            Err(ProfileLockError::Busy { .. })
        ));
        drop(guard);
        assert!(!lock_path(&root).exists());
        let _ = fs::remove_dir_all(root);
    }
}
