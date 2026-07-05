//! Profile ownership lock for headless/runtime entry points.
//!
//! On Unix the lock is an advisory `flock` held on `profile.lock` for the life
//! of the owning process. The kernel releases it automatically when the process
//! exits — including SIGKILL/OOM — so a crashed daemon never wedges a restart,
//! and two daemons can never both believe they own one profile. The file's JSON
//! body is purely informational. On non-Unix platforms the lock falls back to
//! exclusive file creation; a crashed process leaves a stale file that
//! `ratspeakctl profile unlock --force` clears.

use std::fs::{self, File, OpenOptions};
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
    // Holds the advisory flock for the process lifetime on Unix; dropping it
    // releases the lock. Unused on non-Unix, where exclusivity is the file's
    // existence.
    #[cfg(unix)]
    _file: File,
}

impl ProfileLockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ProfileLockGuard {
    fn drop(&mut self) {
        // Unix: dropping `_file` releases the advisory flock; the informational
        // lock file is intentionally left in place (unlinking would race the
        // inode against a concurrent acquirer).
        #[cfg(not(unix))]
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn try_acquire_profile_lock(
    ratspeak_data_dir: &Path,
    owner: &str,
) -> Result<ProfileLockGuard, ProfileLockError> {
    fs::create_dir_all(ratspeak_data_dir).map_err(ProfileLockError::Io)?;
    let path = lock_path(ratspeak_data_dir);

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Open (or create) without truncating — we must not clobber a live
        // holder's body before we own the lock.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(ProfileLockError::Io)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => {
                    Err(ProfileLockError::Busy {
                        owner: read_profile_lock(ratspeak_data_dir),
                        path,
                    })
                }
                _ => Err(ProfileLockError::Io(err)),
            };
        }
        write_lock_info(&file, owner)?;
        Ok(ProfileLockGuard { path, _file: file })
    }

    #[cfg(not(unix))]
    {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                write_lock_info(&file, owner)?;
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
}

fn write_lock_info(mut file: &File, owner: &str) -> Result<(), ProfileLockError> {
    use std::io::{Seek, SeekFrom};
    let info = ProfileLockInfo {
        owner: owner.to_string(),
        pid: std::process::id(),
        created_at_unix: unix_now_secs(),
    };
    let encoded = serde_json::to_vec_pretty(&info).map_err(ProfileLockError::Encode)?;
    file.set_len(0).map_err(ProfileLockError::Io)?;
    file.seek(SeekFrom::Start(0)).map_err(ProfileLockError::Io)?;
    file.write_all(&encoded).map_err(ProfileLockError::Io)?;
    file.write_all(b"\n").map_err(ProfileLockError::Io)?;
    file.flush().map_err(ProfileLockError::Io)?;
    Ok(())
}

pub fn read_profile_lock(ratspeak_data_dir: &Path) -> Option<ProfileLockInfo> {
    let bytes = fs::read(lock_path(ratspeak_data_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Best-effort check of whether a live process currently holds the profile lock.
/// On Unix this probes the advisory lock (so a crashed holder reads as
/// unlocked); on other platforms it reports the lock file's existence.
pub fn is_profile_locked(ratspeak_data_dir: &Path) -> bool {
    let path = lock_path(ratspeak_data_dir);
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
            return false;
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            // We acquired it, so nobody held it — release and report unlocked.
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
            false
        } else {
            true
        }
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

/// Remove the profile lock file. Returns whether a file was present. On Unix a
/// live holder keeps its advisory lock on the now-unlinked inode, so callers
/// should refuse this while [`is_profile_locked`] is true; it is meaningful for
/// clearing a stale file left by a crash on non-advisory-lock platforms.
pub fn remove_lock_file(ratspeak_data_dir: &Path) -> std::io::Result<bool> {
    match fs::remove_file(lock_path(ratspeak_data_dir)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
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
    fn profile_lock_is_exclusive_and_released_on_drop() {
        let root = std::env::temp_dir().join(format!(
            "ratspeak-profile-lock-test-{}-{}",
            std::process::id(),
            unix_now_secs()
        ));
        let guard = try_acquire_profile_lock(&root, "test-owner").unwrap();
        assert!(is_profile_locked(&root));
        assert!(matches!(
            try_acquire_profile_lock(&root, "second-owner"),
            Err(ProfileLockError::Busy { .. })
        ));
        drop(guard);
        // Lock released; a crashed/exited holder must not wedge re-acquisition.
        assert!(!is_profile_locked(&root));
        let reacquired = try_acquire_profile_lock(&root, "third-owner").unwrap();
        drop(reacquired);
        let _ = fs::remove_dir_all(root);
    }
}
