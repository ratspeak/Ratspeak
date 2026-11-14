//! What Pause all paused, so Resume all restarts just that, and which
//! interfaces are switched Off. Names are kept per identity, as each identity
//! has a Reticulum config of its own; Bluetooth Peer is the device's.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::db::{self, DbPool};

/// `{ "names": { identity: [name] }, "ble_peer": bool }`.
pub const PAUSED_ALL_SETTING: &str = "paused_all_interfaces";
/// `{ identity: [name] }`.
pub const INTERFACES_OFF_SETTING: &str = "interfaces_off";

/// The Reticulum config groups an interface can be in.
pub const CONFIG_GROUPS: [&str; 6] = [
    "rnode",
    "auto",
    "tcp_client",
    "tcp_server",
    "backbone_client",
    "backbone_server",
];

type Names = BTreeMap<String, BTreeSet<String>>;

#[derive(Default, Serialize, Deserialize)]
struct PausedAll {
    #[serde(default)]
    names: Names,
    #[serde(default)]
    ble_peer: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InterfaceLifecycle {
    paused: Names,
    ble_peer: bool,
    /// Bluetooth Peer's request expiry while paused, Unix seconds; 0 never.
    ble_peer_expires_at: u64,
    off: Names,
}

/// What Resume all restarts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResumeAll {
    pub names: Vec<String>,
    /// Bluetooth Peer's expiry when it restarts too; 0 never expires.
    pub ble_peer_expires_at: Option<u64>,
}

/// `Some(enabled)` for a configured interface, `None` for one not configured.
pub fn config_enabled(ifaces: &Value, name: &str) -> Option<bool> {
    CONFIG_GROUPS.iter().find_map(|group| {
        ifaces
            .get(group)?
            .as_array()?
            .iter()
            .find(|entry| entry.get("name").and_then(Value::as_str) == Some(name))
            .map(crate::rns_config::interface_entry_enabled)
    })
}

fn unexpired(expires_at: u64, now: u64) -> bool {
    expires_at == 0 || expires_at > now
}

impl InterfaceLifecycle {
    pub fn from_settings(
        paused: Option<&str>,
        off: Option<&str>,
        ble_peer_expires_at: u64,
    ) -> Self {
        let paused: PausedAll = paused
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default();
        Self {
            paused: paused.names,
            ble_peer: paused.ble_peer,
            ble_peer_expires_at,
            off: off
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default(),
        }
    }

    fn settings(&self) -> Vec<(String, String)> {
        let paused = PausedAll {
            names: self.paused.clone(),
            ble_peer: self.ble_peer,
        };
        vec![
            (
                PAUSED_ALL_SETTING.to_string(),
                serde_json::to_string(&paused).unwrap_or_default(),
            ),
            (
                INTERFACES_OFF_SETTING.to_string(),
                serde_json::to_string(&self.off).unwrap_or_default(),
            ),
        ]
    }

    /// Remember what Pause all paused, beside what it paused before and is
    /// still paused.
    pub fn record_pause_all(
        &mut self,
        identity: &str,
        names: impl IntoIterator<Item = String>,
        ble_peer_expires_at: Option<u64>,
    ) {
        let set = self.paused.entry(identity.to_string()).or_default();
        set.extend(names);
        if set.is_empty() {
            self.paused.remove(identity);
        }
        if let Some(expires_at) = ble_peer_expires_at {
            self.ble_peer = true;
            self.ble_peer_expires_at = expires_at;
        }
    }

    fn is_off(&self, identity: &str, name: &str) -> bool {
        self.off.get(identity).is_some_and(|off| off.contains(name))
    }

    /// Remembered interfaces still configured, paused, and not switched Off.
    pub fn paused_names(&self, identity: &str, ifaces: &Value) -> Vec<String> {
        self.paused
            .get(identity)
            .into_iter()
            .flatten()
            .filter(|name| {
                config_enabled(ifaces, name) == Some(false) && !self.is_off(identity, name)
            })
            .cloned()
            .collect()
    }

    /// Interfaces switched Off that are still configured and off.
    pub fn off_names(&self, identity: &str, ifaces: &Value) -> Vec<String> {
        self.off
            .get(identity)
            .into_iter()
            .flatten()
            .filter(|name| config_enabled(ifaces, name) == Some(false))
            .cloned()
            .collect()
    }

