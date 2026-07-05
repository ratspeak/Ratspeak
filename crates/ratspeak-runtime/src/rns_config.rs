//! RNS config file (INI-flavoured) read/write.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ratspeak_core::config::{
    LEGACY_RNS_INSTANCE_CONTROL_PORT, LEGACY_RNS_SHARED_INSTANCE_PORT,
    RATSPEAK_RNS_INSTANCE_CONTROL_PORT, RATSPEAK_RNS_SHARED_INSTANCE_PORT, RnsInstanceIdentity,
};
use serde_json::{Value, json};

static CONFIG_WRITE_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub const RNODE_DEFAULT_INTERFACE_MODE: &str = "full";
pub const RNODE_INTERFACE_MODES: &[&str] =
    &["full", "gateway", "access_point", "boundary", "roaming"];

pub fn read_config(config_dir: &Path) -> Option<String> {
    let path = config_dir.join("config");
    std::fs::read_to_string(&path).ok()
}

pub fn write_config_result(config_dir: &Path, content: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config");
    let tmp_path = unique_config_tmp_path(config_dir, "write");

    let write_result = (|| {
        let mut tmp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        tmp.write_all(content.as_bytes())?;
        tmp.sync_all()?;
        drop(tmp);

        if path.exists() {
            std::fs::copy(&path, config_dir.join("config.backup"))?;
        }

        match std::fs::rename(&tmp_path, &path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::copy(&tmp_path, &path)?;
                std::fs::remove_file(&tmp_path).ok();
                Ok(())
            }
            Err(e) => Err(e),
        }
    })();

    if write_result.is_err() {
        std::fs::remove_file(&tmp_path).ok();
    }
    write_result
}

pub fn write_config(config_dir: &Path, content: &str) -> bool {
    write_config_result(config_dir, content).is_ok()
}

fn unique_config_tmp_path(config_dir: &Path, label: &str) -> PathBuf {
    let unique = CONFIG_WRITE_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    config_dir.join(format!(
        ".config.{label}.{}.{}.{unique}.tmp",
        std::process::id(),
        nanos,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatspeakRnsPortConfigChange {
    Created,
    Updated,
    Unchanged,
}

/// Ensure the app-private Reticulum config matches `identity`: create it from
/// the identity when missing, or reconcile share_instance/instance_name/ports
/// toward it (preserving operator customization). The desktop app passes its
/// legacy identity (shared instance, `default`, fixed ports) so its behavior is
/// unchanged; the headless CLI passes a per-profile Standalone/derived identity.
pub fn ensure_app_private_instance_config(
    config_dir: &Path,
    identity: &RnsInstanceIdentity,
) -> std::io::Result<RatspeakRnsPortConfigChange> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config");
    if !path.exists() {
        write_config_result(config_dir, &ratspeak_config_for(identity))?;
        return Ok(RatspeakRnsPortConfigChange::Created);
    }

    let content = std::fs::read_to_string(&path)?;
    let Some(updated) = reconcile_reticulum_settings(&content, identity) else {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    };
    if updated == content {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }

    write_config_result(config_dir, &updated)?;
    Ok(RatspeakRnsPortConfigChange::Updated)
}

/// Full default config body for a given instance identity.
fn ratspeak_config_for(identity: &RnsInstanceIdentity) -> String {
    let share = if identity.share_instance { "Yes" } else { "No" };
    format!(
        "# This is the default Ratspeak Reticulum config file.\n\n[reticulum]\nenable_transport = False\nshare_instance = {share}\ninstance_name = {name}\nshared_instance_port = {shared}\ninstance_control_port = {control}\n\n[logging]\nloglevel = 4\n\n[interfaces]\n",
        name = identity.instance_name,
        shared = identity.shared_instance_port,
        control = identity.instance_control_port,
    )
}

/// The historical desktop-app default (shared instance, `default`, fixed ports).
/// Used as the read fallback for interface edits on a config-less profile.
fn ratspeak_default_config() -> String {
    ratspeak_config_for(&RnsInstanceIdentity {
        share_instance: true,
        instance_name: "default".to_string(),
        shared_instance_port: RATSPEAK_RNS_SHARED_INSTANCE_PORT,
        instance_control_port: RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
    })
}

pub fn strip_legacy_default_auto_interface(content: &str) -> String {
    if !content.contains("[[Default Interface]]") {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let had_trailing_newline = content.ends_with('\n');
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        if interface_block_name(lines[i]) == Some("Default Interface") {
            let end = next_section_header(&lines, i + 1);
            if is_legacy_default_auto_interface_block(&lines[i..end]) {
                i = end;
                continue;
            }
        }

        out.push(lines[i]);
        i += 1;
    }

    let mut result = out.join("\n");
    if had_trailing_newline && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

fn interface_block_name(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .map(str::trim)
}

fn next_section_header(lines: &[&str], start: usize) -> usize {
    lines[start..]
        .iter()
        .position(|line| line.trim().starts_with('[') && line.trim().ends_with(']'))
        .map(|offset| start + offset)
        .unwrap_or(lines.len())
}

fn config_value_eq(value: &str, expected: &str) -> bool {
    value
        .trim()
        .trim_matches('"')
        .eq_ignore_ascii_case(expected)
}

fn is_legacy_default_auto_interface_block(block: &[&str]) -> bool {
    let mut is_auto = false;

    for line in block.iter().skip(1) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = parse_key_value(trimmed) else {
            return false;
        };
        match key.as_str() {
            "type" | "interface_type" => {
                if !config_value_eq(&value, "AutoInterface") {
                    return false;
                }
                is_auto = true;
            }
            "enabled" | "interface_enabled" => {}
            _ => return false,
        }
    }

    is_auto
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortSetting {
    Missing,
    Value(u16),
    Invalid,
}

impl PortSetting {
    fn is_missing(self) -> bool {
        matches!(self, Self::Missing)
    }

    fn is_invalid(self) -> bool {
        matches!(self, Self::Invalid)
    }

    fn is_legacy_value(self, legacy: u16) -> bool {
        matches!(self, Self::Value(port) if port == legacy)
    }
}

fn reconcile_reticulum_settings(content: &str, identity: &RnsInstanceIdentity) -> Option<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let Some((section_start, section_end)) = reticulum_section_bounds(&lines) else {
        // No [reticulum] section — synthesize one from the identity.
        let share = if identity.share_instance { "Yes" } else { "No" };
        let mut new_lines = vec![
            "[reticulum]".to_string(),
            format!("share_instance = {share}"),
            format!("instance_name = {}", identity.instance_name),
            format!("shared_instance_port = {}", identity.shared_instance_port),
            format!("instance_control_port = {}", identity.instance_control_port),
            String::new(),
        ];
        new_lines.extend(lines);
        return Some(join_config_lines(new_lines));
    };

    let mut shared_line = None;
    let mut control_line = None;
    let mut share_line = None;
    let mut name_line = None;
    let mut shared = PortSetting::Missing;
    let mut control = PortSetting::Missing;
    let mut share_value: Option<String> = None;
    let mut name_value: Option<String> = None;

    for (idx, line) in lines
        .iter()
        .enumerate()
        .take(section_end)
        .skip(section_start + 1)
    {
        let Some((key, value)) = parse_ini_key_value(line) else {
            continue;
        };
        match key.as_str() {
            "shared_instance_port" => {
                if shared_line.replace(idx).is_some() {
                    return None;
                }
                shared = value
                    .parse::<u16>()
                    .map(PortSetting::Value)
                    .unwrap_or(PortSetting::Invalid);
            }
            "instance_control_port" => {
                if control_line.replace(idx).is_some() {
                    return None;
                }
                control = value
                    .parse::<u16>()
                    .map(PortSetting::Value)
                    .unwrap_or(PortSetting::Invalid);
            }
            "share_instance" => {
                if share_line.replace(idx).is_some() {
                    return None;
                }
                share_value = Some(value.trim().trim_matches('"').to_string());
            }
            "instance_name" => {
                if name_line.replace(idx).is_some() {
                    return None;
                }
                name_value = Some(value.trim().trim_matches('"').to_string());
            }
            _ => {}
        }
    }

    if shared.is_invalid() || control.is_invalid() {
        return None;
    }

    // Ports: preserve operator-custom values; only migrate missing/legacy toward
    // the identity's ports (identical to prior behavior when identity == app).
    let pair_is_defaultish = (shared.is_missing()
        || shared.is_legacy_value(LEGACY_RNS_SHARED_INSTANCE_PORT))
        && (control.is_missing() || control.is_legacy_value(LEGACY_RNS_INSTANCE_CONTROL_PORT));
    let desired_shared = if pair_is_defaultish || shared.is_missing() {
        Some(identity.shared_instance_port)
    } else {
        None
    };
    let desired_control = if pair_is_defaultish || control.is_missing() {
        Some(identity.instance_control_port)
    } else {
        None
    };

    // share_instance/instance_name: only rewrite existing lines still at the old
    // app default (share Yes/true + name default), so operator edits survive and
    // we never inject keys a hand-written config omitted.
    let share_is_defaultish = share_value
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("yes") || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let name_is_defaultish = name_value
        .as_deref()
        .map(|v| v.eq_ignore_ascii_case("default"))
        .unwrap_or(true);
    let is_old_default = share_is_defaultish && name_is_defaultish;
    let desired_share = if is_old_default && share_line.is_some() {
        Some(if identity.share_instance { "Yes" } else { "No" }.to_string())
    } else {
        None
    };
    let desired_name = if is_old_default && name_line.is_some() {
        Some(identity.instance_name.clone())
    } else {
        None
    };

    if desired_shared.is_none()
        && desired_control.is_none()
        && desired_share.is_none()
        && desired_name.is_none()
    {
        return None;
    }

    let mut inserted = Vec::new();
    if let Some(port) = desired_shared {
        if let Some(idx) = shared_line {
            lines[idx] = replace_key_line(&lines[idx], "shared_instance_port", port);
        } else {
            inserted.push(format!(
                "shared_instance_port = {}",
                identity.shared_instance_port
            ));
        }
    }
    if let Some(port) = desired_control {
        if let Some(idx) = control_line {
            lines[idx] = replace_key_line(&lines[idx], "instance_control_port", port);
        } else {
            inserted.push(format!(
                "instance_control_port = {}",
                identity.instance_control_port
            ));
        }
    }
    if let Some(share) = desired_share {
        if let Some(idx) = share_line {
            lines[idx] = replace_key_line(&lines[idx], "share_instance", share);
        }
    }
    if let Some(name) = desired_name {
        if let Some(idx) = name_line {
            lines[idx] = replace_key_line(&lines[idx], "instance_name", name);
        }
    }

    if !inserted.is_empty() {
        lines.splice(section_end..section_end, inserted);
    }

    Some(join_config_lines(lines))
}

fn reticulum_section_bounds(lines: &[String]) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|line| named_top_level_section(line, "reticulum"))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find_map(|(idx, line)| is_top_level_section(line).then_some(idx))
        .unwrap_or(lines.len());
    Some((start, end))
}

