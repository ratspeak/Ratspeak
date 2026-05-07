//! RNS config file (INI-flavoured) read/write.

use std::path::Path;

use ratspeak_core::config::{
    LEGACY_RNS_INSTANCE_CONTROL_PORT, LEGACY_RNS_SHARED_INSTANCE_PORT,
    RATSPEAK_RNS_INSTANCE_CONTROL_PORT, RATSPEAK_RNS_SHARED_INSTANCE_PORT,
};
use serde_json::{Value, json};

pub fn read_config(config_dir: &Path) -> Option<String> {
    let path = config_dir.join("config");
    std::fs::read_to_string(&path).ok()
}

pub fn write_config(config_dir: &Path, content: &str) -> bool {
    let path = config_dir.join("config");
    let backup = config_dir.join("config.backup");
    if path.exists() {
        std::fs::copy(&path, &backup).ok();
    }
    std::fs::write(&path, content).is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatspeakRnsPortConfigChange {
    Created,
    Updated,
    Unchanged,
}

pub fn ensure_app_private_shared_instance_ports(
    config_dir: &Path,
) -> std::io::Result<RatspeakRnsPortConfigChange> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config");
    if !path.exists() {
        std::fs::write(&path, ratspeak_default_config())?;
        return Ok(RatspeakRnsPortConfigChange::Created);
    }

    let content = std::fs::read_to_string(&path)?;
    let Some(updated) = ratspeak_shared_instance_port_update(&content) else {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    };
    if updated == content {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }

    std::fs::copy(&path, config_dir.join("config.backup"))?;
    std::fs::write(&path, updated)?;
    Ok(RatspeakRnsPortConfigChange::Updated)
}

fn ratspeak_default_config() -> String {
    format!(
        "# This is the default Ratspeak Reticulum config file.\n\n[reticulum]\nenable_transport = False\nshare_instance = Yes\ninstance_name = default\nshared_instance_port = {RATSPEAK_RNS_SHARED_INSTANCE_PORT}\ninstance_control_port = {RATSPEAK_RNS_INSTANCE_CONTROL_PORT}\n\n[logging]\nloglevel = 4\n\n[interfaces]\n\n[[Default Interface]]\ntype = AutoInterface\nenabled = Yes\n"
    )
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

fn ratspeak_shared_instance_port_update(content: &str) -> Option<String> {
    let mut lines = content.lines().map(str::to_string).collect::<Vec<_>>();
    let Some((section_start, section_end)) = reticulum_section_bounds(&lines) else {
        let mut new_lines = vec![
            "[reticulum]".to_string(),
            format!("shared_instance_port = {RATSPEAK_RNS_SHARED_INSTANCE_PORT}"),
            format!("instance_control_port = {RATSPEAK_RNS_INSTANCE_CONTROL_PORT}"),
            String::new(),
        ];
        new_lines.extend(lines);
        return Some(join_config_lines(new_lines));
    };

    let mut shared_line = None;
    let mut control_line = None;
    let mut shared = PortSetting::Missing;
    let mut control = PortSetting::Missing;

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
            _ => {}
        }
    }

    if shared.is_invalid() || control.is_invalid() {
        return None;
    }

    let pair_is_defaultish = (shared.is_missing()
        || shared.is_legacy_value(LEGACY_RNS_SHARED_INSTANCE_PORT))
        && (control.is_missing() || control.is_legacy_value(LEGACY_RNS_INSTANCE_CONTROL_PORT));
    let desired_shared = if pair_is_defaultish || shared.is_missing() {
        Some(RATSPEAK_RNS_SHARED_INSTANCE_PORT)
    } else {
        None
    };
    let desired_control = if pair_is_defaultish || control.is_missing() {
        Some(RATSPEAK_RNS_INSTANCE_CONTROL_PORT)
    } else {
        None
    };

    if desired_shared.is_none() && desired_control.is_none() {
        return None;
    }

    let mut inserted = Vec::new();
    if let Some(port) = desired_shared {
        if let Some(idx) = shared_line {
            lines[idx] = replace_key_line(&lines[idx], "shared_instance_port", port);
        } else {
            inserted.push(format!(
                "shared_instance_port = {RATSPEAK_RNS_SHARED_INSTANCE_PORT}"
            ));
        }
    }
    if let Some(port) = desired_control {
        if let Some(idx) = control_line {
            lines[idx] = replace_key_line(&lines[idx], "instance_control_port", port);
        } else {
            inserted.push(format!(
                "instance_control_port = {RATSPEAK_RNS_INSTANCE_CONTROL_PORT}"
            ));
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

fn replace_key_line(line: &str, key: &str, value: u16) -> String {
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

/// Removes any existing block with the same `name` before insertion.
pub fn add_rnode_interface(config_dir: &Path, args: RnodeInterfaceArgs<'_>) -> bool {
    let block = rnode_interface_block(args);
    upsert_interface_block(config_dir, &[args.name], &block)
}

#[derive(Clone, Copy)]
/// RNode interface settings written into the Ratspeak-owned Reticulum config.
pub struct RnodeInterfaceArgs<'a> {
    pub name: &'a str,
    pub port: &'a str,
    pub frequency: u64,
    pub bandwidth: u64,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    pub tx_power: i8,
    pub region_key: Option<&'a str>,
    pub preset_key: Option<&'a str>,
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
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };

    let name_refs = names.iter().map(String::as_str).collect::<Vec<_>>();
    let (result, _) = remove_interface_blocks_from_content(&content, &name_refs);
    write_config(config_dir, &result)
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
    let content = read_config(config_dir).unwrap_or_default();
    let (content, _) = remove_interface_blocks_from_content(&content, remove_names);
    let new_content = insert_interface_block(&content, block);
    write_config(config_dir, &new_content)
}

fn replace_interface_block(config_dir: &Path, old_name: &str, new_name: &str, block: &str) -> bool {
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
        frequency,
        bandwidth,
        spreading_factor,
        coding_rate,
        tx_power,
        region_key,
        preset_key,
    } = args;

    let mut block = format!(
        "\n  [[{name}]]\n    type = RNodeInterface\n    port = {port}\n    frequency = {frequency}\n    bandwidth = {bandwidth}\n    spreadingfactor = {spreading_factor}\n    codingrate = {coding_rate}\n    txpower = {tx_power}\n    enabled = true\n"
    );
    if let Some(region_key) = region_key {
        block.push_str(&format!("    ratspeak_region = {region_key}\n"));
    }
    if let Some(preset_key) = preset_key {
        block.push_str(&format!("    ratspeak_preset = {preset_key}\n"));
    }
    block
}

