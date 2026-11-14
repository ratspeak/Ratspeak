//! The one-time network data move of 1.1.0, as the migrating screen sees it.
//!
//! rsReticulum imports the earlier releases' files into its SQLite stores while
//! Reticulum starts (`initialize_storage_as`), and reports nothing while it
//! works. The import leaves its own traces, though: the ratchet files go once
//! the peer keys are in, and the announce files go chunk by chunk, each after
//! its commit. This module counts those traces and turns them into the
//! `network-migration` snapshot. A start with nothing to import has no move.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::state::AppState;

pub const EVENT: &str = "network-migration";

const POLL: Duration = Duration::from_millis(250);
const HEARTBEAT: Duration = Duration::from_secs(2);

/// What the storage folder still holds of the earlier releases' files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Pending {
    /// `known_destinations` (Python) or `announce_cache.msgpack` (Rust).
    destination_files: bool,
    ratchets: usize,
    announces: usize,
}

impl Pending {
    fn read(storage: &Path) -> Self {
        Self {
            destination_files: storage.join("known_destinations").is_file()
                || storage.join("announce_cache.msgpack").is_file(),
            ratchets: count_files(&storage.join("ratchets")),
            announces: count_files(&storage.join("cache").join("announces")),
        }
    }

    fn any(&self) -> bool {
        self.destination_files || self.ratchets > 0 || self.announces > 0
    }

    fn keys_pending(&self) -> bool {
        self.destination_files || self.ratchets > 0
    }
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .count()
        })
        .unwrap_or(0)
}

/// The move's progress: the peer keys (ratchet files), then the known peers
/// (announce files). Totals are the counts at the start; the peers total also
/// takes the announce files the keys step writes out of a version 5 Rust cache.
#[derive(Clone, Debug, PartialEq)]
struct Progress {
    keys_total: usize,
    peers_total: usize,
    keys_cost: usize,
    fraction: f64,
}

impl Progress {
    fn new(first: Pending) -> Self {
        Self {
            keys_total: first.ratchets,
            peers_total: first.announces,
            // A destination file is imported in one commit; it still weighs one row.
            keys_cost: first.ratchets + usize::from(first.destination_files),
            fraction: 0.0,
        }
    }

    /// The running snapshot for `now`. The fraction never moves back.
    fn running(&mut self, now: Pending, elapsed: Duration) -> Value {
        let keys_phase = now.keys_pending();
        if keys_phase {
            self.peers_total = self.peers_total.max(now.announces);
        }
        let keys_done = if keys_phase {
            self.keys_total.saturating_sub(now.ratchets)
        } else {
            self.keys_total
        };
        let peers_done = if keys_phase {
            0
        } else {
            self.peers_total.saturating_sub(now.announces)
        };
        let weight = self.keys_cost as f64 / (self.keys_cost + self.peers_total).max(1) as f64;
        let keys_part = if keys_phase {
            keys_done as f64 / self.keys_total.max(1) as f64
        } else {
            1.0
        };
        let peers_part = peers_done as f64 / self.peers_total.max(1) as f64;
        let next = (weight * keys_part + (1.0 - weight) * peers_part).clamp(0.0, 0.99);
        self.fraction = self.fraction.max(next);
        let (phase, done, total) = if keys_phase {
            ("keys", keys_done, self.keys_total)
        } else {
            ("peers", peers_done, self.peers_total)
        };
        snapshot("running", phase, done, total, self.fraction, elapsed, None)
    }
}

fn snapshot(
    state: &str,
    phase: &str,
    done: usize,
    total: usize,
    fraction: f64,
    elapsed: Duration,
    error: Option<Value>,
) -> Value {
    let mut value = json!({
        "state": state,
        "phase": phase,
        "done": done,
        "total": total,
        "fraction": fraction,
        "elapsed_ms": elapsed.as_millis() as u64,
    });
    if let Some(error) = error {
        value["error"] = error;
    }
    value
}

/// The reason code the screen words: no space, no permission, unreadable, or other.
fn error_code(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("no space") || lower.contains("os error 28") || lower.contains("disk is full")
    {
        "no_space"
    } else if lower.contains("permission denied")
        || lower.contains("os error 13")
        || lower.contains("read-only")
    {
        "no_permission"
    } else if lower.contains("corrupt")
        || lower.contains("malformed")
        || lower.contains("not a database")
    {
        "unreadable"
    } else {
        "other"
    }
}