fn named_top_level_section(line: &str, name: &str) -> bool {
    let trimmed = line.trim();
    if !is_top_level_section(line) {
        return false;
    }
    trimmed[1..trimmed.len() - 1]
        .trim()
        .eq_ignore_ascii_case(name)
}

fn is_top_level_section(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[')
        && trimmed.ends_with(']')
        && !trimmed.starts_with("[[")
        && trimmed.len() >= 3
}

fn parse_ini_key_value(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((
        key.to_ascii_lowercase(),
        unquote_ini_scalar(strip_inline_comment(value).trim()).to_string(),
    ))
}

fn strip_inline_comment(value: &str) -> &str {
    let mut quote = None;
    for (idx, ch) in value.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '#' {
            return &value[..idx];
        }
    }
    value
}

fn unquote_ini_scalar(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn replace_key_line(line: &str, key: &str, value: impl std::fmt::Display) -> String {
    let indent = line
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>();
    format!("{indent}{key} = {value}")
}

fn join_config_lines(lines: Vec<String>) -> String {
    let mut content = lines.join("\n");
    content.push('\n');
    content
}

/// Used by the BLE↔USB priority handoff.
pub fn rnode_names_with_port_prefix(config_dir: &Path, prefix: &str) -> Vec<String> {
    let v = get_all_interfaces(config_dir);
    let mut names = Vec::new();
    if let Some(arr) = v.get("rnode").and_then(|v| v.as_array()) {
        for entry in arr {
            let port = entry.get("port").and_then(|v| v.as_str()).unwrap_or("");
            if port.starts_with(prefix)
                && let Some(name) = entry.get("name").and_then(|v| v.as_str())
            {
                names.push(name.to_string());
            }
        }
    }
    names
}

pub fn get_all_interfaces(config_dir: &Path) -> Value {
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => {
            return json!({
                "rnode": [],
                "auto": [],
                "tcp_client": [],
                "tcp_server": [],
                "backbone_client": [],
                "backbone_server": [],
            });
        }
    };

    let mut rnode = Vec::new();
    let mut auto = Vec::new();
    let mut tcp_client = Vec::new();
    let mut tcp_server = Vec::new();
    let mut backbone_client = Vec::new();
    let mut backbone_server = Vec::new();

    let mut current_section: Option<String> = None;
    let mut current_iface: Option<(String, String, std::collections::HashMap<String, String>)> =
        None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
            if let Some((name, itype, props)) = current_iface.take() {
                let entry = build_interface_entry(&name, &itype, &props);
                match itype.as_str() {
                    "RNodeInterface" => rnode.push(entry),
                    "AutoInterface" => auto.push(entry),
                    "TCPClientInterface" => tcp_client.push(entry),
                    "TCPServerInterface" => tcp_server.push(entry),
                    "BackboneInterface" => {
                        if props.contains_key("target_host") {
                            backbone_client.push(entry);
                        } else {
                            backbone_server.push(entry);
                        }
                    }
                    _ => {}
                }
            }

            let name = trimmed
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .trim()
                .to_string();
            current_section = Some(name.clone());
            current_iface = None;
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[[") {
            let _section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            continue;
        }

        if let Some(ref section_name) = current_section
            && let Some((key, value)) = parse_key_value(trimmed)
        {
            if key == "type" || key == "interface_type" {
                current_iface = Some((
                    section_name.clone(),
                    value.clone(),
                    std::collections::HashMap::new(),
                ));
            }
            if let Some((_, _, ref mut props)) = current_iface {
                props.insert(key, value);
            }
        }
    }

    if let Some((name, itype, props)) = current_iface {
        let entry = build_interface_entry(&name, &itype, &props);
        match itype.as_str() {
            "RNodeInterface" => rnode.push(entry),
            "AutoInterface" => auto.push(entry),
            "TCPClientInterface" => tcp_client.push(entry),
            "TCPServerInterface" => tcp_server.push(entry),
            "BackboneInterface" => {
                if props.contains_key("target_host") {
                    backbone_client.push(entry);
                } else {
                    backbone_server.push(entry);
                }
            }
            _ => {}
        }
    }

    json!({
        "rnode": rnode,
        "auto": auto,
        "tcp_client": tcp_client,
        "tcp_server": tcp_server,
        "backbone_client": backbone_client,
        "backbone_server": backbone_server,
    })
}