fn tcp_client_block(name: &str, host: &str, port: u16) -> String {
    format!(
        "\n  [[{name}]]\n    type = TCPClientInterface\n    target_host = {host}\n    target_port = {port}\n    enabled = true\n"
    )
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
    let block = tcp_client_block(name, host, port);
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn add_tcp_server(config_dir: &Path, name: &str, listen_port: u16, listen_ip: &str) -> bool {
    let block = tcp_server_block(name, listen_port, listen_ip);
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn add_backbone_client(config_dir: &Path, args: BackboneClientArgs<'_>) -> bool {
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
    let block = tcp_client_block(name, host, port);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn update_tcp_server(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
) -> bool {
    let block = tcp_server_block(name, listen_port, listen_ip);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn update_backbone_client(
    config_dir: &Path,
    old_name: &str,
    args: BackboneClientArgs<'_>,
) -> bool {
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
    let block = backbone_server_block(name, listen_port, listen_ip, prefer_ipv6, device);
    replace_interface_block(config_dir, old_name, name, &block)
}

pub fn set_transport_mode(config_dir: &Path, enable: bool) -> bool {
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };

    let val = if enable { "True" } else { "False" };
    let mut found = false;
    let result: String = content
        .lines()
        .map(|line| {
            let trimmed = line.trim().to_lowercase();
            if trimmed.starts_with("enable_transport") {
                found = true;
                format!("  enable_transport = {val}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    if found {
        write_config(config_dir, &result)
    } else {
        let mut content = content;
        content.push_str(&format!("\n  enable_transport = {val}\n"));
        write_config(config_dir, &content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(0);

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
    fn ensure_app_private_ports_creates_ratspeak_config() {
        let dir = temp_config_dir();

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Created);
        let content = read_config(&dir).unwrap();
        assert!(content.contains("shared_instance_port = 37430"));
        assert!(content.contains("instance_control_port = 37431"));
        assert!(content.contains("[[Default Interface]]"));
        assert!(!dir.join("config.backup").exists());
    }

    #[test]
    fn ensure_app_private_ports_migrates_legacy_defaults_with_backup() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nshare_instance = Yes\nshared_instance_port = 37428\ninstance_control_port = 37429\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

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

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

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

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

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

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

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
    fn update_rnode_replaces_radio_parameters() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Radio",
                port: "/dev/ttyUSB0",
                frequency: 915_000_000,
                bandwidth: 125_000,
                spreading_factor: 7,
                coding_rate: 5,
                tx_power: 17,
                region_key: Some("americas"),
                preset_key: Some("short_fast"),
            },
        ));

        assert!(update_rnode_interface(
            &dir,
            "Radio",
            RnodeInterfaceArgs {
                name: "Field Radio",
                port: "/dev/ttyUSB1",
                frequency: 917_000_000,
                bandwidth: 250_000,
                spreading_factor: 9,
                coding_rate: 6,
                tx_power: 20,
                region_key: Some("americas"),
                preset_key: None,
            },
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 0);
        assert_eq!(count_header(&content, "Field Radio"), 1);
        assert!(content.contains("port = /dev/ttyUSB1"));
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
                frequency: 433_000_000,
                bandwidth: 125_000,
                spreading_factor: 10,
                coding_rate: 6,
                tx_power: 17,
                region_key: Some("uhf_433"),
                preset_key: None,
            },
        ));

        let content = read_config(&dir).unwrap();
        assert!(content.contains("frequency = 433000000"));
        assert!(content.contains("bandwidth = 125000"));
        assert!(content.contains("spreadingfactor = 10"));
        assert!(content.contains("codingrate = 6"));
        assert!(content.contains("txpower = 17"));
        assert!(content.contains("ratspeak_region = uhf_433"));
        assert!(!content.contains("ratspeak_preset ="));
    }
}
