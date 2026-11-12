//! Interface discovery: its settings, the servers and radios others publish,
//! and connecting to or keeping one.

use std::sync::Arc;

use ratspeak_core::types::{DiscoveredEntry, DiscoveredInterfaces, NetworkSettings};
use ratspeak_runtime::Zeroizing;
use rns_transport::discovery::{DiscoveredInterface, DiscoveryStatus};
use rns_transport::messages::InterfaceStatRpcEntry;
use serde_json::{Value, json};
use tauri::State;

use crate::commands::interfaces::{
    BackboneConnectionArgs, InterfaceIfacCommandFields, TcpConnectionArgs,
};
use crate::commands::shared::{active_rns_config_dir, with_rns_config_lock};
use crate::error::{AppError, AppResult};
use crate::helpers::validate_hex;
use crate::state::AppState;

/// The Discovered list holds this many entries, the most recently heard first.
pub const DISCOVERED_LIST_LIMIT: usize = 100;

/// Join automatically keeps at most this many servers.
const AUTOJOIN_MAX: u8 = 16;

fn rns_handle(state: &AppState) -> Option<rns_runtime::reticulum::ReticulumHandle> {
    state
        .rns
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|mgr| mgr.handle.clone()))
}

fn hashes(values: &[String], what: &str) -> AppResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        let v = value.trim().to_ascii_lowercase();
        if !validate_hex(&v, 32, 32) {
            return Err(AppError::bad_request(format!(
                "{what} must be 32 hex characters"
            )));
        }
        if !out.contains(&v) {
            out.push(v);
        }
    }
    Ok(out)
}

async fn restart(state: &Arc<AppState>) -> AppResult<()> {
    ratspeak_runtime::restart_rns_lxmf(Arc::clone(state))
        .await
        .map_err(|_| AppError::internal("The network could not restart safely"))
}

async fn store(state: &AppState, settings: &NetworkSettings) -> AppResult<()> {
    let json =
        serde_json::to_string(settings).map_err(|_| AppError::internal("settings encode"))?;
    crate::db::spawn_db(state.db.clone(), move |p| {
        crate::db::try_set_setting(&p, ratspeak_runtime::state::NETWORK_SETTINGS_KEY, &json)
    })
    .await
    .map_err(|_| AppError::internal("settings db task panicked"))?
    .map_err(|e| AppError::database_unavailable(format!("Failed to save discovery: {e}")))?;
    if let Ok(mut slot) = state.network_settings.write() {
        *slot = settings.clone();
    }
    Ok(())
}

fn write_config(state: &AppState, settings: &NetworkSettings) -> AppResult<bool> {
    let config_dir = active_rns_config_dir(state);
    with_rns_config_lock(state, || {
        crate::rns_config::apply_network_settings(&config_dir, settings)
    })
    .map_err(|_| AppError::internal("The Reticulum config could not be written"))
}

#[tauri::command]
pub async fn api_network_settings(state: State<'_, Arc<AppState>>) -> AppResult<NetworkSettings> {
    Ok(state.network_settings())
}

#[tauri::command]
pub async fn set_network_settings(
    state: State<'_, Arc<AppState>>,
    args: NetworkSettings,
) -> AppResult<NetworkSettings> {
    ratspeak_runtime::network_ownership::require_local_interfaces(&state)
        .map_err(AppError::bad_request)?;
    if args.autojoin > AUTOJOIN_MAX {
        return Err(AppError::bad_request("Join at most 16 servers"));
    }
    let previous = state.network_settings();
    let next = NetworkSettings {
        trusted_publishers: hashes(&args.trusted_publishers, "A publisher")?,
        other: previous.other.clone(),
        ..args
    };
    if next == previous {
        return Ok(next);
    }
    store(&state, &next).await?;
    let changed = match write_config(&state, &next) {
        Ok(changed) => changed,
        Err(error) => {
            let _ = store(&state, &previous).await;
            return Err(error);
        }
    };
    // Every key here is read at network start.
    if changed
        && rns_handle(&state).is_some()
        && let Err(error) = restart(&state).await
    {
        // The running network still has the old keys; leave nothing half applied.
        let _ = write_config(&state, &previous);
        let _ = store(&state, &previous).await;
        return Err(error);
    }
    Ok(next)
}