    pub fn ble_peer_paused(&self, now: u64) -> bool {
        self.ble_peer && unexpired(self.ble_peer_expires_at, now)
    }

    /// Forget remembered interfaces no longer paused; with none left, Pause
    /// all's state for `identity` clears itself.
    pub fn retain_paused(&mut self, identity: &str, ifaces: &Value) {
        let still: BTreeSet<String> = self.paused_names(identity, ifaces).into_iter().collect();
        if still.is_empty() {
            self.paused.remove(identity);
        } else {
            self.paused.insert(identity.to_string(), still);
        }
    }

    /// A removed interface is neither paused nor Off any more.
    pub fn forget(&mut self, identity: &str, name: &str) {
        for names in [&mut self.paused, &mut self.off] {
            if let Some(set) = names.get_mut(identity) {
                set.remove(name);
                if set.is_empty() {
                    names.remove(identity);
                }
            }
        }
    }

    /// Switched Off, an interface is no longer Pause all's to restart.
    pub fn set_off(&mut self, identity: &str, name: &str, off: bool) {
        if off {
            self.forget(identity, name);
            self.off
                .entry(identity.to_string())
                .or_default()
                .insert(name.to_string());
        } else if let Some(set) = self.off.get_mut(identity) {
            set.remove(name);
            if set.is_empty() {
                self.off.remove(identity);
            }
        }
    }

    /// Whether Bluetooth Peer was remembered.
    pub fn clear_ble_peer(&mut self) -> bool {
        self.ble_peer_expires_at = 0;
        std::mem::take(&mut self.ble_peer)
    }

    /// A new timer for paused Bluetooth Peer.
    pub fn set_ble_peer_expires_at(&mut self, expires_at: u64) {
        if self.ble_peer {
            self.ble_peer_expires_at = expires_at;
        }
    }

    /// What Resume all restarts; the remembered set is cleared.
    pub fn take_resume_all(&mut self, identity: &str, ifaces: &Value, now: u64) -> ResumeAll {
        let resume = ResumeAll {
            names: self.paused_names(identity, ifaces),
            ble_peer_expires_at: self
                .ble_peer_paused(now)
                .then_some(self.ble_peer_expires_at),
        };
        self.paused.remove(identity);
        self.clear_ble_peer();
        resume
    }

    /// The `lifecycle` object of `hub_interfaces_update`.
    pub fn payload(&self, identity: &str, ifaces: &Value, now: u64) -> Value {
        let names = self.paused_names(identity, ifaces);
        let ble_peer = self.ble_peer_paused(now);
        let count = names.len() + usize::from(ble_peer);
        json!({
            "paused_all": count > 0,
            "paused_names": names,
            "ble_peer_paused": ble_peer,
            "paused_count": count,
            "off": self.off_names(identity, ifaces),
        })
    }
}

/// The in-memory mirror and its SQLite settings.
pub struct InterfaceLifecycleStore {
    current: Mutex<InterfaceLifecycle>,
    /// Orders writes, so the settings end as the mirror does.
    write: Mutex<()>,
}

impl InterfaceLifecycleStore {
    pub fn load(pool: &DbPool) -> Self {
        let ble_peer_expires_at = db::get_setting(pool, "ble_peer_expires_at")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Self::new(InterfaceLifecycle::from_settings(
            db::get_setting(pool, PAUSED_ALL_SETTING).as_deref(),
            db::get_setting(pool, INTERFACES_OFF_SETTING).as_deref(),
            ble_peer_expires_at,
        ))
    }

    pub fn new(current: InterfaceLifecycle) -> Self {
        Self {
            current: Mutex::new(current),
            write: Mutex::new(()),
        }
    }