fn parse_key_value(line: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() == 2 {
        let key = parts[0].trim().to_lowercase();
        let value = parts[1].trim().to_string();
        Some((key, value))
    } else {
        None
    }
}

fn build_interface_entry(
    name: &str,
    itype: &str,
    props: &std::collections::HashMap<String, String>,
) -> Value {
    let mut entry = json!({
        "name": name,
        "type": itype,
    });
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in props {
            obj.insert(k.clone(), json!(v));
        }
    }
    entry
}

fn safe_interface_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '[' | ']' | '=' | '#'))
}

fn safe_config_scalar(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c == '\r' || c == '\n' || c == '\0' || c == '#')
}

fn safe_optional_scalar(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.is_empty() || safe_config_scalar(value))
}

fn safe_config_list(values: Option<&Vec<String>>) -> bool {
    values.is_none_or(|values| {
        values
            .iter()
            .all(|value| safe_config_scalar(value) && !value.contains(','))
    })
}

fn safe_auto_options(opts: &AutoInterfaceOptions) -> bool {
    safe_optional_scalar(opts.group_id.as_deref())
        && safe_optional_scalar(opts.discovery_scope.as_deref())
        && safe_optional_scalar(opts.multicast_address_type.as_deref())
        && safe_config_list(opts.devices.as_ref())
        && safe_config_list(opts.ignored_devices.as_ref())
}

fn safe_rnode_args(args: RnodeInterfaceArgs<'_>) -> bool {
    safe_interface_name(args.name)
        && safe_config_scalar(args.port)
        && normalize_rnode_interface_mode(args.mode).is_some()
        && safe_optional_scalar(args.region_key)
        && safe_optional_scalar(args.preset_key)
        && safe_public_map_args(args.public_map)
}

pub fn normalize_rnode_interface_mode(mode: Option<&str>) -> Option<&'static str> {
    let mode = mode
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .unwrap_or(RNODE_DEFAULT_INTERFACE_MODE);
    let key = mode.to_ascii_lowercase();
    match key.as_str() {
        "full" => Some("full"),
        "gateway" | "gw" => Some("gateway"),
        "access_point" | "accesspoint" | "access point" | "ap" => Some("access_point"),
        "boundary" => Some("boundary"),
        "roaming" => Some("roaming"),
        _ => None,
    }
}

pub fn rnode_interface_mode_value(
    mode: Option<&str>,
) -> Option<rns_interface::traits::InterfaceMode> {
    match normalize_rnode_interface_mode(mode)? {
        "full" => Some(rns_interface::traits::InterfaceMode::Full),
        "gateway" => Some(rns_interface::traits::InterfaceMode::Gateway),
        "access_point" => Some(rns_interface::traits::InterfaceMode::AccessPoint),
        "boundary" => Some(rns_interface::traits::InterfaceMode::Boundary),
        "roaming" => Some(rns_interface::traits::InterfaceMode::Roaming),
        _ => None,
    }
}

fn safe_backbone_client_args(args: BackboneClientArgs<'_>) -> bool {
    safe_interface_name(args.name) && safe_config_scalar(args.host) && safe_ifac_args(args.ifac)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InterfaceIfacArgs<'a> {
    pub network_name: Option<&'a str>,
    pub passphrase: Option<&'a str>,
    pub ifac_size: Option<usize>,
}

fn safe_ifac_args(args: InterfaceIfacArgs<'_>) -> bool {
    safe_optional_scalar(args.network_name)
        && safe_optional_scalar(args.passphrase)
        && args.ifac_size.is_none_or(|size| (1..=64).contains(&size))
}

/// Removes any existing block with the same `name` before insertion.
pub fn add_rnode_interface(config_dir: &Path, args: RnodeInterfaceArgs<'_>) -> bool {
    if !safe_rnode_args(args) {
        return false;
    }
    let block = rnode_interface_block(args);
    upsert_interface_block(config_dir, &[args.name], &block)
}

#[derive(Clone, Copy)]
/// RNode interface settings written into the Ratspeak-owned Reticulum config.
pub struct RnodeInterfaceArgs<'a> {
    pub name: &'a str,
    pub port: &'a str,
    pub mode: Option<&'a str>,
    pub frequency: u64,
    pub bandwidth: u64,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
    pub region_key: Option<&'a str>,
    pub preset_key: Option<&'a str>,
    pub airtime_limit_short: Option<f64>,
    pub airtime_limit_long: Option<f64>,
    pub public_map: RnodePublicMapArgs<'a>,
}

#[derive(Clone, Copy, Debug, Default)]
/// Public interface discovery metadata for Ratspeak map publishing.
pub struct RnodePublicMapArgs<'a> {
    pub discoverable: bool,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub discovery_name: Option<&'a str>,
}