/// What a network restart would drop now: a call's peer, and devices on this device's servers.
#[tauri::command]
pub async fn api_restart_impact(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    #[cfg(feature = "lxst-voice")]
    let call =
        crate::voice::voice_status(&state)["snapshot"]["active_call"]["remote_lxmf_destination"]
            .as_str()
            .map(str::to_string);
    #[cfg(not(feature = "lxst-voice"))]
    let call: Option<String> = None;
    let clients = match rns_handle(&state) {
        Some(handle) => handle
            .interface_stats()
            .await
            .map(|s| {
                s.interfaces
                    .iter()
                    .filter(|i| i.parent_id.is_none())
                    .filter_map(|i| i.clients)
                    .sum::<u64>()
            })
            .unwrap_or(0),
        None => 0,
    };
    Ok(json!({ "call": call, "clients": clients.min(u64::from(u32::MAX)) }))
}

fn kind_of(interface_type: &str) -> &'static str {
    match interface_type {
        "BackboneInterface" => "backbone",
        "TCPServerInterface" | "TCPClientInterface" => "tcp",
        "RNodeInterface" | "RNodeMultiInterface" | "KISSInterface" | "WeaveInterface" => "radio",
        "I2PInterface" => "i2p",
        _ => "other",
    }
}

/// Saved connections: every interface name, and each client's `host:port`.
#[derive(Default)]
struct Saved {
    names: Vec<String>,
    targets: Vec<(String, u16)>,
}

impl Saved {
    /// A saved connection already points at `record`'s address.
    fn has(&self, record: &DiscoveredInterface) -> bool {
        matches!(
            (record.info.reachable_on.as_deref(), record.info.port),
            (Some(host), Some(port)) if self.targets.iter().any(|(h, p)| h == host && *p == port)
        )
    }
}

fn saved_connections(config_dir: &std::path::Path) -> Saved {
    let ifaces = crate::rns_config::get_all_interfaces(config_dir);
    let mut saved = Saved::default();
    let Some(groups) = ifaces.as_object() else {
        return saved;
    };
    for (group, entries) in groups {
        for entry in entries.as_array().into_iter().flatten() {
            if let Some(name) = entry.get("name").and_then(Value::as_str) {
                saved.names.push(name.to_string());
            }
            if !matches!(group.as_str(), "tcp_client" | "backbone_client") {
                continue;
            }
            let host = entry.get("target_host").and_then(Value::as_str);
            let port = entry
                .get("target_port")
                .and_then(Value::as_str)
                .and_then(|port| port.parse().ok());
            if let (Some(host), Some(port)) = (host, port) {
                saved.targets.push((host.to_string(), port));
            }
        }
    }
    saved
}

/// The live interface Join automatically made for `record`: a Backbone client
/// at its address that is no saved connection.
fn joined_interface<'a>(
    record: &DiscoveredInterface,
    live: &'a [InterfaceStatRpcEntry],
    saved: &Saved,
) -> Option<&'a InterfaceStatRpcEntry> {
    let (host, port) = (record.info.reachable_on.as_deref()?, record.info.port?);
    live.iter().find(|entry| {
        entry.parent_id.is_none()
            && !saved.names.contains(&entry.name)
            && entry.identity.as_ref().is_some_and(|identity| {
                identity.name
                    == rns_interface::backbone::BackboneClientConfig::new(
                        &identity.short_name,
                        host,
                        port,
                    )
                    .python_name()
            })
    })
}

fn entry(
    record: &DiscoveredInterface,
    live: &[InterfaceStatRpcEntry],
    saved: &Saved,
) -> DiscoveredEntry {
    let info = &record.info;
    let kind = kind_of(&info.interface_type);
    let address = info.reachable_on.clone();
    let added = saved.has(record);
    let reachable = address
        .as_deref()
        .is_some_and(|h| !h.ends_with(".i2p") && !h.ends_with(".onion"));
    let joined_interface = joined_interface(record, live, saved).map(|entry| entry.name.clone());
    DiscoveredEntry {
        id: hex::encode(record.discovery_hash()),
        name: info.name.clone(),
        kind: kind.to_string(),
        status: match record.status {
            Some(DiscoveryStatus::Available) => "available",
            Some(DiscoveryStatus::Unknown) => "unknown",
            _ => "stale",
        }
        .to_string(),
        hops: record.hops,
        last_heard: record.last_heard.max(0.0) as u64,
        address,
        port: info.port,
        latitude: info.latitude,
        longitude: info.longitude,
        publisher: hex::encode(record.network_id),
        frequency: info.frequency,
        bandwidth: info.bandwidth,
        spreading_factor: info.spreading_factor,
        coding_rate: info.coding_rate,
        private_network: info.ifac_netname.is_some() || info.ifac_netkey.is_some(),
        transport: info.transport_enabled,
        joined: joined_interface.is_some(),
        joined_interface,
        added,
        connectable: matches!(kind, "backbone" | "tcp")
            && reachable
            && info.port.is_some()
            && !added,
    }
}