    pub fn read<R>(&self, f: impl FnOnce(&InterfaceLifecycle) -> R) -> R {
        f(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Apply `change`, and store the result when it changed anything.
    pub fn update<R>(&self, pool: &DbPool, change: impl FnOnce(&mut InterfaceLifecycle) -> R) -> R {
        let _write = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let (result, settings) = {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            let before = current.clone();
            let result = change(&mut current);
            (result, (*current != before).then(|| current.settings()))
        };
        if let Some(settings) = settings
            && let Err(error) = db::try_set_settings(pool, &settings)
        {
            tracing::warn!(%error, "interface pause state not saved");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn config(entries: &[(&str, &str, bool)]) -> Value {
        let mut ifaces = json!({ "rnode": [], "auto": [], "tcp_client": [], "tcp_server": [],
            "backbone_client": [], "backbone_server": [] });
        for (group, name, enabled) in entries {
            ifaces[*group]
                .as_array_mut()
                .unwrap()
                .push(json!({ "name": name, "enabled": enabled }));
        }
        ifaces
    }

    fn names(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn paused_all(ifaces: &Value) -> InterfaceLifecycle {
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home", "Radio", "Local Network"]), None);
        assert_eq!(
            lifecycle.paused_names(ME, ifaces),
            names(&["Home", "Local Network", "Radio"])
        );
        lifecycle
    }

    #[test]
    fn resume_all_restarts_what_is_still_paused_then_forgets_it() {
        let ifaces = config(&[
            ("backbone_client", "Home", false),
            ("rnode", "Radio", false),
            ("auto", "Local Network", false),
            ("tcp_server", "Host", false),
        ]);
        let mut lifecycle = paused_all(&ifaces);
        // Switched off before Pause all: never in the set, never restarted.
        assert!(!lifecycle.paused_names(ME, &ifaces).contains(&"Host".into()));
        let resume = lifecycle.take_resume_all(ME, &ifaces, 100);
        assert_eq!(resume.names, names(&["Home", "Local Network", "Radio"]));
        assert_eq!(resume.ble_peer_expires_at, None);
        assert_eq!(lifecycle, InterfaceLifecycle::default());
        assert_eq!(
            lifecycle.take_resume_all(ME, &ifaces, 100),
            ResumeAll::default()
        );
    }

    #[test]
    fn one_started_alone_no_longer_counts_and_the_state_clears_with_the_last() {
        let mut ifaces = config(&[
            ("backbone_client", "Home", false),
            ("rnode", "Radio", false),
        ]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home", "Radio"]), None);
        ifaces["rnode"][0]["enabled"] = json!(true);
        let payload = lifecycle.payload(ME, &ifaces, 100);
        assert_eq!(payload["paused_all"], true);
        assert_eq!(payload["paused_names"], json!(["Home"]));
        assert_eq!(payload["paused_count"], 1);
        lifecycle.retain_paused(ME, &ifaces);
        // Paused again by hand, it is not Pause all's.
        ifaces["rnode"][0]["enabled"] = json!(false);
        assert_eq!(lifecycle.paused_names(ME, &ifaces), names(&["Home"]));

        ifaces["backbone_client"][0]["enabled"] = json!(true);
        lifecycle.retain_paused(ME, &ifaces);
        assert_eq!(lifecycle, InterfaceLifecycle::default());
        assert_eq!(lifecycle.payload(ME, &ifaces, 100)["paused_all"], false);
    }

    #[test]
    fn a_second_pause_all_keeps_what_the_first_still_holds() {
        let mut ifaces = config(&[("backbone_client", "Home", false), ("rnode", "Radio", true)]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home"]), None);
        lifecycle.record_pause_all(ME, names(&["Radio"]), None);
        ifaces["rnode"][0]["enabled"] = json!(false);
        assert_eq!(
            lifecycle.take_resume_all(ME, &ifaces, 0).names,
            names(&["Home", "Radio"])
        );
    }

    #[test]
    fn a_removed_interface_leaves_the_set() {
        let ifaces = config(&[
            ("backbone_client", "Home", false),
            ("rnode", "Radio", false),
        ]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home", "Radio", "Local Network"]), None);
        // Gone from the config: it does not count even before it is forgotten.
        assert_eq!(
            lifecycle.paused_names(ME, &ifaces),
            names(&["Home", "Radio"])
        );
        lifecycle.forget(ME, "Radio");
        lifecycle.forget(ME, "Home");
        lifecycle.forget(ME, "Local Network");
        assert_eq!(lifecycle, InterfaceLifecycle::default());
    }

    #[test]
    fn identities_keep_their_own_sets() {
        let ifaces = config(&[("backbone_client", "Home", false)]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home"]), None);
        assert!(lifecycle.paused_names(OTHER, &ifaces).is_empty());
        assert_eq!(lifecycle.payload(OTHER, &ifaces, 0)["paused_all"], false);
        assert_eq!(
            lifecycle.take_resume_all(OTHER, &ifaces, 0),
            ResumeAll::default()
        );
        assert_eq!(lifecycle.paused_names(ME, &ifaces), names(&["Home"]));
    }

    #[test]
    fn bluetooth_peer_counts_until_its_timer_ends() {
        let ifaces = config(&[]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, Vec::new(), Some(200));
        let payload = lifecycle.payload(ME, &ifaces, 100);
        assert_eq!(payload["paused_all"], true);
        assert_eq!(payload["ble_peer_paused"], true);
        assert_eq!(payload["paused_count"], 1);
        // A new timer while paused moves the end.
        lifecycle.set_ble_peer_expires_at(300);
        assert!(lifecycle.ble_peer_paused(250));
        // Expired while paused: no longer paused, and Resume all leaves it off.
        assert!(!lifecycle.ble_peer_paused(300));
        assert_eq!(lifecycle.payload(ME, &ifaces, 300)["paused_all"], false);
        let mut expired = lifecycle.clone();
        assert_eq!(
            expired.take_resume_all(ME, &ifaces, 300),
            ResumeAll::default()
        );
        assert_eq!(
            lifecycle
                .take_resume_all(ME, &ifaces, 250)
                .ble_peer_expires_at,
            Some(300)
        );

        let mut always = InterfaceLifecycle::default();
        always.record_pause_all(ME, Vec::new(), Some(0));
        assert!(always.ble_peer_paused(u64::MAX));
        assert!(always.clear_ble_peer());
        assert!(!always.clear_ble_peer());
        assert!(!always.ble_peer_paused(0));
        // Not paused, a timer change is not remembered.
        always.set_ble_peer_expires_at(500);
        assert_eq!(always, InterfaceLifecycle::default());
    }

    #[test]
    fn off_is_not_paused_and_resume_all_leaves_it() {
        let mut ifaces = config(&[("rnode", "Radio", false), ("tcp_server", "Host", false)]);
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Radio", "Host"]), None);
        lifecycle.set_off(ME, "Radio", true);
        let payload = lifecycle.payload(ME, &ifaces, 0);
        assert_eq!(payload["off"], json!(["Radio"]));
        assert_eq!(payload["paused_names"], json!(["Host"]));
        assert_eq!(
            lifecycle.clone().take_resume_all(ME, &ifaces, 0).names,
            names(&["Host"])
        );
        // On again: not listed while it runs, and no longer Off.
        ifaces["rnode"][0]["enabled"] = json!(true);
        assert_eq!(lifecycle.off_names(ME, &ifaces), Vec::<String>::new());
        lifecycle.set_off(ME, "Radio", false);
        ifaces["rnode"][0]["enabled"] = json!(false);
        assert!(lifecycle.off_names(ME, &ifaces).is_empty());
        lifecycle.forget(ME, "Host");
        assert_eq!(lifecycle, InterfaceLifecycle::default());
    }

    #[test]
    fn the_settings_round_trip_and_bad_ones_read_as_none() {
        let mut lifecycle = InterfaceLifecycle::default();
        lifecycle.record_pause_all(ME, names(&["Home"]), Some(9));
        lifecycle.set_off(OTHER, "Radio", true);
        let settings = lifecycle.settings();
        assert_eq!(settings[0].0, PAUSED_ALL_SETTING);
        assert_eq!(
            settings[0].1,
            format!(r#"{{"names":{{"{ME}":["Home"]}},"ble_peer":true}}"#)
        );
        assert_eq!(settings[1].1, format!(r#"{{"{OTHER}":["Radio"]}}"#));
        let read = InterfaceLifecycle::from_settings(Some(&settings[0].1), Some(&settings[1].1), 9);
        assert_eq!(read, lifecycle);
        assert_eq!(
            InterfaceLifecycle::from_settings(Some("nope"), Some("[1]"), 0),
            InterfaceLifecycle::default()
        );
    }

    #[test]
    fn the_store_saves_changes_and_loads_them_back() {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        db::init_schema(&pool).unwrap();
        let store = InterfaceLifecycleStore::load(&pool);
        assert_eq!(store.read(Clone::clone), InterfaceLifecycle::default());
        db::set_setting(&pool, "ble_peer_expires_at", "77");
        store.update(&pool, |l| {
            l.record_pause_all(ME, names(&["Home"]), Some(77))
        });
        let loaded = InterfaceLifecycleStore::load(&pool);
        assert_eq!(loaded.read(Clone::clone), store.read(Clone::clone));
        assert!(loaded.read(|l| l.ble_peer_paused(10)));
    }
}