fn safe_public_map_args(args: RnodePublicMapArgs<'_>) -> bool {
    if !safe_optional_scalar(args.discovery_name) {
        return false;
    }
    if !args.discoverable {
        return true;
    }
    let Some(latitude) = args.latitude else {
        return false;
    };
    let Some(longitude) = args.longitude else {
        return false;
    };
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

pub fn auto_interface_names(config_dir: &Path) -> Vec<String> {
    get_all_interfaces(config_dir)
        .get("auto")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| entry.get("name").and_then(|v| v.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn remove_interface(config_dir: &Path, name: &str) -> bool {
    remove_interfaces(config_dir, &[name.to_string()])
}

pub fn remove_interfaces(config_dir: &Path, names: &[String]) -> bool {
    if names.iter().any(|name| !safe_interface_name(name)) {
        return false;
    }
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };

    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    let (result, _) = remove_interface_blocks_from_content(&content, &name_refs);
    write_config(config_dir, &result)
}

pub fn set_interface_enabled(config_dir: &Path, name: &str, enabled: bool) -> bool {
    if !safe_interface_name(name) {
        return false;
    }
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut result = Vec::with_capacity(lines.len() + 1);
    let mut found = false;
    let enabled_value = if enabled { "true" } else { "false" };
    let mut i = 0;

    while i < lines.len() {
        if interface_block_name(lines[i]) == Some(name) {
            found = true;
            let end = next_section_header(&lines, i + 1);
            let mut block = lines[i..end]
                .iter()
                .map(|line| (*line).to_string())
                .collect::<Vec<_>>();
            let mut saw_enabled_key = false;
            let mut insert_idx = 1;

            for (idx, line) in block.iter_mut().enumerate().skip(1) {
                if let Some((key, _)) = parse_ini_key_value(line) {
                    let key = key.to_string();
                    if key == "type" || key == "interface_type" {
                        insert_idx = idx + 1;
                    }
                    if key == "enabled" || key == "interface_enabled" {
                        let indent = line
                            .chars()
                            .take_while(|ch| ch.is_whitespace())
                            .collect::<String>();
                        *line = format!("{indent}{key} = {enabled_value}");
                        saw_enabled_key = true;
                    }
                }
            }

            if !saw_enabled_key {
                block.insert(insert_idx, format!("    enabled = {enabled_value}"));
            }

            result.extend(block);
            i = end;
            continue;
        }

        result.push(lines[i].to_string());
        i += 1;
    }

    if !found {
        return false;
    }

    write_config(config_dir, &join_config_lines(result))
}

fn remove_interface_blocks_from_content(content: &str, names: &[&str]) -> (String, bool) {
    let headers: std::collections::HashSet<String> = names
        .iter()
        .filter(|name| !name.trim().is_empty())
        .map(|name| format!("[[{}]]", name.trim()))
        .collect();
    let mut result = String::new();
    let mut skipping = false;
    let mut removed = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if skipping && trimmed.starts_with('[') {
            skipping = false;
        }
        if !skipping
            && trimmed.starts_with("[[")
            && trimmed.ends_with("]]")
            && headers.contains(trimmed)
        {
            skipping = true;
            removed = true;
            continue;
        }
        if !skipping {
            result.push_str(line);
            result.push('\n');
        }
    }

    (result, removed)
}

/// Insert `block` inside `[interfaces]`; create the section if missing.
fn insert_interface_block(content: &str, block: &str) -> String {
    let mut result = String::new();
    let mut in_interfaces = false;
    let mut inserted = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "[interfaces]" {
            in_interfaces = true;
            result.push_str(line);
            result.push('\n');
            continue;
        }

        if in_interfaces && trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            if !inserted {
                result.push_str(block);
                inserted = true;
            }
            in_interfaces = false;
        }

        result.push_str(line);
        result.push('\n');
    }

    if in_interfaces && !inserted {
        result.push_str(block);
    }

    if !inserted && !content.contains("[interfaces]") {
        result.push_str("\n[interfaces]\n");
        result.push_str(block);
    }

    result
}

fn upsert_interface_block(config_dir: &Path, remove_names: &[&str], block: &str) -> bool {
    if remove_names.iter().any(|name| !safe_interface_name(name)) {
        return false;
    }
    let content = read_config(config_dir).unwrap_or_default();
    let (content, _) = remove_interface_blocks_from_content(&content, remove_names);
    let new_content = insert_interface_block(&content, block);
    write_config(config_dir, &new_content)
}

fn replace_interface_block(config_dir: &Path, old_name: &str, new_name: &str, block: &str) -> bool {
    if !safe_interface_name(old_name) || !safe_interface_name(new_name) {
        return false;
    }
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };
    let remove_names = if old_name == new_name {
        vec![old_name]
    } else {
        vec![old_name, new_name]
    };
    let (content, removed_old) = remove_interface_blocks_from_content(&content, &remove_names);
    if !removed_old {
        return false;
    }
    let new_content = insert_interface_block(&content, block);
    write_config(config_dir, &new_content)
}

fn rnode_interface_block(args: RnodeInterfaceArgs<'_>) -> String {
    let RnodeInterfaceArgs {
        name,
        port,
        mode,
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power,
        region_key,
        preset_key,
        airtime_limit_short,
        airtime_limit_long,
        public_map,
    } = args;
    let mode = normalize_rnode_interface_mode(mode).unwrap_or(RNODE_DEFAULT_INTERFACE_MODE);

    let mut block = format!(
        "\n  [[{name}]]\n    type = RNodeInterface\n    port = {port}\n    mode = {mode}\n    frequency = {frequency}\n    bandwidth = {bandwidth}\n    spreadingfactor = {spreading_factor}\n    codingrate = {coding_rate}\n    txpower = {tx_power}\n    enabled = true\n"
    );
    if let Some(v) = airtime_limit_short {
        block.push_str(&format!("    airtime_limit_short = {v}\n"));
    }
    if let Some(v) = airtime_limit_long {
        block.push_str(&format!("    airtime_limit_long = {v}\n"));
    }
    append_public_map_fields(&mut block, public_map);
    if let Some(region_key) = region_key {
        block.push_str(&format!("    ratspeak_region = {region_key}\n"));
    }
    if let Some(preset_key) = preset_key {
        block.push_str(&format!("    ratspeak_preset = {preset_key}\n"));
    }
    block
}

fn append_public_map_fields(block: &mut String, public_map: RnodePublicMapArgs<'_>) {
    if !public_map.discoverable {
        return;
    }
    let Some(latitude) = public_map.latitude else {
        return;
    };
    let Some(longitude) = public_map.longitude else {
        return;
    };
    block.push_str("    discoverable = yes\n");
    block.push_str(&format!("    latitude = {latitude}\n"));
    block.push_str(&format!("    longitude = {longitude}\n"));
    if let Some(discovery_name) = public_map.discovery_name.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    discovery_name = {discovery_name}\n"));
    }
}

fn append_ifac_fields(block: &mut String, ifac: InterfaceIfacArgs<'_>) {
    if let Some(network_name) = ifac.network_name.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    network_name = {network_name}\n"));
    }
    if let Some(passphrase) = ifac.passphrase.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    passphrase = {passphrase}\n"));
    }
    if let Some(ifac_size) = ifac.ifac_size {
        block.push_str(&format!("    ifac_size = {ifac_size}\n"));
    }
}

fn tcp_client_block(name: &str, host: &str, port: u16, ifac: InterfaceIfacArgs<'_>) -> String {
    let mut block = format!(
        "\n  [[{name}]]\n    type = TCPClientInterface\n    target_host = {host}\n    target_port = {port}\n    enabled = true\n"
    );
    append_ifac_fields(&mut block, ifac);
    block
}

fn tcp_server_block(name: &str, listen_port: u16, listen_ip: &str) -> String {
    format!(
        "\n  [[{name}]]\n    type = TCPServerInterface\n    listen_ip = {listen_ip}\n    listen_port = {listen_port}\n    enabled = true\n"
    )
}

#[derive(Clone, Copy)]
/// Backbone client settings written into the Ratspeak-owned Reticulum config.
pub struct BackboneClientArgs<'a> {
    pub name: &'a str,
    pub host: &'a str,
    pub port: u16,
    pub prefer_ipv6: bool,
    pub connect_timeout: Option<u64>,
    pub max_reconnect_tries: Option<usize>,
    pub i2p_tunneled: bool,
    pub ifac: InterfaceIfacArgs<'a>,
}