/// The list as the UI shows it: one row per server, most recently heard
/// first, cut to `limit`. A joined or saved entry always stays and counts
/// toward the limit.
fn listed(
    records: &[DiscoveredInterface],
    live: &[InterfaceStatRpcEntry],
    saved: &Saved,
    limit: usize,
) -> Vec<DiscoveredEntry> {
    let mut entries: Vec<DiscoveredEntry> = records.iter().map(|r| entry(r, live, saved)).collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.last_heard));
    // A publisher without transport has a new identity each start, so one server can be
    // stored several times; show it once, as last heard.
    let mut seen = std::collections::HashSet::new();
    entries.retain(|e| seen.insert((e.kind.clone(), e.name.clone(), e.address.clone(), e.port)));
    let held = entries.iter().filter(|e| e.joined || e.added).count();
    let mut room = limit.saturating_sub(held);
    entries.retain(|e| {
        if e.joined || e.added {
            return true;
        }
        let keep = room > 0;
        room = room.saturating_sub(1);
        keep
    });
    entries
}

async fn discovered(
    state: &AppState,
) -> Option<(
    rns_runtime::reticulum::ReticulumHandle,
    Vec<DiscoveredInterface>,
)> {
    let handle = rns_handle(state)?;
    let records = handle.discovered_interfaces().await;
    Some((handle, records))
}

async fn live_interfaces(
    handle: &rns_runtime::reticulum::ReticulumHandle,
) -> Vec<InterfaceStatRpcEntry> {
    handle
        .interface_stats()
        .await
        .map(|s| s.interfaces)
        .unwrap_or_default()
}

#[tauri::command]
pub async fn api_discovered_interfaces(
    state: State<'_, Arc<AppState>>,
) -> AppResult<DiscoveredInterfaces> {
    let Some((handle, records)) = discovered(&state).await else {
        return Ok(DiscoveredInterfaces::default());
    };
    let enabled = handle.discovery_enabled().await && state.network_settings().discover;
    let live = live_interfaces(&handle).await;
    let saved = saved_connections(&active_rns_config_dir(&state));
    Ok(DiscoveredInterfaces {
        enabled,
        entries: listed(&records, &live, &saved, DISCOVERED_LIST_LIMIT),
    })
}

fn ifac_of(record: &DiscoveredInterface) -> InterfaceIfacCommandFields {
    let info = &record.info;
    let private = info.ifac_netname.is_some() || info.ifac_netkey.is_some();
    InterfaceIfacCommandFields {
        ifac_enabled: Some(private),
        ifac_network_name: info.ifac_netname.clone(),
        ifac_passphrase: info.ifac_netkey.clone().map(Zeroizing::new),
        ifac_size: None,
    }
}

/// The published name as a connection name no saved interface has yet.
fn connection_name(name: &str, taken: &[String]) -> String {
    let name: String = name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '=' | '#') && !c.is_control())
        .collect();
    let name = name.trim();
    let base: String = if name.is_empty() {
        "Discovered server".to_string()
    } else {
        name.chars().take(58).collect::<String>().trim().to_string()
    };
    if !taken.contains(&base) {
        return base;
    }
    (2u32..)
        .map(|n| format!("{base} ({n})"))
        .find(|candidate| !taken.contains(candidate))
        .expect("an unused suffix exists")
}

async fn add_client(
    state: &State<'_, Arc<AppState>>,
    record: &DiscoveredInterface,
    name: String,
) -> AppResult<Value> {
    let info = &record.info;
    let (Some(host), Some(port)) = (info.reachable_on.clone(), info.port) else {
        return Err(AppError::bad_request(
            "This connection has no address to join.",
        ));
    };
    match kind_of(&info.interface_type) {
        "backbone" => {
            crate::commands::interfaces::add_backbone_connection(
                state.clone(),
                BackboneConnectionArgs {
                    host,
                    port: i64::from(port),
                    name,
                    prefer_ipv6: false,
                    connect_timeout: None,
                    max_reconnect_tries: None,
                    i2p_tunneled: false,
                    ifac: ifac_of(record),
                },
            )
            .await
        }
        "tcp" => {
            crate::commands::interfaces::add_tcp_connection(
                state.clone(),
                TcpConnectionArgs {
                    host,
                    port: i64::from(port),
                    name,
                    ifac: ifac_of(record),
                },
            )
            .await
        }
        _ => Err(AppError::bad_request(
            "Only servers can be joined over the internet.",
        )),
    }
}

