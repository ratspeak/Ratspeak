//! Daemon lifecycle: the profile lock must exclude a second daemon, survive a
//! crash (SIGKILL) so a restart can reclaim it, and release cleanly on SIGTERM.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn ratspeakd_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ratspeakd")
}

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!(
        "ratspeak-lifecycle-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn lock_file(data_dir: &Path) -> PathBuf {
    data_dir.join(".ratspeak").join("profile.lock")
}

fn spawn_daemon(data_dir: &Path) -> Child {
    Command::new(ratspeakd_bin())
        .args([
            "--data-dir",
            data_dir.to_str().unwrap(),
            "run",
            "--quiet",
            "--no-share-instance",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ratspeakd")
}

fn wait_until<F: Fn() -> bool>(cond: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn second_daemon_refuses_a_locked_profile() {
    let dir = temp_dir("locked");
    let mut first = spawn_daemon(&dir);
    assert!(
        wait_until(|| lock_file(&dir).exists(), Duration::from_secs(15)),
        "first daemon never acquired the profile lock"
    );

    // A second daemon on the same profile must fail fast with a lock error.
    let mut second = spawn_daemon(&dir);
    assert!(
        wait_for_exit(&mut second, Duration::from_secs(10)),
        "second daemon did not exit; it wrongly co-owned the profile"
    );
    assert!(
        !second.wait().unwrap().success(),
        "second daemon should exit non-zero while the profile is locked"
    );

    kill_and_reap(first);
    let _ = std::fs::remove_dir_all(dir);
    let _ = &mut second;
}

#[test]
fn restart_after_sigkill_reclaims_the_lock() {
    let dir = temp_dir("crash");
    let first = spawn_daemon(&dir);
    assert!(
        wait_until(|| lock_file(&dir).exists(), Duration::from_secs(15)),
        "first daemon never acquired the profile lock"
    );
    // SIGKILL: no chance to clean up. The kernel must release the advisory lock.
    kill_and_reap(first);

    let mut restart = spawn_daemon(&dir);
    // If the stale lock wedged startup, the daemon exits early with a lock error.
    std::thread::sleep(Duration::from_millis(750));
    assert!(
        restart.try_wait().unwrap().is_none(),
        "restart after SIGKILL failed to reclaim the lock"
    );
    kill_and_reap(restart);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sigterm_shuts_down_and_allows_restart() {
    let dir = temp_dir("term");
    let mut child = spawn_daemon(&dir);
    assert!(
        wait_until(|| lock_file(&dir).exists(), Duration::from_secs(15)),
        "daemon never acquired the profile lock"
    );

    // SIGTERM should trigger graceful shutdown, not be ignored.
    Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(
        wait_for_exit(&mut child, Duration::from_secs(20)),
        "daemon did not exit on SIGTERM"
    );

    // A clean stop must let a fresh daemon acquire the lock immediately.
    let mut restart = spawn_daemon(&dir);
    std::thread::sleep(Duration::from_millis(750));
    assert!(
        restart.try_wait().unwrap().is_none(),
        "restart after clean SIGTERM stop failed to acquire the lock"
    );
    kill_and_reap(restart);
    let _ = std::fs::remove_dir_all(dir);
}