fn backbone_client_block(args: BackboneClientArgs<'_>) -> String {
    let BackboneClientArgs {
        name,
        host,
        port,
        prefer_ipv6,
        connect_timeout,
        max_reconnect_tries,
        i2p_tunneled,
        ifac,
    } = args;

    let mut block = format!(
        "\n  [[{name}]]\n    type = BackboneInterface\n    target_host = {host}\n    target_port = {port}\n    enabled = true\n"
    );
    if prefer_ipv6 {
        block.push_str("    prefer_ipv6 = true\n");
    }
    if let Some(t) = connect_timeout {
        block.push_str(&format!("    connect_timeout = {t}\n"));
    }
    if let Some(m) = max_reconnect_tries {
        block.push_str(&format!("    max_reconnect_tries = {m}\n"));
    }
    if i2p_tunneled {
        block.push_str("    i2p_tunneled = true\n");
    }
    append_ifac_fields(&mut block, ifac);
    block
}

fn backbone_server_block(
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    prefer_ipv6: bool,
    device: Option<&str>,
) -> String {
    let mut block = format!(
        "\n  [[{name}]]\n    type = BackboneInterface\n    listen_on = {listen_ip}\n    listen_port = {listen_port}\n    enabled = true\n"
    );
    if prefer_ipv6 {
        block.push_str("    prefer_ipv6 = true\n");
    }
    if let Some(d) = device.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    device = {d}\n"));
    }
    block
}

/// Optional `AutoInterface` settings; `None` keeps the on-disk config minimal.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AutoInterfaceOptions {
    pub group_id: Option<String>,
    pub discovery_scope: Option<String>,
    pub discovery_port: Option<u16>,
    pub data_port: Option<u16>,
    pub multicast_address_type: Option<String>,
    pub devices: Option<Vec<String>>,
    pub ignored_devices: Option<Vec<String>>,
    pub configured_bitrate: Option<u64>,
}

pub fn add_auto_interface(config_dir: &Path, name: &str, opts: &AutoInterfaceOptions) -> bool {
    if !safe_interface_name(name) || !safe_auto_options(opts) {
        return false;
    }
    let mut block = format!("\n  [[{name}]]\n    type = AutoInterface\n    enabled = true\n");
    if let Some(g) = opts.group_id.as_deref().filter(|s| !s.is_empty()) {
        block.push_str(&format!("    group_id = {g}\n"));
    }
    if let Some(s) = opts.discovery_scope.as_deref().filter(|s| !s.is_empty()) {
        block.push_str(&format!("    discovery_scope = {s}\n"));
    }
    if let Some(p) = opts.discovery_port {
        block.push_str(&format!("    discovery_port = {p}\n"));
    }
    if let Some(p) = opts.data_port {
        block.push_str(&format!("    data_port = {p}\n"));
    }
    if let Some(t) = opts
        .multicast_address_type
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        block.push_str(&format!("    multicast_address_type = {t}\n"));
    }
    if let Some(devs) = opts.devices.as_ref().filter(|v| !v.is_empty()) {
        block.push_str(&format!("    devices = {}\n", devs.join(",")));
    }
    if let Some(devs) = opts.ignored_devices.as_ref().filter(|v| !v.is_empty()) {
        block.push_str(&format!("    ignored_devices = {}\n", devs.join(",")));
    }
    if let Some(b) = opts.configured_bitrate {
        block.push_str(&format!("    configured_bitrate = {b}\n"));
    }
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn add_tcp_client(config_dir: &Path, name: &str, host: &str, port: u16) -> bool {
    add_tcp_client_with_ifac(config_dir, name, host, port, InterfaceIfacArgs::default())
}