fn publish(state: &AppState, value: Value) {
    if let Ok(mut slot) = state.network_migration.lock() {
        *slot = Some(value.clone());
    }
    state.emit_to_all(EVENT, value);
}

/// Watches one start's import. Dropped without `finish`, it stops quietly.
pub struct Watch {
    state: Arc<AppState>,
    storage: PathBuf,
    started: Instant,
    stop: Arc<AtomicBool>,
    progress: Arc<std::sync::Mutex<Progress>>,
    thread: Option<JoinHandle<()>>,
}

/// Starts watching when `config_dir/storage` still holds the earlier files;
/// otherwise this start has no move and the snapshot is cleared.
pub fn begin(state: &Arc<AppState>, config_dir: &Path) -> Option<Watch> {
    let storage = config_dir.join("storage");
    let first = Pending::read(&storage);
    if !first.any() {
        if let Ok(mut slot) = state.network_migration.lock() {
            *slot = None;
        }
        return None;
    }
    if let Ok(mut path) = state.network_migration_storage.lock() {
        *path = Some(storage.clone());
    }
    let started = Instant::now();
    let progress = Arc::new(std::sync::Mutex::new(Progress::new(first)));
    let stop = Arc::new(AtomicBool::new(false));
    if let Ok(mut p) = progress.lock() {
        publish(state, p.running(first, Duration::ZERO));
    }
    let thread = {
        let state = Arc::clone(state);
        let storage = storage.clone();
        let progress = Arc::clone(&progress);
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("network-migration".into())
            .spawn(move || {
                let mut last: Option<Value> = None;
                let mut sent = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL);
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let now = Pending::read(&storage);
                    let Ok(mut p) = progress.lock() else { break };
                    let value = p.running(now, started.elapsed());
                    let moved = last.as_ref().is_none_or(|prev| {
                        prev["done"] != value["done"] || prev["phase"] != value["phase"]
                    });
                    if moved || sent.elapsed() >= HEARTBEAT {
                        publish(&state, value.clone());
                        sent = Instant::now();
                        last = Some(value);
                    }
                }
            })
            .ok()
    };
    Some(Watch {
        state: Arc::clone(state),
        storage,
        started,
        stop,
        progress,
        thread,
    })
}

impl Watch {
    /// Reticulum has started (`Ok`) or could not start (`Err` with its reason).
    /// A start that failed while files remained is the move's failure; one
    /// that failed after the move is not.
    pub fn finish(mut self, outcome: Result<(), String>) {
        self.halt();
        let remaining = Pending::read(&self.storage);
        let elapsed = self.started.elapsed();
        let value = match outcome {
            Err(detail) if remaining.any() => {
                let mut p = self
                    .progress
                    .lock()
                    .map(|p| p.clone())
                    .unwrap_or_else(|e| e.into_inner().clone());
                let mut failed = p.running(remaining, elapsed);
                failed["state"] = json!("failed");
                failed["error"] = json!({
                    "code": error_code(&detail),
                    "detail": detail,
                    "can_skip": true,
                });
                failed
            }
            _ => snapshot("finished", "peers", 0, 0, 1.0, elapsed, None),
        };
        publish(&self.state, value);
    }

    fn halt(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.halt();
    }
}

/// "Continue without old data": removes what the move had left, so the next
/// start begins with an empty peer list that refills as peers announce.
pub fn discard_remaining(state: &AppState) -> std::io::Result<()> {
    let storage = state
        .network_migration_storage
        .lock()
        .ok()
        .and_then(|path| path.clone());
    let Some(storage) = storage else {
        return Ok(());
    };
    discard(&storage)?;
    if let Ok(mut slot) = state.network_migration.lock() {
        *slot = None;
    }
    Ok(())
}