/// Saves `record` as a connection; the saved one replaces a joined one, and
/// Join automatically skips saved addresses.
async fn save_connection(
    state: &State<'_, Arc<AppState>>,
    handle: &rns_runtime::reticulum::ReticulumHandle,
    record: &DiscoveredInterface,
) -> AppResult<Value> {
    let saved = saved_connections(&active_rns_config_dir(state));
    if saved.has(record) {
        return Err(AppError::conflict("That connection is already saved."));
    }
    let live = live_interfaces(handle).await;
    let joined = joined_interface(record, &live, &saved).map(|entry| entry.id);
    let name = connection_name(&record.info.name, &saved.names);
    let queued = add_client(state, record, name).await?;
    if let Some(id) = joined {
        rns_runtime::reticulum::teardown_interface(handle, id).await;
    }
    Ok(queued)
}

#[tauri::command]
pub async fn connect_discovered_interface(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<Value> {
    let Some((handle, records)) = discovered(&state).await else {
        return Err(AppError::bad_request("The network isn't running."));
    };
    let id = id.to_ascii_lowercase();
    let record = records
        .into_iter()
        .find(|r| hex::encode(r.discovery_hash()) == id)
        .ok_or_else(|| AppError::bad_request("That connection is no longer discovered."))?;
    save_connection(&state, &handle, &record).await
}

#[tauri::command]
pub async fn keep_joined_interface(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    let Some((handle, mut records)) = discovered(&state).await else {
        return Err(AppError::bad_request("The network isn't running."));
    };
    let saved = saved_connections(&active_rns_config_dir(&state));
    let live = live_interfaces(&handle).await;
    records.sort_by(|a, b| b.last_heard.total_cmp(&a.last_heard));
    let record = records
        .into_iter()
        .find(|r| joined_interface(r, &live, &saved).is_some_and(|entry| entry.name == name))
        .ok_or_else(|| AppError::bad_request("That connection is no longer discovered."))?;
    save_connection(&state, &handle, &record).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rns_transport::discovery::app_data::DiscoveryInfo;
    use rns_transport::messages::InterfaceIdentity;

    fn server(name: &str, host: &str, last_heard: u64) -> DiscoveredInterface {
        DiscoveredInterface {
            info: DiscoveryInfo {
                name: name.to_string(),
                transport_id: [7; 16],
                interface_type: "BackboneInterface".to_string(),
                transport_enabled: true,
                reachable_on: Some(host.to_string()),
                port: Some(4242),
                ..DiscoveryInfo::default()
            },
            network_id: [9; 16],
            hops: 2,
            stamp_value: 0.0,
            stamp: Vec::new(),
            discovered: last_heard as f64,
            last_heard: last_heard as f64,
            heard_count: 0,
            status: Some(DiscoveryStatus::Available),
        }
    }

    fn many(count: u64) -> Vec<DiscoveredInterface> {
        (0..count)
            .map(|n| {
                server(
                    &format!("Server {n}"),
                    &format!("s{n}.example.org"),
                    1_000 + n,
                )
            })
            .collect()
    }

    fn backbone_client(id: u64, name: &str, host: &str) -> InterfaceStatRpcEntry {
        InterfaceStatRpcEntry {
            id,
            name: name.to_string(),
            online: true,
            identity: Some(InterfaceIdentity::new(
                rns_interface::backbone::BackboneClientConfig::new(name, host, 4242).python_name(),
                name,
                "BackboneClientInterface",
            )),
            ..InterfaceStatRpcEntry::default()
        }
    }

    fn names(entries: &[DiscoveredEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn the_list_holds_the_hundred_most_recently_heard() {
        assert_eq!(DISCOVERED_LIST_LIMIT, 100);
        let records = many(130);
        let entries = listed(&records, &[], &Saved::default(), DISCOVERED_LIST_LIMIT);
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[0].name, "Server 129");
        assert_eq!(entries[99].name, "Server 30");
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].last_heard > pair[1].last_heard)
        );
        // Fewer than the limit are all listed.
        assert_eq!(
            listed(&many(40), &[], &Saved::default(), DISCOVERED_LIST_LIMIT).len(),
            40
        );
    }

    #[test]
    fn a_saved_or_joined_entry_stays_and_counts_toward_the_limit() {
        let records = many(130);
        // The two oldest: one saved, one Join automatically is connected to.
        let saved = Saved {
            names: vec!["Home".to_string()],
            targets: vec![("s0.example.org".to_string(), 4242)],
        };
        let live = [backbone_client(5, "Server 1", "s1.example.org")];
        let entries = listed(&records, &live, &saved, DISCOVERED_LIST_LIMIT);
        assert_eq!(entries.len(), 100);
        let listed_names = names(&entries);
        assert_eq!(listed_names[0], "Server 129");
        assert_eq!(&listed_names[97..], ["Server 32", "Server 1", "Server 0"]);
        let kept = &entries[99];
        assert!(kept.added && !kept.joined && !kept.connectable);
        assert!(saved.has(&records[0]) && !saved.has(&records[1]));
        let joined = &entries[98];
        assert!(joined.joined && !joined.added);
        assert_eq!(joined.joined_interface.as_deref(), Some("Server 1"));
        assert!(entries[..98].iter().all(|e| !e.joined && !e.added));
    }

    #[test]
    fn the_limit_applies_after_one_row_per_server() {
        // 150 records of one server under new identities, then 120 others.
        let mut records: Vec<DiscoveredInterface> = (0..150)
            .map(|n| {
                let mut record = server("Relay", "relay.example.org", 5_000 + n);
                record.network_id = [(n % 250) as u8; 16];
                record.info.transport_id = [(n % 250) as u8; 16];
                record
            })
            .collect();
        records.extend(many(120));
        let entries = listed(&records, &[], &Saved::default(), DISCOVERED_LIST_LIMIT);
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[0].name, "Relay");
        assert_eq!(entries[0].last_heard, 5_149);
        assert_eq!(entries.iter().filter(|e| e.name == "Relay").count(), 1);
        assert_eq!(entries[99].name, "Server 21");
    }

    #[test]
    fn joined_means_an_unsaved_backbone_client_at_the_published_address() {
        let record = server("Maya's backbone", "maya.example.org", 10);
        let none = Saved::default();
        // Join automatically renames on a collision; the address still matches.
        let live = [backbone_client(
            3,
            "Maya's backbone (2)",
            "maya.example.org",
        )];
        assert_eq!(
            joined_interface(&record, &live, &none).map(|e| e.id),
            Some(3)
        );
        // Another server, a saved connection, and a server's accepted client are not it.
        let elsewhere = [backbone_client(3, "Maya's backbone", "other.example.org")];
        assert!(joined_interface(&record, &elsewhere, &none).is_none());
        let saved = Saved {
            names: vec!["Maya's backbone (2)".to_string()],
            targets: vec![("maya.example.org".to_string(), 4242)],
        };
        assert!(joined_interface(&record, &live, &saved).is_none());
        let mut child = backbone_client(4, "Maya's backbone", "maya.example.org");
        child.parent_id = Some(1);
        assert!(joined_interface(&record, &[child], &none).is_none());
        let mut radio = server("Ridge LoRa", "maya.example.org", 10);
        radio.info.reachable_on = None;
        assert!(joined_interface(&radio, &live, &none).is_none());
    }

    #[test]
    fn a_saved_name_never_replaces_another_connection() {
        let taken = vec!["Relay".to_string(), "Relay (2)".to_string()];
        assert_eq!(connection_name("Relay", &[]), "Relay");
        assert_eq!(connection_name("Relay", &taken), "Relay (3)");
        assert_eq!(connection_name(" [Re#lay]=\n", &[]), "Relay");
        assert_eq!(connection_name("\u{7}", &[]), "Discovered server");
        let long = connection_name(&"x".repeat(200), &["x".repeat(58)]);
        assert_eq!(long, format!("{} (2)", "x".repeat(58)));
        assert!(long.chars().count() <= 64);
    }

    #[test]
    fn entries_say_what_can_be_connected() {
        let none = Saved::default();
        let mut onion = server("Hidden", "abcdef.onion", 10);
        assert!(!entry(&onion, &[], &none).connectable);
        onion.info.reachable_on = Some("hidden.example.org".to_string());
        let open = entry(&onion, &[], &none);
        assert!(open.connectable && open.kind == "backbone" && open.status == "available");
        assert_eq!(open.publisher, hex::encode([9u8; 16]));
        let mut radio = server("Ridge LoRa", "x", 10);
        radio.info.interface_type = "RNodeInterface".to_string();
        radio.info.reachable_on = None;
        radio.info.port = None;
        radio.info.frequency = Some(915_000_000);
        radio.status = None;
        let row = entry(&radio, &[], &none);
        assert!(!row.connectable && row.kind == "radio" && row.status == "stale");
        assert_eq!(row.frequency, Some(915_000_000));
    }
}