pub fn add_tcp_client_with_ifac(
    config_dir: &Path,
    name: &str,
    host: &str,
    port: u16,
    ifac: InterfaceIfacArgs<'_>,
) -> bool {
    if !safe_interface_name(name) || !safe_config_scalar(host) {
        return false;
    }
    if !safe_ifac_args(ifac) {
        return false;
    }
    let block = tcp_client_block(name, host, port, ifac);
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn add_tcp_server(config_dir: &Path, name: &str, listen_port: u16, listen_ip: &str) -> bool {
    if !safe_interface_name(name) || !safe_config_scalar(listen_ip) {
        return false;
    }
    let block = tcp_server_block(name, listen_port, listen_ip);
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn add_backbone_client(config_dir: &Path, args: BackboneClientArgs<'_>) -> bool {
    if !safe_backbone_client_args(args) {
        return false;
    }
    let block = backbone_client_block(args);
    upsert_interface_block(config_dir, &[args.name], &block)
}

pub fn add_backbone_server(
    config_dir: &Path,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    prefer_ipv6: bool,
    device: Option<&str>,
) -> bool {
    if !safe_interface_name(name) || !safe_config_scalar(listen_ip) || !safe_optional_scalar(device)
    {
        return false;
    }
    // Synthesizer reads `listen_on` (interface_factory.rs); IPC arg is
    // `listen_ip` for parity with the TCP server command.
    let block = backbone_server_block(name, listen_port, listen_ip, prefer_ipv6, device);
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn update_rnode_interface(
    config_dir: &Path,
    old_name: &str,
    args: RnodeInterfaceArgs<'_>,
) -> bool {
    if !safe_interface_name(old_name) || !safe_rnode_args(args) {
        return false;
    }
    let block = rnode_interface_block(args);
    replace_interface_block(config_dir, old_name, args.name, &block)
}

pub fn update_tcp_client(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    host: &str,
    port: u16,
) -> bool {
    update_tcp_client_with_ifac(
        config_dir,
        old_name,
        name,
        host,
        port,
        InterfaceIfacArgs::default(),
    )
}

pub fn update_tcp_client_with_ifac(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    host: &str,
    port: u16,
    ifac: InterfaceIfacArgs<'_>,
) -> bool {
    if !safe_interface_name(old_name) || !safe_interface_name(name) || !safe_config_scalar(host) {
        return false;
    }
    if !safe_ifac_args(ifac) {
        return false;
    }
    let block = tcp_client_block(name, host, port, ifac);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn update_tcp_server(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
) -> bool {
    if !safe_interface_name(old_name)
        || !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
    {
        return false;
    }
    let block = tcp_server_block(name, listen_port, listen_ip);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn update_backbone_client(
    config_dir: &Path,
    old_name: &str,
    args: BackboneClientArgs<'_>,
) -> bool {
    if !safe_interface_name(old_name) || !safe_backbone_client_args(args) {
        return false;
    }
    let block = backbone_client_block(args);
    replace_interface_block(config_dir, old_name, args.name, &block)
}

pub fn update_backbone_server(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    prefer_ipv6: bool,
    device: Option<&str>,
) -> bool {
    if !safe_interface_name(old_name)
        || !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
        || !safe_optional_scalar(device)
    {
        return false;
    }
    let block = backbone_server_block(name, listen_port, listen_ip, prefer_ipv6, device);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn set_transport_mode(config_dir: &Path, enable: bool) -> bool {
    let content = read_config(config_dir).unwrap_or_else(ratspeak_default_config);
    write_config(config_dir, &transport_mode_update(&content, enable))
}

fn transport_mode_update(content: &str, enable: bool) -> String {
    let val = if enable { "True" } else { "False" };
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let Some((section_start, section_end)) = reticulum_section_bounds(&lines) else {
        let mut new_lines = vec![
            "[reticulum]".to_string(),
            format!("enable_transport = {val}"),
            String::new(),
        ];
        new_lines.extend(lines);
        return join_config_lines(new_lines);
    };

    for idx in (section_start + 1)..section_end {
        if parse_ini_key_value(&lines[idx]).is_some_and(|(key, _)| key == "enable_transport") {
            lines[idx] = replace_key_line(&lines[idx], "enable_transport", val);
            return join_config_lines(lines);
        }
    }

    lines.splice(
        section_end..section_end,
        [format!("enable_transport = {val}")],
    );
    join_config_lines(lines)
}

pub fn transport_mode_enabled(config_dir: &Path) -> bool {
    read_config(config_dir)
        .and_then(|content| transport_enabled_from_config(&content))
        .unwrap_or(false)
}

fn transport_enabled_from_config(content: &str) -> Option<bool> {
    let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let (section_start, section_end) = reticulum_section_bounds(&lines)?;
    lines
        .iter()
        .take(section_end)
        .skip(section_start + 1)
        .find_map(|line| {
            let (key, value) = parse_ini_key_value(line)?;
            if key == "enable_transport" {
                Some(matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "1" | "on"
                ))
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// The desktop-app legacy identity (shared instance, `default`, fixed ports).
    fn app_identity() -> RnsInstanceIdentity {
        RnsInstanceIdentity {
            share_instance: true,
            instance_name: "default".to_string(),
            shared_instance_port: RATSPEAK_RNS_SHARED_INSTANCE_PORT,
            instance_control_port: RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
        }
    }

    fn temp_config_dir() -> std::path::PathBuf {
        let unique = TEMP_CONFIG_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ratspeak-rns-config-test-{}-{nanos}-{unique}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_base_config(dir: &Path) {
        write_config(
            dir,
            "[reticulum]\n  enable_transport = False\n\n[interfaces]\n  [[Keep]]\n    type = TCPClientInterface\n    target_host = keep.example\n    target_port = 1000\n    enabled = true\n\n[logging]\n  loglevel = 3\n",
        );
    }

    fn count_header(content: &str, name: &str) -> usize {
        let needle = format!("[[{name}]]");
        content.lines().filter(|line| line.trim() == needle).count()
    }

    #[test]
    fn write_config_creates_missing_config_dir() {
        let dir = temp_config_dir().join("identity").join("reticulum");
        assert!(!dir.exists());

        write_config_result(&dir, "[interfaces]\n").unwrap();

        assert_eq!(read_config(&dir).unwrap(), "[interfaces]\n");
        assert!(dir.join("config").exists());
    }

    #[test]
    fn set_transport_mode_creates_missing_config_dir() {
        let dir = temp_config_dir().join("identity").join("reticulum");

        assert!(set_transport_mode(&dir, true));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("enable_transport = True"));
        assert!(content.contains("[interfaces]"));
    }

    #[test]
    fn set_transport_mode_inserts_missing_key_in_reticulum_section() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshare_instance = Yes\n\n[interfaces]\n  [[Keep]]\n    type = TCPClientInterface\n",
        );

        assert!(set_transport_mode(&dir, true));

        let content = read_config(&dir).unwrap();
        let reticulum = content.split("[interfaces]").next().unwrap();
        assert!(reticulum.contains("enable_transport = True"));
        assert!(
            !content
                .split("[interfaces]")
                .nth(1)
                .unwrap()
                .contains("enable_transport")
        );
        assert!(transport_mode_enabled(&dir));
    }

    #[test]
    fn transport_mode_enabled_ignores_non_reticulum_key() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshare_instance = Yes\n\n[interfaces]\nenable_transport = True\n",
        );

        assert!(!transport_mode_enabled(&dir));
    }

    #[test]
    fn ensure_app_private_ports_creates_ratspeak_config() {
        let dir = temp_config_dir();

        let change = ensure_app_private_instance_config(&dir, &app_identity()).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Created);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("shared_instance_port = 37430"));
        assert!(content.contains("instance_control_port = 37431"));
        assert!(content.contains("[interfaces]"));
        assert!(content.contains("share_instance = Yes"));
        assert!(content.contains("instance_name = default"));
        assert!(!content.contains("[[Default Interface]]"));
        assert!(!content.contains("type = AutoInterface"));
        assert!(!dir.join("config.backup").exists());
    }

    #[test]
    fn ensure_standalone_writes_share_instance_no_and_derived_name() {
        let dir = temp_config_dir();
        let identity = ratspeak_core::config::derive_rns_instance_identity(
            std::path::Path::new("/tmp/ratspeak-bot-standalone"),
            false,
            None,
            true,
        );
        let change = ensure_app_private_instance_config(&dir, &identity).unwrap();
        assert_eq!(change, RatspeakRnsPortConfigChange::Created);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("share_instance = No"));
        assert!(content.contains("instance_name = rsk-"));
        assert!(!content.contains("instance_name = default"));
    }

    #[test]
    fn reconcile_flips_old_default_to_standalone() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nenable_transport = False\nshare_instance = Yes\ninstance_name = default\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
        );
        let identity = ratspeak_core::config::derive_rns_instance_identity(
            std::path::Path::new("/tmp/ratspeak-bot-upgrade"),
            false,
            None,
            true,
        );
        let change = ensure_app_private_instance_config(&dir, &identity).unwrap();
        assert_eq!(change, RatspeakRnsPortConfigChange::Updated);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("share_instance = No"));
        assert!(content.contains(&format!("instance_name = {}", identity.instance_name)));
        assert!(!content.contains("instance_name = default"));
    }

    #[test]
    fn reconcile_preserves_operator_customized_config() {
        let dir = temp_config_dir();
        // Operator explicitly chose a non-default name + custom ports.
        write_config(
            &dir,
            "[reticulum]\nshare_instance = Yes\ninstance_name = my-share\nshared_instance_port = 39000\ninstance_control_port = 39001\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();
        let identity = ratspeak_core::config::derive_rns_instance_identity(
            std::path::Path::new("/tmp/ratspeak-bot-custom"),
            false,
            None,
            true,
        );
        let change = ensure_app_private_instance_config(&dir, &identity).unwrap();
        assert_eq!(change, RatspeakRnsPortConfigChange::Unchanged);
        assert_eq!(read_config(&dir).unwrap(), before);
    }

    #[test]
    fn strip_legacy_default_auto_interface_removes_seeded_default_only() {
        let content = "[reticulum]\nshare_instance = Yes\n\n[interfaces]\n  [[Default Interface]]\n    type = AutoInterface\n    enabled = True\n\n  [[TCP Node]]\n    type = TCPClientInterface\n    target_host = example.org\n    target_port = 4242\n    enabled = true\n";

        let stripped = strip_legacy_default_auto_interface(content);

        assert!(!stripped.contains("[[Default Interface]]"));
        assert!(!stripped.contains("type = AutoInterface"));
        assert!(stripped.contains("[[TCP Node]]"));
        assert!(stripped.ends_with('\n'));
    }

    #[test]
    fn strip_legacy_default_auto_interface_preserves_custom_auto_config() {
        let content = "[interfaces]\n  [[Default Interface]]\n    type = AutoInterface\n    enabled = True\n    group_id = private\n";

        assert_eq!(strip_legacy_default_auto_interface(content), content);
    }

    #[test]
    fn ensure_app_private_ports_migrates_legacy_defaults_with_backup() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshare_instance = Yes\nshared_instance_port = 37428\ninstance_control_port = 37429\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        let change = ensure_app_private_instance_config(&dir, &app_identity()).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Updated);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("shared_instance_port = 37430"));
        assert!(content.contains("instance_control_port = 37431"));
        assert!(!content.contains("shared_instance_port = 37428"));
        assert!(!content.contains("instance_control_port = 37429"));
        assert_eq!(
            std::fs::read_to_string(dir.join("config.backup")).unwrap(),
            before
        );
    }

    #[test]
    fn ensure_app_private_ports_inserts_when_runtime_default_omits_ports() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nenable_transport = False\nshare_instance = Yes\ninstance_name = default\n\n[interfaces]\n",
        );

        let change = ensure_app_private_instance_config(&dir, &app_identity()).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Updated);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("shared_instance_port = 37430"));
        assert!(content.contains("instance_control_port = 37431"));
    }

    #[test]
    fn ensure_app_private_ports_preserves_custom_port_pair() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshared_instance_port = 39000\ninstance_control_port = 39001\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        let change = ensure_app_private_instance_config(&dir, &app_identity()).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Unchanged);
        assert_eq!(read_config(&dir).unwrap(), before);
        assert!(!dir.join("config.backup").exists());
    }

    #[test]
    fn ensure_app_private_ports_skips_invalid_or_duplicate_ports() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshared_instance_port = nope\ninstance_control_port = 37429\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        let change = ensure_app_private_instance_config(&dir, &app_identity()).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Unchanged);
        assert_eq!(read_config(&dir).unwrap(), before);
        assert!(!dir.join("config.backup").exists());
    }

    #[test]
    fn update_tcp_client_renames_without_duplicate_blocks() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_tcp_client(&dir, "Old TCP", "old.example", 4242));

        assert!(update_tcp_client(
            &dir,
            "Old TCP",
            "New TCP",
            "new.example",
            4243
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Old TCP"), 0);
        assert_eq!(count_header(&content, "New TCP"), 1);
        assert_eq!(count_header(&content, "Keep"), 1);
        assert!(content.contains("target_host = new.example"));
        assert!(content.contains("target_port = 4243"));
        assert!(content.contains("[logging]"));
    }

    #[test]
    fn tcp_client_ifac_writes_passphrase_without_network_name() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_tcp_client_with_ifac(
            &dir,
            "Private TCP",
            "private.example",
            4242,
            InterfaceIfacArgs {
                network_name: None,
                passphrase: Some("secret"),
                ifac_size: None,
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("[[Private TCP]]"));
        assert!(content.contains("passphrase = secret"));
        assert!(!content.contains("network_name ="));
        assert!(!content.contains("ifac_size ="));
    }

    #[test]
    fn update_tcp_client_ifac_can_preserve_size() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_tcp_client_with_ifac(
            &dir,
            "Old TCP",
            "old.example",
            4242,
            InterfaceIfacArgs {
                network_name: Some("mesh"),
                passphrase: Some("secret"),
                ifac_size: Some(8),
            },
        ));

        assert!(update_tcp_client_with_ifac(
            &dir,
            "Old TCP",
            "New TCP",
            "new.example",
            4243,
            InterfaceIfacArgs {
                network_name: Some("mesh"),
                passphrase: Some("changed"),
                ifac_size: Some(8),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Old TCP"), 0);
        assert_eq!(count_header(&content, "New TCP"), 1);
        assert!(content.contains("network_name = mesh"));
        assert!(content.contains("passphrase = changed"));
        assert!(content.contains("ifac_size = 8"));
    }

    #[test]
    fn backbone_client_ifac_writes_network_name_and_passphrase() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_backbone_client(
            &dir,
            BackboneClientArgs {
                name: "Private Backbone",
                host: "backbone.example",
                port: 4242,
                prefer_ipv6: false,
                connect_timeout: None,
                max_reconnect_tries: None,
                i2p_tunneled: false,
                ifac: InterfaceIfacArgs {
                    network_name: Some("mesh"),
                    passphrase: Some("secret"),
                    ifac_size: None,
                },
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("[[Private Backbone]]"));
        assert!(content.contains("type = BackboneInterface"));
        assert!(content.contains("network_name = mesh"));
        assert!(content.contains("passphrase = secret"));
    }

    #[test]
    fn update_tcp_server_same_name_replaces_existing_block() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_tcp_server(&dir, "Host", 4242, "0.0.0.0"));

        assert!(update_tcp_server(&dir, "Host", "Host", 5252, "127.0.0.1"));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Host"), 1);
        assert!(content.contains("listen_ip = 127.0.0.1"));
        assert!(content.contains("listen_port = 5252"));
        assert!(!content.contains("listen_port = 4242"));
    }

    #[test]
    fn update_missing_interface_returns_false_and_preserves_config() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        let before = read_config(&dir).unwrap();

        assert!(!update_tcp_client(
            &dir,
            "Missing",
            "New",
            "new.example",
            4242
        ));

        assert_eq!(read_config(&dir).unwrap(), before);
    }

    #[test]
    fn add_tcp_client_upserts_existing_name() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_tcp_client(&dir, "Hub", "one.example", 1001));
        assert!(add_tcp_client(&dir, "Hub", "two.example", 1002));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Hub"), 1);
        assert!(!content.contains("one.example"));
        assert!(content.contains("two.example"));
    }

    #[test]
    fn unsafe_interface_config_values_are_rejected_without_writing() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        let before = read_config(&dir).unwrap();

        assert!(!add_tcp_client(
            &dir,
            "Injected]]\n[logging]",
            "host.example",
            4242
        ));
        assert!(!add_auto_interface(
            &dir,
            "Local Network",
            &AutoInterfaceOptions {
                devices: Some(vec!["eth0\n  [[Injected]]".to_string()]),
                ..AutoInterfaceOptions::default()
            }
        ));

        assert_eq!(read_config(&dir).unwrap(), before);
    }

    #[test]
    fn auto_names_and_batch_remove_handle_default_and_custom_blocks() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_auto_interface(
            &dir,
            "Default Interface",
            &AutoInterfaceOptions::default()
        ));
        assert!(add_auto_interface(
            &dir,
            "Local Network",
            &AutoInterfaceOptions {
                group_id: Some("field".to_string()),
                discovery_scope: Some("site".to_string()),
                discovery_port: Some(30_000),
                data_port: Some(30_001),
                multicast_address_type: None,
                devices: None,
                ignored_devices: None,
                configured_bitrate: None,
            }
        ));

        let names = auto_interface_names(&dir);
        assert_eq!(names, vec!["Default Interface", "Local Network"]);

        assert!(remove_interfaces(&dir, &names));
        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Default Interface"), 0);
        assert_eq!(count_header(&content, "Local Network"), 0);
        assert_eq!(count_header(&content, "Keep"), 1);
    }

    #[test]
    fn set_interface_enabled_toggles_existing_enabled_key() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(set_interface_enabled(&dir, "Keep", false));
        let content = read_config(&dir).unwrap();
        assert!(content.contains("[[Keep]]"));
        assert!(content.contains("enabled = false"));
        assert!(content.contains("target_host = keep.example"));

        assert!(set_interface_enabled(&dir, "Keep", true));
        let content = read_config(&dir).unwrap();
        assert!(content.contains("enabled = true"));
    }

    #[test]
    fn set_interface_enabled_handles_legacy_interface_enabled_key() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[interfaces]\n  [[Legacy]]\n    type = TCPClientInterface\n    interface_enabled = yes\n    enabled = yes\n    target_host = legacy.example\n    target_port = 4242\n",
        );

        assert!(set_interface_enabled(&dir, "Legacy", false));
        let content = read_config(&dir).unwrap();
        assert!(content.contains("interface_enabled = false"));
        assert!(content.contains("enabled = false"));
    }

    #[test]
    fn set_interface_enabled_inserts_key_when_missing() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[interfaces]\n  [[No Flag]]\n    type = TCPClientInterface\n    target_host = noflag.example\n    target_port = 4242\n",
        );

        assert!(set_interface_enabled(&dir, "No Flag", false));
        let content = read_config(&dir).unwrap();
        assert!(content.contains("type = TCPClientInterface\n    enabled = false\n"));
        assert!(content.contains("target_host = noflag.example"));
    }

    #[test]
    fn update_rnode_replaces_radio_parameters() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Radio",
                port: "/dev/ttyUSB0",
                mode: None,
                frequency: 915_000_000,
                bandwidth: 125_000,
                spreading_factor: 7,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("short_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        assert!(update_rnode_interface(
            &dir,
            "Radio",
            RnodeInterfaceArgs {
                name: "Field Radio",
                port: "/dev/ttyUSB1",
                mode: Some("roaming"),
                frequency: 917_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 6,
                tx_power: 20,
                region_key: Some("americas"),
                preset_key: None,
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 0);
        assert_eq!(count_header(&content, "Field Radio"), 1);
        assert!(content.contains("port = /dev/ttyUSB1"));
        assert!(content.contains("mode = roaming"));
        assert!(content.contains("frequency = 917000000"));
        assert!(content.contains("spreadingfactor = 9"));
        assert!(content.contains("ratspeak_region = americas"));
        assert!(!content.contains("ratspeak_preset = short_fast"));
    }

    #[test]
    fn rnode_interface_writes_custom_433_radio_parameters() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "UHF Radio",
                port: "/dev/ttyUSB0",
                mode: Some("boundary"),
                frequency: 433_000_000,
                bandwidth: 125_000,
                spreading_factor: 10,
                coding_rate: 6,
                tx_power: 17,
                region_key: Some("uhf_433"),
                preset_key: None,
                airtime_limit_short: Some(33.0),
                airtime_limit_long: Some(3.5),
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("mode = boundary"));
        assert!(content.contains("frequency = 433000000"));
        assert!(content.contains("bandwidth = 125000"));
        assert!(content.contains("spreadingfactor = 10"));
        assert!(content.contains("codingrate = 6"));
        assert!(content.contains("txpower = 17"));
        assert!(content.contains("airtime_limit_short = 33"));
        assert!(content.contains("airtime_limit_long = 3.5"));
        assert!(content.contains("ratspeak_region = uhf_433"));
        assert!(!content.contains("ratspeak_preset ="));
    }

    #[test]
    fn rnode_interface_writes_public_map_discovery_fields() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Mapped Radio",
                port: "/dev/ttyUSB0",
                mode: Some("full"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs {
                    discoverable: true,
                    latitude: Some(39.739),
                    longitude: Some(-104.99),
                    discovery_name: Some("Alice"),
                },
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("discoverable = yes"));
        assert!(content.contains("latitude = 39.739"));
        assert!(content.contains("longitude = -104.99"));
        assert!(content.contains("discovery_name = Alice"));
        assert!(!content.contains("height ="));
    }

    #[test]
    fn update_rnode_clears_public_map_fields_when_disabled() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Mapped Radio",
                port: "/dev/ttyUSB0",
                mode: Some("full"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs {
                    discoverable: true,
                    latitude: Some(39.739),
                    longitude: Some(-104.99),
                    discovery_name: Some("Alice"),
                },
            },
        ));

        assert!(update_rnode_interface(
            &dir,
            "Mapped Radio",
            RnodeInterfaceArgs {
                name: "Mapped Radio",
                port: "/dev/ttyUSB0",
                mode: Some("full"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(!content.contains("discoverable ="));
        assert!(!content.contains("latitude ="));
        assert!(!content.contains("longitude ="));
        assert!(!content.contains("discovery_name ="));
    }

    #[test]
    fn rnode_interface_rejects_invalid_public_map_coordinates() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(!add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Bad Map Radio",
                port: "/dev/ttyUSB0",
                mode: Some("full"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs {
                    discoverable: true,
                    latitude: Some(91.0),
                    longitude: Some(-104.99),
                    discovery_name: Some("Alice"),
                },
            },
        ));
        assert!(!read_config(&dir).unwrap().contains("Bad Map Radio"));
    }

    #[test]
    fn rnode_interface_accepts_tcp_port_url() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "TCP Radio",
                port: "tcp://rnode.local:7633",
                mode: Some("gw"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("port = tcp://rnode.local:7633"));
        assert!(content.contains("mode = gateway"));
        assert!(content.contains("type = RNodeInterface"));
        assert!(!content.contains("airtime_limit_short ="));
        assert!(!content.contains("airtime_limit_long ="));
    }

    #[test]
    fn rnode_interface_defaults_to_full_mode() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Default Mode Radio",
                port: "/dev/ttyUSB0",
                mode: None,
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("mode = full"));
    }

    #[test]
    fn rnode_interface_rejects_unsupported_mode() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(!add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "P2P Radio",
                port: "/dev/ttyUSB0",
                mode: Some("point_to_point"),
                frequency: 915_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("medium_fast"),
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));
        assert!(read_config(&dir).unwrap().contains("[interfaces]"));
    }

    #[test]
    fn rnode_mode_helpers_cover_exposed_modes() {
        assert_eq!(normalize_rnode_interface_mode(None), Some("full"));
        assert_eq!(normalize_rnode_interface_mode(Some("full")), Some("full"));
        assert_eq!(
            normalize_rnode_interface_mode(Some("gateway")),
            Some("gateway")
        );
        assert_eq!(normalize_rnode_interface_mode(Some("gw")), Some("gateway"));
        assert_eq!(
            normalize_rnode_interface_mode(Some("Access Point")),
            Some("access_point")
        );
        assert_eq!(
            normalize_rnode_interface_mode(Some("ap")),
            Some("access_point")
        );
        assert_eq!(
            normalize_rnode_interface_mode(Some("boundary")),
            Some("boundary")
        );
        assert_eq!(
            normalize_rnode_interface_mode(Some("roaming")),
            Some("roaming")
        );
        assert_eq!(normalize_rnode_interface_mode(Some("point_to_point")), None);

        for mode in RNODE_INTERFACE_MODES {
            assert!(rnode_interface_mode_value(Some(mode)).is_some());
        }
    }
}