/// Removes the earlier files from `storage`; the new stores stay.
fn discard(storage: &Path) -> std::io::Result<()> {
    for file in ["known_destinations", "announce_cache.msgpack"] {
        let path = storage.join(file);
        if path.is_file() {
            std::fs::remove_file(path)?;
        }
    }
    for dir in [
        storage.join("ratchets"),
        storage.join("cache").join("announces"),
    ] {
        if dir.is_dir() {
            std::fs::remove_dir_all(dir)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(destination_files: bool, ratchets: usize, announces: usize) -> Pending {
        Pending {
            destination_files,
            ratchets,
            announces,
        }
    }

    #[test]
    fn the_keys_step_runs_first_and_the_bar_never_moves_back() {
        let mut p = Progress::new(pending(true, 100, 300));
        let start = p.running(pending(true, 100, 300), Duration::ZERO);
        assert_eq!(start["phase"], "keys");
        assert_eq!(start["done"], 0);
        assert_eq!(start["total"], 100);

        // A version 5 cache writes its announces out during the keys step.
        let more = p.running(pending(true, 100, 340), Duration::from_millis(500));
        assert_eq!(more["phase"], "keys");

        let peers = p.running(pending(false, 0, 340), Duration::from_secs(1));
        assert_eq!(peers["phase"], "peers");
        assert_eq!(peers["done"], 0);
        assert_eq!(peers["total"], 340);
        let keys_share = peers["fraction"].as_f64().unwrap();
        assert!(keys_share > 0.0 && keys_share < 0.5);

        let half = p.running(pending(false, 0, 170), Duration::from_secs(2));
        assert_eq!(half["done"], 170);
        let later = half["fraction"].as_f64().unwrap();
        assert!(later > keys_share);

        // A count that grows again never pulls the bar back, and it never reads 100 %.
        let back = p.running(pending(false, 0, 300), Duration::from_secs(3));
        assert!(back["fraction"].as_f64().unwrap() >= later);
        let end = p.running(pending(false, 0, 0), Duration::from_secs(4));
        assert!(end["fraction"].as_f64().unwrap() <= 0.99);
    }

    #[test]
    fn reasons_name_what_stopped_the_move() {
        assert_eq!(
            error_code("No space left on device (os error 28)"),
            "no_space"
        );
        assert_eq!(error_code("database or disk is full"), "no_space");
        assert_eq!(
            error_code("Permission denied (os error 13)"),
            "no_permission"
        );
        assert_eq!(error_code("file is not a database"), "unreadable");
        assert_eq!(error_code("the shared instance is not ready"), "other");
    }

    #[test]
    fn the_earlier_files_are_read_from_the_storage_folder() {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path();
        assert!(
            !Pending::read(storage).any(),
            "an empty folder has nothing to move"
        );
        std::fs::create_dir_all(storage.join("ratchets")).unwrap();
        std::fs::create_dir_all(storage.join("cache").join("announces")).unwrap();
        std::fs::write(storage.join("ratchets").join("a"), b"x").unwrap();
        std::fs::write(storage.join("cache").join("announces").join("b"), b"x").unwrap();
        std::fs::write(storage.join("cache").join("announces").join("c"), b"x").unwrap();
        assert_eq!(Pending::read(storage), pending(false, 1, 2));
        std::fs::write(storage.join("known_destinations"), b"x").unwrap();
        assert!(Pending::read(storage).keys_pending());
        // known_destinations.db is the new store, not an earlier file.
        std::fs::remove_file(storage.join("known_destinations")).unwrap();
        std::fs::write(storage.join("known_destinations.db"), b"x").unwrap();
        assert_eq!(Pending::read(storage), pending(false, 1, 2));
    }

    #[test]
    fn continuing_without_old_data_removes_only_the_earlier_files() {
        let root = tempfile::tempdir().unwrap();
        let storage = root.path();
        std::fs::create_dir_all(storage.join("ratchets")).unwrap();
        std::fs::create_dir_all(storage.join("cache").join("announces")).unwrap();
        std::fs::write(storage.join("ratchets").join("a"), b"x").unwrap();
        std::fs::write(storage.join("cache").join("announces").join("b"), b"x").unwrap();
        std::fs::write(storage.join("known_destinations"), b"x").unwrap();
        std::fs::write(storage.join("known_destinations.db"), b"x").unwrap();
        std::fs::write(storage.join("cache").join("announces.db"), b"x").unwrap();
        discard(storage).unwrap();
        assert!(!Pending::read(storage).any());
        assert!(
            storage.join("known_destinations.db").is_file(),
            "the new store stays"
        );
        assert!(storage.join("cache").join("announces.db").is_file());
    }
}
