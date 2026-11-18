//! RNS config file (INI-flavoured) read/write.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::Zeroizing;

use ratspeak_core::config::{
    LEGACY_RNS_INSTANCE_CONTROL_PORT, LEGACY_RNS_SHARED_INSTANCE_PORT,
    RATSPEAK_RNS_INSTANCE_CONTROL_PORT, RATSPEAK_RNS_SHARED_INSTANCE_PORT,
};
use serde_json::{Value, json};

/// Ratspeak's product default for newly added mobile radio interfaces.
pub const RNODE_NEW_INTERFACE_DEFAULT_MODE: &str = "roaming";
/// Reticulum's default when an existing config omits an interface mode.
pub const RETICULUM_DEFAULT_INTERFACE_MODE: &str = "full";
pub const RNODE_INTERFACE_MODES: &[&str] =
    &["full", "gateway", "access_point", "boundary", "roaming"];

pub fn read_config(config_dir: &Path) -> Option<Zeroizing<String>> {
    let path = config_dir.join("config");
    read_config_text(&path).ok()
}

fn read_config_bytes(path: impl AsRef<Path>) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0u8; 8192]);
    loop {
        let count = match file.read(chunk.as_mut()) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        let required = bytes.len() + count;
        if required > bytes.capacity() {
            let mut grown = Zeroizing::new(Vec::with_capacity(
                required.max(bytes.capacity().saturating_mul(2)),
            ));
            grown.extend_from_slice(&bytes);
            bytes = grown;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}

fn read_config_text(path: impl AsRef<Path>) -> std::io::Result<Zeroizing<String>> {
    let bytes = read_config_bytes(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(Zeroizing::new(text.to_owned()))
}

// String growth otherwise releases the old allocation without wiping it.
fn reserve_config_space(content: &mut Zeroizing<String>, additional: usize) {
    let required = content.len() + additional;
    if required > content.capacity() {
        let mut grown = Zeroizing::new(String::with_capacity(
            required.max(content.capacity().saturating_mul(2)),
        ));
        grown.push_str(content);
        *content = grown;
    }
}

/// Validate before publication; preserve the prior file in a private, unique backup.
/// The shared OS lock also serializes app writes with the offline CLI migrator.
pub fn write_config_result(config_dir: &Path, content: &str) -> std::io::Result<()> {
    let path = config_dir.join("config");
    let previous = match read_config_text(&path) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    write_config_if_unchanged(config_dir, previous.as_deref().map(String::as_str), content)
}

fn write_config_if_unchanged(
    config_dir: &Path,
    expected: Option<&str>,
    content: &str,
) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config");
    let _lock = rns_runtime::config::ifac_lock::ConfigWriteLock::acquire(&path)?;
    let previous = match read_config_text(&path) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if previous.as_deref().map(String::as_str) != expected {
        return Err(std::io::Error::other(
            "Settings changed; reload before saving.",
        ));
    }
    publish_config_locked(config_dir, previous.as_deref().map(String::as_str), content)
}

fn publish_config_locked(
    config_dir: &Path,
    previous: Option<&str>,
    content: &str,
) -> std::io::Result<()> {
    rns_runtime::config::ifac::validate_ifac_update(previous, content)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    if previous == Some(content) {
        return Ok(());
    }
    if let Some(previous) = previous {
        let backup = config_dir.join("config.backup");
        rns_identity::persistence::durable_write(&backup, previous.as_bytes())
            .map_err(std::io::Error::other)?;
    }
    rns_identity::persistence::durable_write(&config_dir.join("config"), content.as_bytes())
        .map_err(std::io::Error::other)?;
    Ok(())
}

pub fn write_config(config_dir: &Path, content: &str) -> bool {
    write_config_result(config_dir, content).is_ok()
}

/// Apply an explicit unit decision only to the source the user inspected.
pub fn migrate_ifac_units(
    config_dir: &Path,
    interface: &str,
    units: rns_runtime::config::ifac::IfacSourceUnits,
    expected_source_hash: &[u8; 32],
) -> std::io::Result<bool> {
    let path = config_dir.join("config");
    let _lock = rns_runtime::config::ifac_lock::ConfigWriteLock::acquire(&path)?;
    let source = read_config_text(&path)?;
    if rns_crypto::sha::sha256(source.as_bytes()) != *expected_source_hash {
        return Err(std::io::Error::other(
            "Settings changed. Reload the interface and choose again.",
        ));
    }
    let plan =
        rns_runtime::config::ifac::plan_ifac_unit_migration(&source, &[interface.into()], units)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    if plan.is_unchanged() {
        return Ok(false);
    }
    // Detect an editor that does not participate in our OS lock before replacing.
    if read_config_bytes(&path)?.as_slice() != source.as_bytes() {
        return Err(std::io::Error::other(
            "Settings changed. Reload the interface and choose again.",
        ));
    }
    let backup = config_dir.join(format!("config.backup.ifac.{}", uuid::Uuid::new_v4()));
    rns_identity::persistence::durable_write(&backup, source.as_bytes())
        .map_err(std::io::Error::other)?;
    publish_config_locked(config_dir, Some(&source), plan.proposed_config())?;
    Ok(true)
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
        write_config_if_unchanged(config_dir, None, &ratspeak_default_config(config_dir))?;
        return Ok(RatspeakRnsPortConfigChange::Created);
    }

    let content = read_config_text(&path)?;
    if has_operator_shared_selector(&content) {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }
    let Some(updated) = ratspeak_shared_instance_port_update(&content) else {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    };
    if updated == content {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }

    write_config_if_unchanged(config_dir, Some(&content), &updated)?;
    Ok(RatspeakRnsPortConfigChange::Updated)
}

/// Prepare the complete app-managed shared-instance selector in one durable
/// config rewrite. The public ports-only helper above remains behaviorally
/// narrow for existing callers; runtime startup additionally migrates the
/// legacy Linux/Android `instance_name = default` selector.
pub(crate) fn ensure_app_private_shared_instance(
    config_dir: &Path,
) -> std::io::Result<RatspeakRnsPortConfigChange> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join("config");
    if !path.exists() {
        write_config_if_unchanged(config_dir, None, &ratspeak_default_config(config_dir))?;
        return Ok(RatspeakRnsPortConfigChange::Created);
    }

    let content = read_config_text(&path)?;
    if has_operator_shared_selector(&content) {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }
    let port_updated = ratspeak_shared_instance_port_update(&content)
        .filter(|updated| updated != &content)
        .unwrap_or_else(|| content.clone());
    let updated = ratspeak_private_instance_name_update(&port_updated, config_dir)
        .filter(|updated| updated != &port_updated)
        .unwrap_or(port_updated);
    if updated == content {
        return Ok(RatspeakRnsPortConfigChange::Unchanged);
    }

    write_config_if_unchanged(config_dir, Some(&content), &updated)?;
    Ok(RatspeakRnsPortConfigChange::Updated)
}

fn ratspeak_default_config(config_dir: &Path) -> Zeroizing<String> {
    let instance_name = ratspeak_private_instance_name(config_dir);
    Zeroizing::new(format!(
        "# This is the default Ratspeak Reticulum config file.\n\n[reticulum]\nenable_transport = False\nshare_instance = No\ninstance_name = {instance_name}\nshared_instance_port = {RATSPEAK_RNS_SHARED_INSTANCE_PORT}\ninstance_control_port = {RATSPEAK_RNS_INSTANCE_CONTROL_PORT}\n\n[logging]\nloglevel = 4\n\n[interfaces]\n"
    ))
}

fn ratspeak_private_instance_name(config_dir: &Path) -> String {
    let stable_path =
        std::fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let digest = rns_crypto::sha::sha256(stable_path.as_os_str().as_encoded_bytes());
    format!("ratspeak-{}", hex::encode(&digest[..8]))
}

/// Determine operator intent before any port/name rewrite. An explicit key or
/// carrier can point at upstream's canonical endpoints even in a private file.
pub(crate) fn has_operator_shared_selector(content: &str) -> bool {
    if rns_runtime::config::Config::parse(content).is_err() {
        return true;
    }
    let lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let Some((start, end)) = reticulum_section_bounds(&lines) else {
        return false;
    };
    for line in &lines[start + 1..end] {
        let Some((key, value)) = parse_ini_key_value(line) else {
            continue;
        };
        match key.as_str() {
            "rpc_key" | "shared_instance_type" => return true,
            "instance_name"
                if value.as_str() != "default"
                    && !value.strip_prefix("ratspeak-").is_some_and(|s| {
                        s.len() == 16 && s.bytes().all(|c| c.is_ascii_hexdigit())
                    }) =>
            {
                return true;
            }
            "shared_instance_port"
                if !matches!(
                    value.parse::<u16>(),
                    Ok(LEGACY_RNS_SHARED_INSTANCE_PORT | RATSPEAK_RNS_SHARED_INSTANCE_PORT)
                ) =>
            {
                return true;
            }
            "instance_control_port"
                if !matches!(
                    value.parse::<u16>(),
                    Ok(LEGACY_RNS_INSTANCE_CONTROL_PORT | RATSPEAK_RNS_INSTANCE_CONTROL_PORT)
                ) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

pub fn strip_legacy_default_auto_interface(content: &str) -> Zeroizing<String> {
    if !content.contains("[[Default Interface]]") {
        return Zeroizing::new(content.to_string());
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

    let mut result = join_config_text(out, false);
    if had_trailing_newline && !result.ends_with('\n') {
        reserve_config_space(&mut result, 1);
        result.push('\n');
    }
    result
}

fn interface_block_name(line: &str) -> Option<&str> {
    strip_inline_comment(line)
        .trim()
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .map(str::trim)
        .map(unquote_ini_scalar)
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

    fn is_app_managed(self, legacy: u16, current: u16) -> bool {
        matches!(self, Self::Missing)
            || matches!(self, Self::Value(port) if port == legacy || port == current)
    }
}

fn ratspeak_private_instance_name_update(
    content: &str,
    config_dir: &Path,
) -> Option<Zeroizing<String>> {
    let mut lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let (section_start, section_end) = reticulum_section_bounds(&lines)?;

    let mut instance_name_line = None;
    let mut instance_name = None;
    let mut shared = PortSetting::Missing;
    let mut shared_seen = false;
    let mut control = PortSetting::Missing;
    let mut control_seen = false;
    let mut share_instance = None;
    let mut explicit_shared_instance_type = false;
    let mut explicit_rpc_key = false;

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
            "instance_name" => {
                if instance_name_line.replace(idx).is_some() {
                    return None;
                }
                instance_name = Some(value);
            }
            "shared_instance_port" => {
                if shared_seen {
                    return None;
                }
                shared_seen = true;
                shared = value
                    .parse::<u16>()
                    .map(PortSetting::Value)
                    .unwrap_or(PortSetting::Invalid);
            }
            "instance_control_port" => {
                if control_seen {
                    return None;
                }
                control_seen = true;
                control = value
                    .parse::<u16>()
                    .map(PortSetting::Value)
                    .unwrap_or(PortSetting::Invalid);
            }
            "share_instance" => {
                if share_instance.is_some() {
                    return None;
                }
                share_instance = Some(match value.trim().to_ascii_lowercase().as_str() {
                    "yes" | "true" | "1" | "on" => true,
                    "no" | "false" | "0" | "off" => false,
                    _ => return None,
                });
            }
            "shared_instance_type" => explicit_shared_instance_type = true,
            "rpc_key" => explicit_rpc_key = true,
            _ => {}
        }
    }

    // A configured carrier, key, custom port pair, disabled sharing, or custom
    // name is an explicit external/custom ownership choice. Only migrate the
    // app-generated/default-like selector that previously collided with rnsd.
    if explicit_shared_instance_type
        || explicit_rpc_key
        || share_instance == Some(false)
        || !shared.is_app_managed(
            LEGACY_RNS_SHARED_INSTANCE_PORT,
            RATSPEAK_RNS_SHARED_INSTANCE_PORT,
        )
        || !control.is_app_managed(
            LEGACY_RNS_INSTANCE_CONTROL_PORT,
            RATSPEAK_RNS_INSTANCE_CONTROL_PORT,
        )
    {
        return None;
    }

    match instance_name.as_deref() {
        Some(name) if !name.eq_ignore_ascii_case("default") => return None,
        Some(_) => {
            let idx = instance_name_line?;
            lines[idx] = replace_key_line(
                &lines[idx],
                "instance_name",
                ratspeak_private_instance_name(config_dir),
            );
        }
        None => {
            lines.splice(
                section_end..section_end,
                [Zeroizing::new(format!(
                    "instance_name = {}",
                    ratspeak_private_instance_name(config_dir)
                ))],
            );
        }
    }

    Some(join_config_lines(lines))
}

fn ratspeak_shared_instance_port_update(content: &str) -> Option<Zeroizing<String>> {
    let mut lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let Some((section_start, section_end)) = reticulum_section_bounds(&lines) else {
        let mut new_lines = vec![
            "[reticulum]".to_string(),
            format!("shared_instance_port = {RATSPEAK_RNS_SHARED_INSTANCE_PORT}"),
            format!("instance_control_port = {RATSPEAK_RNS_INSTANCE_CONTROL_PORT}"),
            String::new(),
        ]
        .into_iter()
        .map(Zeroizing::new)
        .collect::<Vec<_>>();
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
        lines.splice(
            section_end..section_end,
            inserted.into_iter().map(Zeroizing::new),
        );
    }

    Some(join_config_lines(lines))
}

fn reticulum_section_bounds(lines: &[Zeroizing<String>]) -> Option<(usize, usize)> {
    section_bounds(lines, "reticulum")
}

fn section_bounds(lines: &[Zeroizing<String>], name: &str) -> Option<(usize, usize)> {
    let start = lines
        .iter()
        .position(|line| named_top_level_section(line, name))?;
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

fn parse_ini_key_value(line: &str) -> Option<(String, Zeroizing<String>)> {
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
        unquote_ini_scalar(key).to_ascii_lowercase(),
        Zeroizing::new(unquote_ini_scalar(strip_inline_comment(value).trim()).to_string()),
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

fn replace_key_line(line: &str, key: &str, value: impl std::fmt::Display) -> Zeroizing<String> {
    let indent = line
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>();
    Zeroizing::new(format!("{indent}{key} = {value}"))
}

fn join_config_text<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    trailing_newline: bool,
) -> Zeroizing<String> {
    let lines = lines.into_iter().collect::<Vec<_>>();
    let capacity = lines.iter().map(|line| line.len()).sum::<usize>()
        + lines.len().saturating_sub(1)
        + usize::from(trailing_newline);
    let mut content = Zeroizing::new(String::with_capacity(capacity));
    for (index, line) in lines.into_iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(line);
    }
    if trailing_newline {
        content.push('\n');
    }
    content
}

fn join_config_lines(lines: Vec<Zeroizing<String>>) -> Zeroizing<String> {
    join_config_text(lines.iter().map(|line| line.as_str()), true)
}

/// Enabled RNode entries matching `prefix`, in configuration order.
/// Android uses this to enforce its single native Bluetooth-radio owner.
pub fn enabled_rnode_names_with_port_prefix(config_dir: &Path, prefix: &str) -> Vec<String> {
    get_all_interfaces(config_dir)
        .get("rnode")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .get("port")
                .and_then(Value::as_str)
                .is_some_and(|port| port.starts_with(prefix))
                && interface_entry_enabled(entry)
        })
        .filter_map(|entry| {
            entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// Whether an interface from [`get_all_interfaces`] starts.
pub fn interface_entry_enabled(entry: &Value) -> bool {
    entry.get("enabled").and_then(Value::as_bool) == Some(true)
}

pub fn get_all_interfaces(config_dir: &Path) -> Value {
    use rns_runtime::config::ifac::{IfacConfigError, parse_ifac_size};
    use rns_runtime::config::{Config, ConfigValue};
    let mut result = json!({ "rnode": [], "auto": [], "tcp_client": [], "tcp_server": [],
        "backbone_client": [], "backbone_server": [] });
    let Some(content) = read_config(config_dir) else {
        return result;
    };
    let Ok(config) = Config::parse(&content) else {
        result["config_error"] =
            json!("Configuration syntax is invalid. Correct the file before editing interfaces.");
        return result;
    };
    let source_hash = hex::encode(rns_crypto::sha::sha256(content.as_bytes()));
    for (name, section) in config.subsections("interfaces") {
        let kind = section
            .get("type")
            .or_else(|| section.get("interface_type"))
            .unwrap_or("");
        let group = match kind {
            "RNodeInterface" => "rnode",
            "AutoInterface" => "auto",
            "TCPClientInterface" => "tcp_client",
            "TCPServerInterface" => "tcp_server",
            "BackboneInterface" if section.has("target_host") => "backbone_client",
            "BackboneInterface" => "backbone_server",
            _ => continue,
        };
        let props = section
            .values
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    match value {
                        ConfigValue::Scalar(value) => Zeroizing::new(value.clone()),
                        ConfigValue::List(values) => Zeroizing::new(values.join(",")),
                    },
                )
            })
            .collect();
        let mut entry = build_interface_entry(name, kind, &props);
        let object = entry.as_object_mut().expect("interface object");
        for key in rns_runtime::config::ifac::SIZE_KEYS {
            object.remove(*key);
        }
        // Either key set to yes enables an interface; with neither it is on,
        // the client default that startup writes out for the runtime.
        object.remove("interface_enabled");
        object.insert(
            "enabled".into(),
            json!(!section.has_enable_key() || section.interface_enabled()),
        );
        match parse_ifac_size(section) {
            Ok(Some(bytes)) => {
                object.insert("ifac_size".into(), json!(bytes.to_string()));
            }
            Ok(None) => {}
            Err(error) => {
                object.insert("ifac_error".into(), json!(error.to_string()));
                if error == IfacConfigError::IfacUnitsRequired {
                    let raw = section
                        .get_uint("ifac_size")
                        .expect("validated ambiguous integer");
                    object.insert(
                        "ifac_migration".into(),
                        json!({
                            "source_hash": source_hash, "bytes": raw,
                            "python_bytes": if raw % 8 == 0 { Some(raw / 8) } else { None },
                        }),
                    );
                }
            }
        }
        result[group].as_array_mut().unwrap().push(entry);
    }
    result
}

#[derive(Clone, Debug)]
pub struct AndroidUsbSelectorMigrationCandidate {
    pub name: String,
    pub device_name: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    revision: InterfaceBlockRevision,
}

/// Snapshot enabled Android USB blocks while the caller owns the app config
/// lock. Native device resolution then occurs without holding that lock.
pub fn android_usb_selector_migration_candidates(
    config_dir: &Path,
) -> Vec<AndroidUsbSelectorMigrationCandidate> {
    let mut candidates = Vec::new();
    let entries = get_all_interfaces(config_dir)
        .get("rnode")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in entries {
        if !interface_entry_enabled(&entry) {
            continue;
        }
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(device_name) = entry
            .get("port")
            .and_then(Value::as_str)
            .and_then(|port| port.strip_prefix("androidusb://"))
            .filter(|device_name| !device_name.is_empty())
        else {
            continue;
        };
        let parsed_u16 = |key: &str| {
            entry
                .get(key)
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                })
                .and_then(|value| u16::try_from(value).ok())
        };
        let current_vendor = parsed_u16("usb_vendor_id");
        let current_product = parsed_u16("usb_product_id");
        let current_serial = entry
            .get("usb_serial_number")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let Ok(revision) = snapshot_interface_block(config_dir, name) else {
            continue;
        };
        candidates.push(AndroidUsbSelectorMigrationCandidate {
            name: name.to_string(),
            device_name: device_name.to_string(),
            vendor_id: current_vendor,
            product_id: current_product,
            serial_number: current_serial,
            revision,
        });
    }
    candidates
}

/// Persist a native-resolved selector only if the snapshotted block is still
/// exact. Callers must hold the app config lock around this CAS.
pub fn apply_android_usb_selector_migration(
    config_dir: &Path,
    candidate: &AndroidUsbSelectorMigrationCandidate,
    vendor_id: u16,
    product_id: u16,
    serial_number: Option<&str>,
) -> InterfaceBlockCasOutcome {
    if candidate.vendor_id == Some(vendor_id)
        && candidate.product_id == Some(product_id)
        && candidate.serial_number.as_deref() == serial_number
    {
        return InterfaceBlockCasOutcome::Applied;
    }
    set_rnode_usb_selector_if_revision(
        config_dir,
        &candidate.revision,
        vendor_id,
        product_id,
        serial_number,
    )
}

fn parse_key_value(line: &str) -> Option<(String, Zeroizing<String>)> {
    let parts: Vec<&str> = line.splitn(2, '=').collect();
    if parts.len() == 2 {
        let key = parts[0].trim().to_lowercase();
        let value = Zeroizing::new(parts[1].trim().to_string());
        Some((key, value))
    } else {
        None
    }
}

fn build_interface_entry(
    name: &str,
    itype: &str,
    props: &std::collections::HashMap<String, Zeroizing<String>>,
) -> Value {
    let mut entry = json!({
        "name": name,
        "type": itype,
    });
    if let Some(obj) = entry.as_object_mut() {
        for (k, v) in props {
            obj.insert(k.clone(), json!(v.as_str()));
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
        && safe_optional_scalar(args.mode)
        && safe_optional_scalar(args.region_key)
        && safe_optional_scalar(args.preset_key)
        && safe_public_map_args(args.public_map)
}

pub fn normalize_rnode_interface_mode(mode: Option<&str>) -> Option<&'static str> {
    let mode = mode
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .unwrap_or(RNODE_NEW_INTERFACE_DEFAULT_MODE);
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

/// Known modes normalize; unknown non-empty modes pass through verbatim so
/// hand-edited values (e.g. `mode = internal`) survive UI-driven updates.
pub fn rnode_interface_mode_passthrough(mode: Option<&str>) -> &str {
    let mode = mode.map(str::trim).filter(|mode| !mode.is_empty());
    normalize_rnode_interface_mode(mode)
        .unwrap_or_else(|| mode.unwrap_or(RNODE_NEW_INTERFACE_DEFAULT_MODE))
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

#[derive(Clone, Copy, Default)]
pub struct InterfaceIfacArgs<'a> {
    pub network_name: Option<&'a str>,
    pub passphrase: Option<&'a str>,
    pub ifac_size: Option<usize>,
}

impl std::fmt::Debug for InterfaceIfacArgs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceIfacArgs")
            .field("network_name", &self.network_name)
            .field("passphrase", &self.passphrase.map(|_| "[REDACTED]"))
            .field("ifac_size", &self.ifac_size)
            .finish()
    }
}

fn safe_ifac_args(args: InterfaceIfacArgs<'_>) -> bool {
    safe_optional_scalar(args.network_name)
        && safe_optional_scalar(args.passphrase)
        && args.ifac_size.is_none_or(|size| (1..=64).contains(&size))
}

/// Interface discovery for a TCP or Backbone server. Each key is written only
/// when on or set; Reticulum reads the others only while `discoverable`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostDiscoveryArgs<'a> {
    pub discoverable: bool,
    /// The address others connect to; a server is announced only with one.
    pub reachable_on: Option<&'a str>,
    /// Announce the IFAC network name and passphrase.
    pub publish_ifac: bool,
    /// Encrypt the announce for the `network_identity`.
    pub discovery_encrypt: bool,
}

/// Removes any existing block with the same `name` before insertion.
pub fn add_rnode_interface(config_dir: &Path, args: RnodeInterfaceArgs<'_>) -> bool {
    add_rnode_interface_with_usb_selector(config_dir, args, None)
}

pub fn add_rnode_interface_with_usb_selector(
    config_dir: &Path,
    args: RnodeInterfaceArgs<'_>,
    usb_selector: Option<RnodeUsbSelectorArgs<'_>>,
) -> bool {
    if normalize_rnode_interface_mode(args.mode).is_none() || !safe_rnode_args(args) {
        return false;
    }
    if usb_selector.is_some_and(|selector| {
        !args.port.starts_with("androidusb://") || !safe_optional_scalar(selector.serial_number)
    }) {
        return false;
    }
    let mut block = rnode_interface_block(args);
    if let Some(selector) = usb_selector {
        block = interface_block_with_usb_selector(
            &block,
            selector.vendor_id,
            selector.product_id,
            selector.serial_number,
        );
    }
    upsert_interface_block(config_dir, &[args.name], &block)
}

#[derive(Clone, Copy)]
pub struct RnodeUsbSelectorArgs<'a> {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<&'a str>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveInterfaceOutcome {
    Removed,
    NotFound,
    WriteFailed,
}

/// Removes one interface without rewriting unchanged config and reports
/// whether the requested block actually existed.
pub fn remove_interface_checked(config_dir: &Path, name: &str) -> RemoveInterfaceOutcome {
    if !safe_interface_name(name) {
        return RemoveInterfaceOutcome::WriteFailed;
    }

    let content = match read_config_text(config_dir.join("config")) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match std::fs::metadata(config_dir) {
                Ok(metadata) if !metadata.is_dir() => RemoveInterfaceOutcome::WriteFailed,
                Ok(_) => RemoveInterfaceOutcome::NotFound,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    RemoveInterfaceOutcome::NotFound
                }
                Err(_) => RemoveInterfaceOutcome::WriteFailed,
            };
        }
        Err(_) => return RemoveInterfaceOutcome::WriteFailed,
    };
    let (result, removed) = remove_interface_blocks_from_content(&content, &[name]);
    if !removed {
        return RemoveInterfaceOutcome::NotFound;
    }

    match write_config_if_unchanged(config_dir, Some(&content), &result) {
        Ok(()) => RemoveInterfaceOutcome::Removed,
        Err(_) => RemoveInterfaceOutcome::WriteFailed,
    }
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
    write_config_if_unchanged(config_dir, Some(&content), &result).is_ok()
}

/// Opaque, exact snapshot of one interface block in one config file.
///
/// The raw block is deliberately private: callers may retain a revision for
/// compare-and-swap rollback, but cannot synthesize or mutate one. Revisions
/// are bound to the config path they were read from so identity/config
/// switches cannot accidentally restore a block into another config.
#[derive(Clone, PartialEq, Eq)]
pub struct InterfaceBlockRevision {
    config_path: PathBuf,
    name: String,
    block: Zeroizing<String>,
}

impl std::fmt::Debug for InterfaceBlockRevision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceBlockRevision")
            .field("name", &self.name)
            .field("block_bytes", &self.block.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceBlockSnapshotError {
    NotFound,
    Ambiguous,
    ReadFailed,
}

/// Result of a conditional interface-block mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceBlockCasOutcome {
    Applied,
    Stale,
    NotFound,
    WriteFailed,
}

/// Snapshot exactly one named interface block without rewriting the config.
///
/// Duplicate same-name blocks are rejected as ambiguous rather than choosing
/// one implicitly. The snapshot includes the block's comments and formatting
/// through the next config header.
pub fn snapshot_interface_block(
    config_dir: &Path,
    name: &str,
) -> Result<InterfaceBlockRevision, InterfaceBlockSnapshotError> {
    if !safe_interface_name(name) {
        return Err(InterfaceBlockSnapshotError::NotFound);
    }

    let config_path = config_dir.join("config");
    let content = match read_config_text(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(InterfaceBlockSnapshotError::NotFound);
        }
        Err(_) => return Err(InterfaceBlockSnapshotError::ReadFailed),
    };
    let range = match unique_interface_block_range(&content, name) {
        Ok(Some(range)) => range,
        Ok(None) => return Err(InterfaceBlockSnapshotError::NotFound),
        Err(()) => return Err(InterfaceBlockSnapshotError::Ambiguous),
    };

    Ok(InterfaceBlockRevision {
        config_path,
        name: name.to_string(),
        block: Zeroizing::new(content[range.start..range.end].to_string()),
    })
}

/// Restore `previous` only while the named current block is byte-for-byte the
/// supplied `expected_current` revision.
///
/// Edits outside the target block are preserved. For a rename rollback, an
/// independently-created block using the old name is a collision and makes
/// the operation stale instead of overwriting that newer block.
pub fn restore_interface_block_if_revision(
    config_dir: &Path,
    expected_current: &InterfaceBlockRevision,
    previous: &InterfaceBlockRevision,
) -> InterfaceBlockCasOutcome {
    let config_path = config_dir.join("config");
    if expected_current.config_path != config_path || previous.config_path != config_path {
        return InterfaceBlockCasOutcome::Stale;
    }

    let content = match read_config_text(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InterfaceBlockCasOutcome::NotFound;
        }
        Err(_) => return InterfaceBlockCasOutcome::WriteFailed,
    };
    let current = match unique_interface_block_range(&content, &expected_current.name) {
        Ok(Some(range)) => range,
        Ok(None) => return InterfaceBlockCasOutcome::NotFound,
        Err(()) => return InterfaceBlockCasOutcome::Stale,
    };
    if &content[current.start..current.end] != expected_current.block.as_str() {
        return InterfaceBlockCasOutcome::Stale;
    }

    if previous.name != expected_current.name {
        match unique_interface_block_range(&content, &previous.name) {
            Ok(None) => {}
            Ok(Some(_)) | Err(()) => return InterfaceBlockCasOutcome::Stale,
        }
    }

    let restored = replace_interface_block_range(&content, &current, &previous.block);
    match write_config_if_unchanged(config_dir, Some(&content), &restored) {
        Ok(()) => InterfaceBlockCasOutcome::Applied,
        Err(_) => InterfaceBlockCasOutcome::WriteFailed,
    }
}

/// Remove one interface only while its exact block still matches
/// `expected_current`.
///
/// Edits outside the target block are preserved. A missing block reports
/// `NotFound`; a duplicate or changed same-name block reports `Stale`.
pub fn remove_interface_block_if_revision(
    config_dir: &Path,
    expected_current: &InterfaceBlockRevision,
) -> InterfaceBlockCasOutcome {
    let config_path = config_dir.join("config");
    if expected_current.config_path != config_path {
        return InterfaceBlockCasOutcome::Stale;
    }

    let content = match read_config_text(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InterfaceBlockCasOutcome::NotFound;
        }
        Err(_) => return InterfaceBlockCasOutcome::WriteFailed,
    };
    let current = match unique_interface_block_range(&content, &expected_current.name) {
        Ok(Some(range)) => range,
        Ok(None) => return InterfaceBlockCasOutcome::NotFound,
        Err(()) => return InterfaceBlockCasOutcome::Stale,
    };
    if &content[current.start..current.end] != expected_current.block.as_str() {
        return InterfaceBlockCasOutcome::Stale;
    }

    let updated = replace_interface_block_range(&content, &current, "");
    match write_config_if_unchanged(config_dir, Some(&content), &updated) {
        Ok(()) => InterfaceBlockCasOutcome::Applied,
        Err(_) => InterfaceBlockCasOutcome::WriteFailed,
    }
}

/// Change an interface's enabled flag only while its exact block still
/// matches `expected_current`.
pub fn set_interface_enabled_if_revision(
    config_dir: &Path,
    expected_current: &InterfaceBlockRevision,
    enabled: bool,
) -> InterfaceBlockCasOutcome {
    let config_path = config_dir.join("config");
    if expected_current.config_path != config_path {
        return InterfaceBlockCasOutcome::Stale;
    }

    let content = match read_config_text(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InterfaceBlockCasOutcome::NotFound;
        }
        Err(_) => return InterfaceBlockCasOutcome::WriteFailed,
    };
    let current = match unique_interface_block_range(&content, &expected_current.name) {
        Ok(Some(range)) => range,
        Ok(None) => return InterfaceBlockCasOutcome::NotFound,
        Err(()) => return InterfaceBlockCasOutcome::Stale,
    };
    if &content[current.start..current.end] != expected_current.block.as_str() {
        return InterfaceBlockCasOutcome::Stale;
    }

    let updated_block = interface_block_with_enabled(&expected_current.block, enabled);
    if updated_block == expected_current.block {
        return InterfaceBlockCasOutcome::Applied;
    }
    let updated = replace_interface_block_range(&content, &current, &updated_block);
    match write_config_if_unchanged(config_dir, Some(&content), &updated) {
        Ok(()) => InterfaceBlockCasOutcome::Applied,
        Err(_) => InterfaceBlockCasOutcome::WriteFailed,
    }
}

/// Persist a stable Android USB selector only while the exact RNode block is
/// unchanged. The transient device path remains in `port` solely as a legacy
/// migration hint; VID/PID and optional serial are restart-grade identity.
pub fn set_rnode_usb_selector_if_revision(
    config_dir: &Path,
    expected_current: &InterfaceBlockRevision,
    vendor_id: u16,
    product_id: u16,
    serial_number: Option<&str>,
) -> InterfaceBlockCasOutcome {
    if !safe_optional_scalar(serial_number) {
        return InterfaceBlockCasOutcome::WriteFailed;
    }
    let config_path = config_dir.join("config");
    if expected_current.config_path != config_path {
        return InterfaceBlockCasOutcome::Stale;
    }
    let content = match read_config_text(&config_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return InterfaceBlockCasOutcome::NotFound;
        }
        Err(_) => return InterfaceBlockCasOutcome::WriteFailed,
    };
    let current = match unique_interface_block_range(&content, &expected_current.name) {
        Ok(Some(range)) => range,
        Ok(None) => return InterfaceBlockCasOutcome::NotFound,
        Err(()) => return InterfaceBlockCasOutcome::Stale,
    };
    if &content[current.start..current.end] != expected_current.block.as_str() {
        return InterfaceBlockCasOutcome::Stale;
    }
    let updated_block = interface_block_with_usb_selector(
        &expected_current.block,
        vendor_id,
        product_id,
        serial_number,
    );
    let updated = replace_interface_block_range(&content, &current, &updated_block);
    match write_config_if_unchanged(config_dir, Some(&content), &updated) {
        Ok(()) => InterfaceBlockCasOutcome::Applied,
        Err(_) => InterfaceBlockCasOutcome::WriteFailed,
    }
}

#[derive(Clone, Debug)]
struct InterfaceBlockRange {
    start: usize,
    end: usize,
}

fn unique_interface_block_range(
    content: &str,
    name: &str,
) -> Result<Option<InterfaceBlockRange>, ()> {
    rns_runtime::config::Config::parse(content).map_err(|_| ())?;
    let mut matching = interface_block_ranges(content)
        .into_iter()
        .filter(|(block_name, _)| block_name == name)
        .map(|(_, range)| range);
    let first = matching.next();
    if matching.next().is_some() {
        Err(())
    } else {
        Ok(first)
    }
}

fn interface_block_ranges(content: &str) -> Vec<(String, InterfaceBlockRange)> {
    rns_runtime::config::ifac::section_spans(content)
        .unwrap_or_default()
        .into_iter()
        .filter(|span| span.section == "interfaces" && span.path.len() == 1)
        .map(|span| {
            (
                span.path[0].clone(),
                InterfaceBlockRange {
                    start: span.range.start,
                    end: span.range.end,
                },
            )
        })
        .collect()
}

fn replace_interface_block_range(
    content: &str,
    current: &InterfaceBlockRange,
    replacement: &str,
) -> Zeroizing<String> {
    let mut updated = Zeroizing::new(String::with_capacity(
        content.len() - (current.end - current.start) + replacement.len(),
    ));
    updated.push_str(&content[..current.start]);
    updated.push_str(replacement);
    updated.push_str(&content[current.end..]);
    updated
}

fn interface_block_with_enabled(block: &str, enabled: bool) -> Zeroizing<String> {
    let enabled_value = if enabled { "true" } else { "false" };
    let had_trailing_newline = block.ends_with('\n');
    let mut lines = block
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let mut saw_enabled_key = false;
    let mut insert_idx = 1usize.min(lines.len());
    let mut insert_indent = "    ".to_string();

    for (idx, line) in lines.iter_mut().enumerate().skip(1) {
        let Some((key, _)) = parse_ini_key_value(line) else {
            continue;
        };
        if key == "type" || key == "interface_type" {
            insert_idx = idx + 1;
            insert_indent = line.chars().take_while(|ch| ch.is_whitespace()).collect();
        }
        if key == "enabled" || key == "interface_enabled" {
            *line = replace_ini_value_preserving_comment(line, enabled_value);
            saw_enabled_key = true;
        }
    }

    if !saw_enabled_key {
        lines.insert(
            insert_idx,
            Zeroizing::new(format!("{insert_indent}enabled = {enabled_value}")),
        );
    }

    join_config_text(lines.iter().map(|line| line.as_str()), had_trailing_newline)
}

fn interface_block_with_usb_selector(
    block: &str,
    vendor_id: u16,
    product_id: u16,
    serial_number: Option<&str>,
) -> Zeroizing<String> {
    let had_trailing_newline = block.ends_with('\n');
    let mut lines = block
        .lines()
        .filter(|line| {
            parse_ini_key_value(line).is_none_or(|(key, _)| {
                !matches!(
                    key.as_str(),
                    "usb_vendor_id" | "usb_product_id" | "usb_serial_number"
                )
            })
        })
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let insert_idx = lines
        .iter()
        .position(|line| parse_ini_key_value(line).is_some_and(|(key, _)| key == "port"))
        .map_or(1usize.min(lines.len()), |index| index + 1);
    let indent = lines
        .get(insert_idx.saturating_sub(1))
        .map(|line| line.chars().take_while(|ch| ch.is_whitespace()).collect())
        .unwrap_or_else(|| "    ".to_string());
    let mut selector = vec![
        format!("{indent}usb_vendor_id = {vendor_id}"),
        format!("{indent}usb_product_id = {product_id}"),
    ];
    if let Some(serial_number) = serial_number {
        selector.push(format!("{indent}usb_serial_number = {serial_number}"));
    }
    lines.splice(
        insert_idx..insert_idx,
        selector.into_iter().map(Zeroizing::new),
    );
    join_config_text(lines.iter().map(|line| line.as_str()), had_trailing_newline)
}

fn replace_ini_value_preserving_comment(line: &str, replacement: &str) -> Zeroizing<String> {
    let Some(equals) = line.find('=') else {
        return Zeroizing::new(line.to_string());
    };
    let value = &line[equals + 1..];
    let comment_start = inline_comment_start(value).unwrap_or(value.len());
    let value_area = &value[..comment_start];
    let leading_len = value_area.len() - value_area.trim_start().len();
    let remaining = &value_area[leading_len..];
    let scalar_len = remaining.trim_end().len();
    let trailing_start = leading_len + scalar_len;

    let mut updated = Zeroizing::new(String::with_capacity(line.len() + replacement.len()));
    updated.push_str(&line[..equals + 1]);
    updated.push_str(&value_area[..leading_len]);
    updated.push_str(replacement);
    updated.push_str(&value_area[trailing_start..]);
    updated.push_str(&value[comment_start..]);
    updated
}

fn inline_comment_start(value: &str) -> Option<usize> {
    let mut quote = None;
    for (idx, ch) in value.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch == '#' {
            return Some(idx);
        }
    }
    None
}

pub fn set_interface_enabled(config_dir: &Path, name: &str, enabled: bool) -> bool {
    let Some(content) = read_config(config_dir) else {
        return false;
    };
    let Ok(sections) = rns_runtime::config::ifac::section_spans(&content) else {
        return false;
    };
    let Some(section) = sections
        .iter()
        .find(|span| span.section == "interfaces" && span.path == [name])
    else {
        return false;
    };
    let Ok(properties) = rns_runtime::config::ifac::property_spans(&content) else {
        return false;
    };
    let insertion = properties
        .iter()
        .find(|span| {
            span.section == "interfaces"
                && span.path == [name]
                && matches!(span.key.as_str(), "type" | "interface_type")
        })
        .map_or(section.header_end, |span| span.range.end);
    let mut edits = properties
        .into_iter()
        .filter(|span| {
            span.section == "interfaces"
                && span.path == [name]
                && matches!(span.key.as_str(), "enabled" | "interface_enabled")
        })
        .map(|span| (span.range, format!("    {} = {}\n", span.key, enabled)))
        .collect::<Vec<_>>();
    if edits.is_empty() {
        edits.push((
            insertion..insertion,
            enabled_line(&content, insertion, enabled),
        ));
    }
    let mut updated = content.clone();
    for (range, replacement) in edits.into_iter().rev() {
        reserve_config_space(&mut updated, replacement.len());
        updated.replace_range(range, &replacement);
    }
    write_config_if_unchanged(config_dir, Some(&content), &updated).is_ok()
}

/// An `enabled` line for insertion at `at`, starting a new line if `at` ends
/// an unterminated last line.
fn enabled_line(content: &str, at: usize, enabled: bool) -> String {
    let separator = if at == content.len() && !content.is_empty() && !content.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{separator}    enabled = {enabled}\n")
}

/// Ratspeak's client default: an interface that sets neither `enabled` nor
/// `interface_enabled` is on. rsReticulum follows upstream and skips such a
/// block, so write `enabled = true` into each one before the runtime starts.
pub fn enable_unflagged_interfaces(config_dir: &Path) -> Result<usize, String> {
    let Some(content) = read_config(config_dir) else {
        return Ok(0);
    };
    let (Ok(sections), Ok(properties)) = (
        rns_runtime::config::ifac::section_spans(&content),
        rns_runtime::config::ifac::property_spans(&content),
    ) else {
        return Ok(0);
    };
    let mut insertions = sections
        .iter()
        .filter(|section| section.section == "interfaces" && section.path.len() == 1)
        .filter_map(|section| {
            let mut keys = properties
                .iter()
                .filter(|span| span.section == "interfaces" && span.path == section.path);
            if keys
                .clone()
                .any(|span| matches!(span.key.as_str(), "enabled" | "interface_enabled"))
            {
                return None;
            }
            Some(
                keys.find(|span| matches!(span.key.as_str(), "type" | "interface_type"))
                    .map_or(section.header_end, |span| span.range.end),
            )
        })
        .collect::<Vec<_>>();
    if insertions.is_empty() {
        return Ok(0);
    }
    insertions.sort_unstable();
    let mut updated = content.clone();
    for at in insertions.iter().rev() {
        let line = enabled_line(&content, *at, true);
        reserve_config_space(&mut updated, line.len());
        updated.insert_str(*at, &line);
    }
    write_config_if_unchanged(config_dir, Some(&content), &updated)
        .map_err(|_| "interface configuration changed while it was being prepared".to_string())?;
    Ok(insertions.len())
}

fn remove_interface_blocks_from_content(
    content: &str,
    names: &[&str],
) -> (Zeroizing<String>, bool) {
    let mut ranges = interface_block_ranges(content)
        .into_iter()
        .filter(|(name, _)| names.contains(&name.as_str()))
        .map(|(_, range)| range)
        .collect::<Vec<_>>();
    ranges.sort_by_key(|range| range.start);
    let removed = !ranges.is_empty();
    let mut result = Zeroizing::new(content.to_owned());
    for range in ranges.into_iter().rev() {
        result.replace_range(range.start..range.end, "");
    }
    (result, removed)
}

/// Insert under the authoritative interfaces section, never under a quoted fake header.
fn insert_interface_block(content: &str, block: &str) -> Zeroizing<String> {
    let Ok(spans) = rns_runtime::config::ifac::section_spans(content) else {
        return Zeroizing::new(content.to_owned());
    };
    let mut result = Zeroizing::new(content.to_owned());
    if let Some(section) = spans
        .iter()
        .find(|span| span.section == "interfaces" && span.path.is_empty())
    {
        reserve_config_space(&mut result, block.len());
        result.insert_str(section.range.end, block);
    } else {
        reserve_config_space(&mut result, "\n[interfaces]\n".len() + block.len());
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
    let (without_old, _) = remove_interface_blocks_from_content(&content, remove_names);
    let new_content = insert_interface_block(&without_old, block);
    write_config_if_unchanged(
        config_dir,
        if config_dir.join("config").exists() {
            Some(&content)
        } else {
            None
        },
        &new_content,
    )
    .is_ok()
}

// Full key schema each block generator owns (emitted plus read-side aliases).
// Update carry-over drops these — the generator decides whether to re-emit —
// and preserves everything else (hand edits, newer Reticulum keys).
const COMMON_OWNED_KEYS: &[&str] = &["type", "interface_type", "enabled", "interface_enabled"];
const IFAC_OWNED_KEYS: &[&str] = &[
    "network_name",
    "networkname",
    "passphrase",
    "pass_phrase",
    "ifac_size",
    "ifac_size_unit",
    "ifac_size_bytes",
];
const RNODE_OWNED_KEYS: &[&str] = &[
    "port",
    "mode",
    "interface_mode",
    "frequency",
    "bandwidth",
    "spreadingfactor",
    "codingrate",
    "txpower",
    "airtime_limit_short",
    "airtime_limit_long",
    "discoverable",
    "latitude",
    "longitude",
    "discovery_name",
    "ratspeak_region",
    "ratspeak_preset",
    "usb_vendor_id",
    "usb_product_id",
    "usb_serial_number",
];
const TCP_CLIENT_OWNED_KEYS: &[&str] = &["target_host", "target_port"];
const TCP_SERVER_OWNED_KEYS: &[&str] = &["listen_ip", "listen_port"];
const BACKBONE_CLIENT_OWNED_KEYS: &[&str] = &[
    "target_host",
    "target_port",
    "prefer_ipv6",
    "connect_timeout",
    "max_reconnect_tries",
    "i2p_tunneled",
];
const BACKBONE_SERVER_OWNED_KEYS: &[&str] = &[
    "listen_on",
    "listen_ip",
    "listen_port",
    "prefer_ipv6",
    "device",
];
const HOST_DISCOVERY_OWNED_KEYS: &[&str] = &[
    "discoverable",
    "reachable_on",
    "publish_ifac",
    "discovery_publish_ifac",
    "discovery_encrypt",
];

/// Key lines in `name`'s block that no generator owns, in original order.
fn unowned_block_lines(
    content: &str,
    name: &str,
    owned_keys: &[&[&str]],
) -> Vec<Zeroizing<String>> {
    rns_runtime::config::ifac::property_spans(content)
        .unwrap_or_default()
        .into_iter()
        .filter(|span| {
            span.section == "interfaces"
                && span.path == [name]
                && !owned_keys
                    .iter()
                    .any(|keys| keys.contains(&span.key.as_str()))
        })
        .map(|span| {
            Zeroizing::new(
                content[span.range]
                    .trim_end_matches(['\r', '\n'])
                    .to_owned(),
            )
        })
        .collect()
}

fn replace_interface_block(
    config_dir: &Path,
    old_name: &str,
    new_name: &str,
    block: &str,
    owned_keys: &[&[&str]],
    reject_destination_collision: bool,
) -> bool {
    if !safe_interface_name(old_name) || !safe_interface_name(new_name) {
        return false;
    }
    let content = match read_config(config_dir) {
        Some(c) => c,
        None => return false,
    };
    if reject_destination_collision
        && old_name != new_name
        && !matches!(unique_interface_block_range(&content, new_name), Ok(None))
    {
        return false;
    }
    let mut block = Zeroizing::new(block.to_string());
    for line in unowned_block_lines(&content, old_name, owned_keys) {
        reserve_config_space(&mut block, line.len() + 1);
        block.push_str(&line);
        block.push('\n');
    }
    let remove_names = if old_name == new_name {
        vec![old_name]
    } else {
        vec![old_name, new_name]
    };
    let (without_old, removed_old) = remove_interface_blocks_from_content(&content, &remove_names);
    if !removed_old {
        return false;
    }
    let new_content = insert_interface_block(&without_old, &block);
    write_config_if_unchanged(config_dir, Some(&content), &new_content).is_ok()
}

fn rnode_interface_block(args: RnodeInterfaceArgs<'_>) -> Zeroizing<String> {
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
    let mode = rnode_interface_mode_passthrough(mode);

    let mut block = Zeroizing::new(format!(
        "\n  [[{name}]]\n    type = RNodeInterface\n    port = {port}\n    mode = {mode}\n    frequency = {frequency}\n    bandwidth = {bandwidth}\n    spreadingfactor = {spreading_factor}\n    codingrate = {coding_rate}\n    txpower = {tx_power}\n    enabled = true\n"
    ));
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

fn append_ifac_fields(block: &mut Zeroizing<String>, ifac: InterfaceIfacArgs<'_>) {
    let network_name = ifac.network_name.filter(|s| !s.is_empty());
    let passphrase = ifac.passphrase.filter(|s| !s.is_empty());
    let size_fields = ifac
        .ifac_size
        .map(|size| format!("    ifac_size = {}\n    ifac_size_unit = bits\n", size * 8));
    let additional = network_name.map_or(0, |name| "    network_name = \n".len() + name.len())
        + passphrase.map_or(0, |phrase| "    passphrase = \n".len() + phrase.len())
        + size_fields.as_ref().map_or(0, String::len);
    reserve_config_space(block, additional);
    if let Some(network_name) = network_name {
        block.push_str("    network_name = ");
        block.push_str(network_name);
        block.push('\n');
    }
    if let Some(passphrase) = passphrase {
        block.push_str("    passphrase = ");
        block.push_str(passphrase);
        block.push('\n');
    }
    if let Some(size_fields) = size_fields {
        block.push_str(&size_fields);
    }
}

fn tcp_client_block(
    name: &str,
    host: &str,
    port: u16,
    ifac: InterfaceIfacArgs<'_>,
) -> Zeroizing<String> {
    let mut block = Zeroizing::new(format!(
        "\n  [[{name}]]\n    type = TCPClientInterface\n    target_host = {host}\n    target_port = {port}\n    enabled = true\n"
    ));
    append_ifac_fields(&mut block, ifac);
    block
}

fn append_host_discovery_fields(block: &mut Zeroizing<String>, discovery: HostDiscoveryArgs<'_>) {
    if discovery.discoverable {
        block.push_str("    discoverable = yes\n");
    }
    if let Some(reachable_on) = discovery.reachable_on.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    reachable_on = {reachable_on}\n"));
    }
    if discovery.publish_ifac {
        block.push_str("    publish_ifac = yes\n");
    }
    if discovery.discovery_encrypt {
        block.push_str("    discovery_encrypt = yes\n");
    }
}

fn tcp_server_block(
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> Zeroizing<String> {
    let mut block = Zeroizing::new(format!(
        "\n  [[{name}]]\n    type = TCPServerInterface\n    listen_ip = {listen_ip}\n    listen_port = {listen_port}\n    enabled = true\n"
    ));
    append_ifac_fields(&mut block, ifac);
    append_host_discovery_fields(&mut block, discovery);
    block
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

fn backbone_client_block(args: BackboneClientArgs<'_>) -> Zeroizing<String> {
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

    let mut block = Zeroizing::new(format!(
        "\n  [[{name}]]\n    type = BackboneInterface\n    target_host = {host}\n    target_port = {port}\n    enabled = true\n"
    ));
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
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> Zeroizing<String> {
    let mut block = Zeroizing::new(format!(
        "\n  [[{name}]]\n    type = BackboneInterface\n    listen_on = {listen_ip}\n    listen_port = {listen_port}\n    enabled = true\n"
    ));
    if prefer_ipv6 {
        block.push_str("    prefer_ipv6 = true\n");
    }
    if let Some(d) = device.filter(|s| !s.is_empty()) {
        block.push_str(&format!("    device = {d}\n"));
    }
    append_ifac_fields(&mut block, ifac);
    append_host_discovery_fields(&mut block, discovery);
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
    add_tcp_server_with_ifac(
        config_dir,
        name,
        listen_port,
        listen_ip,
        InterfaceIfacArgs::default(),
        HostDiscoveryArgs::default(),
    )
}

pub fn add_tcp_server_with_ifac(
    config_dir: &Path,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> bool {
    if !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
        || !safe_ifac_args(ifac)
        || !safe_optional_scalar(discovery.reachable_on)
    {
        return false;
    }
    let block = tcp_server_block(name, listen_port, listen_ip, ifac, discovery);
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
    add_backbone_server_with_ifac(
        config_dir,
        name,
        listen_port,
        listen_ip,
        prefer_ipv6,
        device,
        InterfaceIfacArgs::default(),
        HostDiscoveryArgs::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn add_backbone_server_with_ifac(
    config_dir: &Path,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    prefer_ipv6: bool,
    device: Option<&str>,
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> bool {
    if !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
        || !safe_optional_scalar(device)
        || !safe_ifac_args(ifac)
        || !safe_optional_scalar(discovery.reachable_on)
    {
        return false;
    }
    // Synthesizer reads `listen_on` (interface_factory.rs); IPC arg is
    // `listen_ip` for parity with the TCP server command.
    let block = backbone_server_block(
        name,
        listen_port,
        listen_ip,
        prefer_ipv6,
        device,
        ifac,
        discovery,
    );
    upsert_interface_block(config_dir, &[name], &block)
}

pub fn update_rnode_interface(
    config_dir: &Path,
    old_name: &str,
    args: RnodeInterfaceArgs<'_>,
) -> bool {
    update_rnode_interface_with_usb_selector(config_dir, old_name, args, None)
}

pub fn update_rnode_interface_with_usb_selector(
    config_dir: &Path,
    old_name: &str,
    args: RnodeInterfaceArgs<'_>,
    usb_selector: Option<RnodeUsbSelectorArgs<'_>>,
) -> bool {
    if !safe_interface_name(old_name) || !safe_rnode_args(args) {
        return false;
    }
    if usb_selector.is_some_and(|selector| {
        !args.port.starts_with("androidusb://") || !safe_optional_scalar(selector.serial_number)
    }) {
        return false;
    }
    let mut block = rnode_interface_block(args);
    if let Some(selector) = usb_selector {
        block = interface_block_with_usb_selector(
            &block,
            selector.vendor_id,
            selector.product_id,
            selector.serial_number,
        );
    }
    replace_interface_block(
        config_dir,
        old_name,
        args.name,
        &block,
        &[COMMON_OWNED_KEYS, RNODE_OWNED_KEYS],
        true,
    )
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
    replace_interface_block(
        config_dir,
        old_name,
        name,
        &block,
        &[COMMON_OWNED_KEYS, IFAC_OWNED_KEYS, TCP_CLIENT_OWNED_KEYS],
        false,
    )
}

pub fn update_tcp_server(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
) -> bool {
    update_tcp_server_with_ifac(
        config_dir,
        old_name,
        name,
        listen_port,
        listen_ip,
        InterfaceIfacArgs::default(),
        HostDiscoveryArgs::default(),
    )
}

pub fn update_tcp_server_with_ifac(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> bool {
    if !safe_interface_name(old_name)
        || !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
        || !safe_ifac_args(ifac)
        || !safe_optional_scalar(discovery.reachable_on)
    {
        return false;
    }
    let block = tcp_server_block(name, listen_port, listen_ip, ifac, discovery);
    replace_interface_block(
        config_dir,
        old_name,
        name,
        &block,
        &[
            COMMON_OWNED_KEYS,
            IFAC_OWNED_KEYS,
            TCP_SERVER_OWNED_KEYS,
            HOST_DISCOVERY_OWNED_KEYS,
        ],
        false,
    )
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
    replace_interface_block(
        config_dir,
        old_name,
        args.name,
        &block,
        &[
            COMMON_OWNED_KEYS,
            IFAC_OWNED_KEYS,
            BACKBONE_CLIENT_OWNED_KEYS,
        ],
        false,
    )
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
    update_backbone_server_with_ifac(
        config_dir,
        old_name,
        name,
        listen_port,
        listen_ip,
        prefer_ipv6,
        device,
        InterfaceIfacArgs::default(),
        HostDiscoveryArgs::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn update_backbone_server_with_ifac(
    config_dir: &Path,
    old_name: &str,
    name: &str,
    listen_port: u16,
    listen_ip: &str,
    prefer_ipv6: bool,
    device: Option<&str>,
    ifac: InterfaceIfacArgs<'_>,
    discovery: HostDiscoveryArgs<'_>,
) -> bool {
    if !safe_interface_name(old_name)
        || !safe_interface_name(name)
        || !safe_config_scalar(listen_ip)
        || !safe_optional_scalar(device)
        || !safe_ifac_args(ifac)
        || !safe_optional_scalar(discovery.reachable_on)
    {
        return false;
    }
    let block = backbone_server_block(
        name,
        listen_port,
        listen_ip,
        prefer_ipv6,
        device,
        ifac,
        discovery,
    );
    replace_interface_block(
        config_dir,
        old_name,
        name,
        &block,
        &[
            COMMON_OWNED_KEYS,
            IFAC_OWNED_KEYS,
            BACKBONE_SERVER_OWNED_KEYS,
            HOST_DISCOVERY_OWNED_KEYS,
        ],
        false,
    )
}

pub fn set_transport_mode(config_dir: &Path, enable: bool) -> bool {
    let previous = read_config(config_dir);
    let content = previous
        .clone()
        .unwrap_or_else(|| ratspeak_default_config(config_dir));
    write_config_if_unchanged(
        config_dir,
        previous.as_deref().map(String::as_str),
        &transport_mode_update(&content, enable),
    )
    .is_ok()
}

fn transport_mode_update(content: &str, enable: bool) -> Zeroizing<String> {
    let val = if enable { "True" } else { "False" };
    let mut lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    let Some((section_start, section_end)) = reticulum_section_bounds(&lines) else {
        let mut new_lines = vec![
            "[reticulum]".to_string(),
            format!("enable_transport = {val}"),
            String::new(),
        ]
        .into_iter()
        .map(Zeroizing::new)
        .collect::<Vec<_>>();
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
        [Zeroizing::new(format!("enable_transport = {val}"))],
    );
    join_config_lines(lines)
}

/// Writes the Network settings' `[reticulum]` and `[stamps]` keys; true when the file changed.
pub fn apply_network_settings(
    config_dir: &Path,
    settings: &ratspeak_core::types::NetworkSettings,
) -> std::io::Result<bool> {
    let previous = read_config(config_dir);
    let mut updated = previous
        .clone()
        .unwrap_or_else(|| ratspeak_default_config(config_dir));
    let own_network = network_identity_hash(&updated);
    for (section, keys) in network_settings_keys(settings, own_network.as_deref()) {
        updated = section_keys_update(&updated, section, &keys);
    }
    if previous.as_deref() == Some(&*updated) {
        return Ok(false);
    }
    write_config_if_unchanged(
        config_dir,
        previous.as_deref().map(String::as_str),
        &updated,
    )
    .map(|_| true)
}

/// Where a profile keeps its network identity: a raw 64-byte Reticulum
/// identity file in Reticulum's identity store under its config.
pub fn network_identity_path(config_dir: &Path) -> PathBuf {
    config_dir
        .join("storage")
        .join("identities")
        .join("network")
}

/// The file `[reticulum] network_identity` names, resolved as Reticulum does.
pub fn configured_network_identity_path(config_dir: &Path) -> Option<PathBuf> {
    network_identity_path_in(&read_config(config_dir)?)
}

fn network_identity_path_in(content: &str) -> Option<PathBuf> {
    let config = rns_runtime::config::Config::parse(content).ok()?;
    rns_runtime::reticulum::ReticulumConfig::try_from_config(&config)
        .ok()?
        .network_identity_path
}

/// The hash of the network identity `content` names, when its file loads.
fn network_identity_hash(content: &str) -> Option<String> {
    rns_identity::identity::Identity::from_file(&network_identity_path_in(content)?)
        .ok()
        .map(|identity| identity.hexhash())
}

/// The configured network identity, when its file loads.
pub fn load_network_identity(config_dir: &Path) -> Option<rns_identity::identity::Identity> {
    rns_identity::identity::Identity::from_file(&configured_network_identity_path(config_dir)?).ok()
}

/// Points `[reticulum] network_identity` at [`network_identity_path`], or
/// removes it; true when the config changed. The path is absolute and quoted,
/// since Reticulum resolves a relative one against its working directory.
pub fn set_network_identity_key(config_dir: &Path, on: bool) -> std::io::Result<bool> {
    let value = if on {
        let path = std::path::absolute(network_identity_path(config_dir))?;
        Some(quoted_config_path(&path).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the profile path cannot be written into the config",
            )
        })?)
    } else {
        None
    };
    let previous = read_config(config_dir);
    let content = previous
        .clone()
        .unwrap_or_else(|| ratspeak_default_config(config_dir));
    let updated = section_keys_update(&content, "reticulum", &[("network_identity", value)]);
    if previous.as_deref() == Some(&*updated) {
        return Ok(false);
    }
    write_config_if_unchanged(
        config_dir,
        previous.as_deref().map(String::as_str),
        &updated,
    )
    .map(|_| true)
}

/// Keeps `network_identity` on the profile's own file while it exists, so a
/// moved data directory still finds it; a key naming another file is left as is.
pub fn apply_network_identity(config_dir: &Path) -> std::io::Result<bool> {
    if !network_identity_path(config_dir).is_file() {
        return Ok(false);
    }
    set_network_identity_key(config_dir, true)
}

fn quoted_config_path(path: &Path) -> Option<String> {
    let path = path.to_str()?;
    if path.contains(['\r', '\n', '\0']) {
        return None;
    }
    if !path.contains('"') {
        Some(format!("\"{path}\""))
    } else if !path.contains('\'') {
        Some(format!("'{path}'"))
    } else {
        None
    }
}

/// The Network settings a config's own keys describe, for a profile that has
/// stored none; `None` while Reticulum would refuse the config or one of them.
pub fn network_settings_from_config(
    config_dir: &Path,
) -> Option<ratspeak_core::types::NetworkSettings> {
    match read_config(config_dir) {
        Some(content) => network_settings_from_content(&content),
        None => Some(Default::default()),
    }
}

fn network_settings_from_content(content: &str) -> Option<ratspeak_core::types::NetworkSettings> {
    use rns_runtime::config::{Config, ConfigSection};
    let config = Config::parse(content).ok()?;
    let none = ConfigSection::default();
    let reticulum = config.section("reticulum").unwrap_or(&none);
    let stamps = config.section("stamps").unwrap_or(&none);
    // Unset reads as `Some(None)`; a value Reticulum refuses as `None`.
    let flag = |section: &ConfigSection, key: &str| {
        if section.has(key) {
            section.get_bool(key).map(Some)
        } else {
            Some(None)
        }
    };
    let autojoin = if reticulum.has("autoconnect_discovered_interfaces") {
        let limit = reticulum.get_uint("autoconnect_discovered_interfaces")?;
        u8::try_from(limit).unwrap_or(u8::MAX)
    } else {
        0
    };
    let sources = reticulum
        .get_list("interface_discovery_sources")
        .unwrap_or_default();
    // This device's own network is accepted anyway, so it never joins the trusted list.
    let own_network = network_identity_hash(content);
    let mut trusted_publishers: Vec<String> = Vec::new();
    for source in &sources {
        let source = source.trim().to_ascii_lowercase();
        if hex::decode(&source).ok()?.len() != 16 {
            return None;
        }
        if own_network.as_ref() != Some(&source) && !trusted_publishers.contains(&source) {
            trusted_publishers.push(source);
        }
    }
    Some(ratspeak_core::types::NetworkSettings {
        discover: flag(reticulum, "discover_interfaces")? == Some(true),
        autojoin,
        autoconnect_unknown_implementations: flag(
            reticulum,
            "autoconnect_unverified_implementations",
        )? == Some(true),
        require_stamps: flag(stamps, "enforce_stamps")? == Some(true),
        link_mtu_discovery_off: flag(reticulum, "link_mtu_discovery")? == Some(false),
        trusted_only: !sources.is_empty(),
        trusted_publishers,
        other: Default::default(),
    })
}

type SectionKeys = Vec<(&'static str, Option<String>)>;

/// `own_network`, this device's network identity hash, is always a source: its
/// private network is decrypted with its own key, whoever else is trusted.
fn network_settings_keys(
    s: &ratspeak_core::types::NetworkSettings,
    own_network: Option<&str>,
) -> Vec<(&'static str, SectionKeys)> {
    // Defaults are left out, so an untouched config stays as upstream writes it.
    let yes = |on: bool| on.then(|| "yes".to_string());
    let list = |v: &[String]| (!v.is_empty()).then(|| v.join(", "));
    let mut sources = s.trusted_publishers.clone();
    if let Some(own) = own_network
        && !sources.iter().any(|source| source == own)
    {
        sources.push(own.to_string());
    }
    // Trusting no one accepts no one; upstream reads an empty list as everyone.
    let discover = s.discover && !(s.trusted_only && sources.is_empty());
    let autojoin = discover && s.autojoin > 0;
    vec![
        (
            "reticulum",
            vec![
                ("discover_interfaces", yes(discover)),
                (
                    "autoconnect_discovered_interfaces",
                    autojoin.then(|| s.autojoin.to_string()),
                ),
                (
                    "autoconnect_unverified_implementations",
                    yes(autojoin && s.autoconnect_unknown_implementations),
                ),
                (
                    "interface_discovery_sources",
                    if s.trusted_only { list(&sources) } else { None },
                ),
                (
                    "link_mtu_discovery",
                    s.link_mtu_discovery_off.then(|| "no".to_string()),
                ),
            ],
        ),
        // Announces and path requests are always stamped; their keys can only
        // turn that on, so they are not Ratspeak's to write.
        ("stamps", vec![("enforce_stamps", yes(s.require_stamps))]),
    ]
}

/// Sets or removes (`None`) keys in `[section]`, keeping every other line. A
/// missing section is added, after `[reticulum]`, only to hold a key; one left
/// holding nothing is removed.
fn section_keys_update(
    content: &str,
    section: &str,
    keys: &[(&str, Option<String>)],
) -> Zeroizing<String> {
    let mut lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
    if section_bounds(&lines, section).is_none() {
        if keys.iter().all(|(_, value)| value.is_none()) {
            return join_config_lines(lines);
        }
        let at = if section == "reticulum" {
            0
        } else {
            reticulum_section_bounds(&lines).map_or(0, |(_, end)| end)
        };
        let mut header = Vec::new();
        if at > 0 && !lines[at - 1].trim().is_empty() {
            header.push(String::new());
        }
        header.push(format!("[{section}]"));
        if at < lines.len() {
            header.push(String::new());
        }
        lines.splice(at..at, header.into_iter().map(Zeroizing::new));
    }
    let mut removed = false;
    for (key, value) in keys {
        let (start, end) = section_bounds(&lines, section).expect("section exists");
        let at = ((start + 1)..end)
            .find(|idx| parse_ini_key_value(&lines[*idx]).is_some_and(|(k, _)| k == *key));
        match (at, value) {
            (Some(idx), Some(v)) => lines[idx] = replace_key_line(&lines[idx], key, v),
            (Some(idx), None) => {
                lines.remove(idx);
                removed = true;
            }
            (None, Some(v)) => {
                // Before the section's trailing blank lines.
                let mut insert = end;
                while insert > start + 1 && lines[insert - 1].trim().is_empty() {
                    insert -= 1;
                }
                lines.insert(insert, Zeroizing::new(format!("{key} = {v}")));
            }
            (None, None) => {}
        }
    }
    if removed {
        let (start, end) = section_bounds(&lines, section).expect("section exists");
        if lines[start + 1..end]
            .iter()
            .all(|line| line.trim().is_empty())
        {
            lines.drain(start..end);
        }
    }
    join_config_lines(lines)
}

pub fn transport_mode_enabled(config_dir: &Path) -> bool {
    read_config(config_dir)
        .and_then(|content| transport_enabled_from_config(&content))
        .unwrap_or(false)
}

fn transport_enabled_from_config(content: &str) -> Option<bool> {
    let lines = content
        .lines()
        .map(|line| Zeroizing::new(line.to_string()))
        .collect::<Vec<_>>();
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

    fn reticulum_value<'a>(content: &'a str, wanted: &str) -> Option<&'a str> {
        let mut in_reticulum = false;
        for line in content.lines() {
            if named_top_level_section(line, "reticulum") {
                in_reticulum = true;
                continue;
            }
            if in_reticulum && is_top_level_section(line) {
                break;
            }
            if in_reticulum {
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                if key.trim().eq_ignore_ascii_case(wanted) {
                    return Some(value.trim());
                }
            }
        }
        None
    }

    #[test]
    fn write_config_creates_missing_config_dir() {
        let dir = temp_config_dir().join("identity").join("reticulum");
        assert!(!dir.exists());

        write_config_result(&dir, "[interfaces]\n").unwrap();

        assert_eq!(read_config(&dir).unwrap().as_str(), "[interfaces]\n");
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
    fn network_settings_own_their_reticulum_keys_and_nothing_else() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        let base = "[reticulum]\nenable_transport = False\nshare_instance = No\n\n[logging]\nloglevel = 4\n\n[interfaces]\n";
        std::fs::write(dir.join("config"), base).unwrap();
        let on = NetworkSettings {
            discover: true,
            autojoin: 3,
            trusted_only: true,
            trusted_publishers: vec!["a".repeat(32), "b".repeat(32)],
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &on).unwrap());
        let text = read_config(&dir).unwrap();
        let cfg = rns_runtime::config::Config::parse(&text).unwrap();
        let rc = rns_runtime::reticulum::ReticulumConfig::try_from_config(&cfg).unwrap();
        assert!(rc.discover_interfaces, "{}", text.as_str());
        assert_eq!(rc.autoconnect_discovered_interfaces, 3);
        assert_eq!(rc.interface_discovery_sources.len(), 2);
        assert!(
            text.contains("enable_transport = False\nshare_instance = No\n")
                && text.contains("[logging]\nloglevel = 4")
        );
        assert!(
            !apply_network_settings(&dir, &on).unwrap(),
            "unchanged settings leave the file alone"
        );

        // Joining needs discovery; with it off neither key is written.
        let listening_off = NetworkSettings {
            discover: false,
            ..on.clone()
        };
        apply_network_settings(&dir, &listening_off).unwrap();
        let text = read_config(&dir).unwrap();
        assert!(
            !text.contains("discover_interfaces") && !text.contains("autoconnect"),
            "{}",
            text.as_str()
        );

        // Trusting no one turns discovery off rather than accepting everyone.
        let nobody = NetworkSettings {
            trusted_publishers: vec![],
            ..on.clone()
        };
        apply_network_settings(&dir, &nobody).unwrap();
        let text = read_config(&dir).unwrap();
        assert!(
            !text.contains("discover_interfaces") && !text.contains("interface_discovery_sources"),
            "{}",
            text.as_str()
        );

        apply_network_settings(&dir, &NetworkSettings::default()).unwrap();
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            base,
            "defaults leave the file as it was"
        );
    }

    #[test]
    fn network_settings_start_a_config_that_has_no_reticulum_section() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        std::fs::write(dir.join("config"), "[interfaces]\n").unwrap();
        let on = NetworkSettings {
            discover: true,
            autojoin: 1,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &on).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "[reticulum]\ndiscover_interfaces = yes\nautoconnect_discovered_interfaces = 1\n\n[interfaces]\n"
        );
    }

    fn runtime_config(dir: &Path) -> rns_runtime::reticulum::ReticulumConfig {
        let text = read_config(dir).unwrap();
        let cfg = rns_runtime::config::Config::parse(&text).unwrap();
        rns_runtime::reticulum::ReticulumConfig::try_from_config(&cfg).unwrap()
    }

    #[test]
    fn unknown_implementations_are_joined_only_while_joining() {
        use ratspeak_core::types::NetworkSettings;
        const KEY: &str = "autoconnect_unverified_implementations";
        let dir = temp_config_dir();
        let base = "[reticulum]\nshare_instance = No\n\n[interfaces]\n";
        std::fs::write(dir.join("config"), base).unwrap();
        let on = NetworkSettings {
            discover: true,
            autojoin: 2,
            autoconnect_unknown_implementations: true,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &on).unwrap());
        let text = read_config(&dir).unwrap();
        assert!(
            text.contains(
                "autoconnect_discovered_interfaces = 2\nautoconnect_unverified_implementations = yes\n"
            ),
            "{}",
            text.as_str()
        );
        assert!(runtime_config(&dir).autoconnect_unverified_implementations);

        // Off, joining off, discovery off, or trusting no one: the key goes and the runtime default holds.
        let offs = [
            NetworkSettings {
                autoconnect_unknown_implementations: false,
                ..on.clone()
            },
            NetworkSettings {
                autojoin: 0,
                ..on.clone()
            },
            NetworkSettings {
                discover: false,
                ..on.clone()
            },
            NetworkSettings {
                trusted_only: true,
                ..on.clone()
            },
        ];
        for off in &offs {
            apply_network_settings(&dir, &on).unwrap();
            assert!(apply_network_settings(&dir, off).unwrap(), "{off:?}");
            let text = read_config(&dir).unwrap();
            assert!(!text.contains(KEY), "{off:?}\n{}", text.as_str());
            assert!(!runtime_config(&dir).autoconnect_unverified_implementations);
        }

        // A line the user wrote goes too once the setting is off.
        std::fs::write(
            dir.join("config"),
            format!("[reticulum]\nshare_instance = No\n{KEY} = True\n\n[interfaces]\n"),
        )
        .unwrap();
        apply_network_settings(&dir, &offs[0]).unwrap();
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "[reticulum]\nshare_instance = No\ndiscover_interfaces = yes\nautoconnect_discovered_interfaces = 2\n\n[interfaces]\n"
        );
        assert!(!runtime_config(&dir).autoconnect_unverified_implementations);
    }

    #[test]
    fn require_stamps_gets_a_section_only_while_on() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        let base = "# Ratspeak\n\n[reticulum]\nshare_instance = No\n\n[logging]\nloglevel = 4\n\n[interfaces]\n  [[Home]]\n    type = TCPClientInterface\n";
        std::fs::write(dir.join("config"), base).unwrap();
        // Announces and path requests are stamped with no key written.
        assert!(!apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        let stamps = runtime_config(&dir).stamps;
        assert!(stamps.stamp_destinations && stamps.stamp_prs);
        assert!(!stamps.stamps_enforced && !stamps.lr_stamps_enforced);

        let requiring = NetworkSettings {
            require_stamps: true,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &requiring).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "# Ratspeak\n\n[reticulum]\nshare_instance = No\n\n[stamps]\nenforce_stamps = yes\n\n[logging]\nloglevel = 4\n\n[interfaces]\n  [[Home]]\n    type = TCPClientInterface\n"
        );
        let stamps = runtime_config(&dir).stamps;
        assert!(stamps.stamp_destinations && stamps.stamp_prs);
        assert!(stamps.stamps_enforced && stamps.lr_stamps_enforced);

        // Back to the defaults, the section it added goes and the file is as it was.
        assert!(apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        assert_eq!(read_config(&dir).unwrap().as_str(), base);
        assert_eq!(
            runtime_config(&dir).stamps,
            rns_transport::stamps::StampPolicy::default()
        );
        assert!(!apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
    }

    #[test]
    fn require_stamps_keeps_the_users_stamps_section() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        let base = "[reticulum]\nshare_instance = No\n\n[stamps]\n  # Tuned for this hub.\n  destination_cost = 24.8\n  stamp_prs = yes\n\n[interfaces]\n";
        std::fs::write(dir.join("config"), base).unwrap();
        let requiring = NetworkSettings {
            require_stamps: true,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &requiring).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "[reticulum]\nshare_instance = No\n\n[stamps]\n  # Tuned for this hub.\n  destination_cost = 24.8\n  stamp_prs = yes\nenforce_stamps = yes\n\n[interfaces]\n"
        );
        assert_eq!(runtime_config(&dir).stamps.destination_cost, 24.8);

        // Off, its key goes; the user's own lines stay.
        assert!(apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        assert_eq!(read_config(&dir).unwrap().as_str(), base);
        let stamps = runtime_config(&dir).stamps;
        assert!(stamps.stamp_prs && !stamps.stamps_enforced);
        assert_eq!(stamps.destination_cost, 24.8);
    }

    #[test]
    fn stamp_lines_from_an_older_install_are_left_and_cannot_turn_stamping_off() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        for written in [
            "stamp_destinations = yes\nstamp_prs = yes\n",
            "stamp_prs = yes\n",
            "stamp_destinations = no\nstamp_prs = no\n",
        ] {
            let config =
                format!("[reticulum]\nshare_instance = No\n\n[stamps]\n{written}\n[interfaces]\n");
            std::fs::write(dir.join("config"), &config).unwrap();
            let seeded = network_settings_from_config(&dir).unwrap();
            assert_eq!(seeded, NetworkSettings::default(), "{written}");
            assert!(!apply_network_settings(&dir, &seeded).unwrap(), "{written}");
            assert_eq!(read_config(&dir).unwrap().as_str(), config);
            let stamps = runtime_config(&dir).stamps;
            assert!(stamps.stamp_destinations && stamps.stamp_prs, "{written}");
        }
    }

    #[test]
    fn link_mtu_discovery_is_written_only_when_off() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        let base = "[reticulum]\nshare_instance = No\n\n[logging]\nloglevel = 4\n\n[interfaces]\n";
        std::fs::write(dir.join("config"), base).unwrap();
        assert!(!apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        assert!(runtime_config(&dir).link_mtu_discovery);

        let off = NetworkSettings {
            link_mtu_discovery_off: true,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &off).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "[reticulum]\nshare_instance = No\nlink_mtu_discovery = no\n\n[logging]\nloglevel = 4\n\n[interfaces]\n"
        );
        assert!(!runtime_config(&dir).link_mtu_discovery);

        assert!(apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        assert_eq!(read_config(&dir).unwrap().as_str(), base);
        assert!(runtime_config(&dir).link_mtu_discovery);
    }

    /// Every managed key, set by hand.
    const HAND_WRITTEN_NETWORK_KEYS: &str = "[reticulum]\nshare_instance = No\ndiscover_interfaces = yes\nautoconnect_discovered_interfaces = 3\nautoconnect_unverified_implementations = yes\ninterface_discovery_sources = aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa, bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nlink_mtu_discovery = no\n\n[stamps]\n  destination_cost = 12\nstamp_destinations = yes\nstamp_prs = yes\nenforce_stamps = yes\n\n[interfaces]\n";

    fn hand_written_network_settings() -> ratspeak_core::types::NetworkSettings {
        ratspeak_core::types::NetworkSettings {
            discover: true,
            autojoin: 3,
            autoconnect_unknown_implementations: true,
            require_stamps: true,
            link_mtu_discovery_off: true,
            trusted_only: true,
            trusted_publishers: vec!["a".repeat(32), "b".repeat(32)],
            other: Default::default(),
        }
    }

    #[test]
    fn network_settings_taken_from_a_config_write_back_its_own_keys() {
        let dir = temp_config_dir();
        std::fs::write(dir.join("config"), HAND_WRITTEN_NETWORK_KEYS).unwrap();
        let seeded = network_settings_from_config(&dir).unwrap();
        assert_eq!(seeded, hand_written_network_settings());
        assert!(!apply_network_settings(&dir, &seeded).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            HAND_WRITTEN_NETWORK_KEYS
        );

        // Spelled otherwise, a value reads as Reticulum reads it.
        std::fs::write(
            dir.join("config"),
            "[reticulum]\ndiscover_interfaces = True\ninterface_discovery_sources = AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n\n[interfaces]\n",
        )
        .unwrap();
        let seeded = network_settings_from_config(&dir).unwrap();
        assert!(seeded.discover && seeded.trusted_only);
        assert_eq!(seeded.trusted_publishers, vec!["a".repeat(32)]);

        let missing = temp_config_dir();
        assert_eq!(
            network_settings_from_config(&missing),
            Some(ratspeak_core::types::NetworkSettings::default())
        );
    }

    #[test]
    fn a_network_key_reticulum_refuses_seeds_nothing() {
        let dir = temp_config_dir();
        for refused in [
            "[reticulum]\ndiscover_interfaces = maybe\n",
            "[reticulum]\nautoconnect_discovered_interfaces = -1\n",
            "[reticulum]\ninterface_discovery_sources = not-a-hash\n",
            "[stamps]\nenforce_stamps = sometimes\n",
        ] {
            std::fs::write(dir.join("config"), refused).unwrap();
            assert_eq!(network_settings_from_config(&dir), None, "{refused}");
        }
    }

    #[test]
    fn a_reticulum_section_added_for_a_key_goes_with_it() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        std::fs::write(dir.join("config"), "[interfaces]\n").unwrap();
        assert!(!apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        let off = NetworkSettings {
            link_mtu_discovery_off: true,
            require_stamps: true,
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &off).unwrap());
        assert_eq!(
            read_config(&dir).unwrap().as_str(),
            "[reticulum]\nlink_mtu_discovery = no\n\n[stamps]\nenforce_stamps = yes\n\n[interfaces]\n"
        );
        assert!(apply_network_settings(&dir, &NetworkSettings::default()).unwrap());
        assert_eq!(read_config(&dir).unwrap().as_str(), "[interfaces]\n");
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

        let change = ensure_app_private_shared_instance_ports(&dir).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Created);
        let content = read_config(&dir).unwrap();
        assert_eq!(reticulum_value(&content, "share_instance"), Some("No"));
        let expected_instance_name = ratspeak_private_instance_name(&dir);
        assert!(content.contains("shared_instance_port = 37430"));
        assert!(content.contains("instance_control_port = 37431"));
        assert_eq!(
            reticulum_value(&content, "instance_name"),
            Some(expected_instance_name.as_str())
        );
        assert!(!content.contains("instance_name = default"));
        assert!(content.contains("[interfaces]"));
        assert!(!content.contains("[[Default Interface]]"));
        assert!(!content.contains("type = AutoInterface"));
        assert!(!dir.join("config.backup").exists());
    }

    #[test]
    fn app_private_instance_names_are_stable_opaque_and_path_scoped() {
        let first = temp_config_dir();
        let second = temp_config_dir();

        let first_name = ratspeak_private_instance_name(&first);
        let second_name = ratspeak_private_instance_name(&second);

        assert_eq!(first_name, ratspeak_private_instance_name(&first));
        assert_ne!(first_name, second_name);
        assert!(first_name.starts_with("ratspeak-"));
        assert_eq!(first_name.len(), "ratspeak-".len() + 16);
        assert!(
            first_name["ratspeak-".len()..]
                .chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        );
        assert!(!first_name.contains(first.file_name().unwrap().to_string_lossy().as_ref()));
    }

    #[test]
    fn explicit_key_or_carrier_preserves_canonical_upstream_ports_before_migration() {
        for explicit in [
            "rpc_key = 1234",
            "shared_instance_type = tcp",
            "instance_name = operator",
        ] {
            let dir = temp_config_dir();
            let original = format!(
                "[reticulum]\nshare_instance = Yes\n{explicit}\nshared_instance_port = 37428\ninstance_control_port = 37429\n[interfaces]\n"
            );
            write_config(&dir, &original);
            assert_eq!(
                ensure_app_private_shared_instance(&dir).unwrap(),
                RatspeakRnsPortConfigChange::Unchanged
            );
            assert_eq!(read_config(&dir).unwrap().as_str(), original);
        }
    }

    #[test]
    fn ensure_app_private_instance_migrates_default_selector_atomically() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nenable_transport = False\nshare_instance = Yes\ninstance_name = default\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        let change = ensure_app_private_shared_instance(&dir).unwrap();

        assert_eq!(change, RatspeakRnsPortConfigChange::Updated);
        let content = read_config(&dir).unwrap();
        let expected = ratspeak_private_instance_name(&dir);
        assert_eq!(
            reticulum_value(&content, "instance_name"),
            Some(expected.as_str())
        );
        assert!(!content.contains("instance_name = default"));
        assert_eq!(
            std::fs::read_to_string(dir.join("config.backup")).unwrap(),
            before.as_str()
        );

        assert_eq!(
            ensure_app_private_shared_instance(&dir).unwrap(),
            RatspeakRnsPortConfigChange::Unchanged
        );
        assert_eq!(read_config(&dir).unwrap().as_str(), content.as_str());
        assert_eq!(
            std::fs::read_to_string(dir.join("config.backup")).unwrap(),
            before.as_str()
        );
    }

    #[test]
    fn ensure_app_private_instance_inserts_selector_and_migrates_legacy_ports_once() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[reticulum]\nenable_transport = False\nshare_instance = Yes\nshared_instance_port = 37428\ninstance_control_port = 37429\n\n[interfaces]\n",
        );
        let before = read_config(&dir).unwrap();

        assert_eq!(
            ensure_app_private_shared_instance(&dir).unwrap(),
            RatspeakRnsPortConfigChange::Updated
        );

        let content = read_config(&dir).unwrap();
        let expected = ratspeak_private_instance_name(&dir);
        assert_eq!(
            reticulum_value(&content, "instance_name"),
            Some(expected.as_str())
        );
        assert_eq!(
            reticulum_value(&content, "shared_instance_port"),
            Some("37430")
        );
        assert_eq!(
            reticulum_value(&content, "instance_control_port"),
            Some("37431")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("config.backup")).unwrap(),
            before.as_str()
        );
    }

    #[test]
    fn ensure_app_private_instance_preserves_explicit_shared_instance_ownership() {
        let explicit_configs = [
            "[reticulum]\nshare_instance = Yes\ninstance_name = operator-instance\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
            "[reticulum]\nshare_instance = Yes\ninstance_name = default\nshared_instance_type = tcp\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
            "[reticulum]\nshare_instance = Yes\ninstance_name = default\nrpc_key = 42424242\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
            "[reticulum]\nshare_instance = Yes\ninstance_name = default\nshared_instance_port = 39000\ninstance_control_port = 39001\n\n[interfaces]\n",
            "[reticulum]\nshare_instance = No\ninstance_name = default\nshared_instance_port = 37430\ninstance_control_port = 37431\n\n[interfaces]\n",
        ];

        for config in explicit_configs {
            let dir = temp_config_dir();
            write_config(&dir, config);
            let before = read_config(&dir).unwrap();

            assert_eq!(
                ensure_app_private_shared_instance(&dir).unwrap(),
                RatspeakRnsPortConfigChange::Unchanged
            );
            assert_eq!(read_config(&dir).unwrap(), before);
            assert!(!dir.join("config.backup").exists());
        }
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

        assert_eq!(
            strip_legacy_default_auto_interface(content).as_str(),
            content
        );
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
            before.as_str()
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
        assert!(content.contains("ifac_size = 64\n    ifac_size_unit = bits"));
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
    fn checked_interface_remove_reports_change_without_rewriting_not_found_config() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        let before = read_config(&dir).unwrap();

        assert_eq!(
            remove_interface_checked(&dir, "Missing"),
            RemoveInterfaceOutcome::NotFound
        );
        assert_eq!(read_config(&dir).unwrap(), before);
        assert!(!dir.join("config.backup").exists());

        assert_eq!(
            remove_interface_checked(&dir, "Keep"),
            RemoveInterfaceOutcome::Removed
        );
        assert_eq!(count_header(&read_config(&dir).unwrap(), "Keep"), 0);
    }

    #[test]
    fn checked_interface_remove_distinguishes_absence_from_io_failure() {
        let missing = temp_config_dir().join("missing");
        assert_eq!(
            remove_interface_checked(&missing, "Keep"),
            RemoveInterfaceOutcome::NotFound
        );

        let not_a_directory = temp_config_dir().join("plain-file");
        std::fs::write(&not_a_directory, "not a directory").unwrap();
        assert_eq!(
            remove_interface_checked(&not_a_directory, "Keep"),
            RemoveInterfaceOutcome::WriteFailed
        );
        assert_eq!(
            remove_interface_checked(&missing, "Unsafe]]\nName"),
            RemoveInterfaceOutcome::WriteFailed
        );

        let write_failure = temp_config_dir();
        write_base_config(&write_failure);
        std::fs::create_dir(write_failure.join("config.backup")).unwrap();
        assert_eq!(
            remove_interface_checked(&write_failure, "Keep"),
            RemoveInterfaceOutcome::WriteFailed
        );
        assert_eq!(
            count_header(&read_config(&write_failure).unwrap(), "Keep"),
            1
        );
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
    fn android_usb_selector_persists_with_exact_revision_and_optional_serial() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("USB Radio", "androidusb://legacy", 915_000_000),
        ));
        let revision = snapshot_interface_block(&dir, "USB Radio").expect("revision");
        assert_eq!(
            set_rnode_usb_selector_if_revision(
                &dir,
                &revision,
                0x1a86,
                0x7523,
                Some("stable-serial"),
            ),
            InterfaceBlockCasOutcome::Applied
        );
        let content = read_config(&dir).expect("config");
        assert!(content.contains("usb_vendor_id = 6790"));
        assert!(content.contains("usb_product_id = 29987"));
        assert!(content.contains("usb_serial_number = stable-serial"));
        assert_eq!(
            set_rnode_usb_selector_if_revision(&dir, &revision, 1, 2, None),
            InterfaceBlockCasOutcome::Stale
        );
    }

    #[test]
    fn startup_android_usb_selector_migration_is_enabled_only_and_cas_safe() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[interfaces]\n  [[Legacy USB]]\n    type = RNodeInterface\n    enabled = true\n    port = androidusb://old-path\n    frequency = 915000000\n  [[Paused USB]]\n    type = RNodeInterface\n    enabled = false\n    port = androidusb://paused-path\n",
        );
        let candidates = android_usb_selector_migration_candidates(&dir);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].name, "Legacy USB");
        assert_eq!(candidates[0].device_name, "old-path");

        assert_eq!(
            apply_android_usb_selector_migration(
                &dir,
                &candidates[0],
                0x303a,
                0x1001,
                Some("stable-radio"),
            ),
            InterfaceBlockCasOutcome::Applied
        );
        let persisted = read_config(&dir).expect("config");
        assert!(persisted.contains("usb_vendor_id = 12346"));
        assert!(persisted.contains("usb_product_id = 4097"));
        assert!(persisted.contains("usb_serial_number = stable-radio"));
        assert!(!persisted.contains("paused-path\n    usb_vendor_id"));

        let stable = android_usb_selector_migration_candidates(&dir);
        assert_eq!(stable[0].vendor_id, Some(0x303a));
        assert_eq!(stable[0].product_id, Some(0x1001));
        assert_eq!(stable[0].serial_number.as_deref(), Some("stable-radio"));

        assert!(set_interface_enabled(&dir, "Legacy USB", false));
        assert_eq!(
            apply_android_usb_selector_migration(
                &dir,
                &stable[0],
                0x303a,
                0x1001,
                Some("replacement"),
            ),
            InterfaceBlockCasOutcome::Stale,
            "a concurrent edit must never be overwritten by startup migration"
        );
        let concurrent = read_config(&dir).expect("concurrent config");
        assert!(concurrent.contains("enabled = false"));
        assert!(concurrent.contains("usb_serial_number = stable-radio"));
        assert!(!concurrent.contains("usb_serial_number = replacement"));
    }

    #[test]
    fn update_rnode_replaces_or_removes_android_usb_selector_as_one_unit() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface_with_usb_selector(
            &dir,
            test_rnode_args("USB Radio", "androidusb://first", 915_000_000),
            Some(RnodeUsbSelectorArgs {
                vendor_id: 0x1a86,
                product_id: 0x7523,
                serial_number: Some("radio-a"),
            }),
        ));

        assert!(update_rnode_interface_with_usb_selector(
            &dir,
            "USB Radio",
            test_rnode_args("USB Radio", "androidusb://second", 917_000_000),
            Some(RnodeUsbSelectorArgs {
                vendor_id: 0x303a,
                product_id: 0x1001,
                serial_number: Some("radio-b"),
            }),
        ));
        let replaced = read_config(&dir).expect("config");
        assert!(replaced.contains("usb_vendor_id = 12346"));
        assert!(replaced.contains("usb_product_id = 4097"));
        assert!(replaced.contains("usb_serial_number = radio-b"));
        assert!(!replaced.contains("radio-a"));

        assert!(update_rnode_interface(
            &dir,
            "USB Radio",
            test_rnode_args("BLE Radio", "ble://00:11:22:33:44:55", 917_000_000),
        ));
        let switched = read_config(&dir).expect("config");
        assert!(!switched.contains("usb_vendor_id"));
        assert!(!switched.contains("usb_product_id"));
        assert!(!switched.contains("usb_serial_number"));
    }

    #[test]
    fn enabled_ble_rnode_inventory_is_ordered_and_ignores_paused_rows() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("First BLE", "ble://00:11:22:33:44:55", 915_000_000),
        ));
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Paused BLE", "ble://11:22:33:44:55:66", 916_000_000),
        ));
        assert!(set_interface_enabled(&dir, "Paused BLE", false));
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Second BLE", "ble://22:33:44:55:66:77", 917_000_000),
        ));
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("USB", "androidusb://opaque", 918_000_000),
        ));

        assert_eq!(
            enabled_rnode_names_with_port_prefix(&dir, "ble://"),
            vec!["First BLE".to_string(), "Second BLE".to_string()]
        );
    }

    fn test_rnode_args<'a>(name: &'a str, port: &'a str, frequency: u64) -> RnodeInterfaceArgs<'a> {
        RnodeInterfaceArgs {
            name,
            port,
            mode: Some("full"),
            frequency,
            bandwidth: 125_000,
            spreading_factor: 7,
            coding_rate: 5,
            tx_power: 17,
            region_key: Some("americas"),
            preset_key: None,
            airtime_limit_short: None,
            airtime_limit_long: None,
            public_map: RnodePublicMapArgs::default(),
        }
    }

    #[test]
    fn update_rnode_rejects_rename_collision_without_changing_config() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Existing Radio", "/dev/ttyUSB1", 917_000_000),
        ));
        std::fs::remove_file(dir.join("config.backup")).unwrap();
        let before = read_config(&dir).unwrap();

        assert!(!update_rnode_interface(
            &dir,
            "Radio",
            test_rnode_args("Existing Radio", "/dev/ttyUSB2", 919_000_000),
        ));

        let after = read_config(&dir).unwrap();
        assert_eq!(after, before);
        assert!(!dir.join("config.backup").exists());
        assert_eq!(count_header(&after, "Radio"), 1);
        assert_eq!(count_header(&after, "Existing Radio"), 1);
        assert!(after.contains("port = /dev/ttyUSB0"));
        assert!(after.contains("port = /dev/ttyUSB1"));
        assert!(!after.contains("port = /dev/ttyUSB2"));
    }

    #[test]
    fn update_rnode_same_name_still_replaces_its_own_block() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));

        assert!(update_rnode_interface(
            &dir,
            "Radio",
            test_rnode_args("Radio", "/dev/ttyUSB1", 917_000_000),
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 1);
        assert!(content.contains("port = /dev/ttyUSB1"));
        assert!(content.contains("frequency = 917000000"));
        assert!(!content.contains("port = /dev/ttyUSB0"));
    }

    #[test]
    fn interface_block_cas_restores_only_the_expected_rnode_block() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let previous = snapshot_interface_block(&dir, "Radio").unwrap();

        assert!(update_rnode_interface(
            &dir,
            "Radio",
            test_rnode_args("Field Radio", "/dev/ttyUSB1", 917_000_000),
        ));
        let expected_current = snapshot_interface_block(&dir, "Field Radio").unwrap();

        let with_unrelated_edit = read_config(&dir)
            .unwrap()
            .replace("loglevel = 3", "loglevel = 5");
        assert!(write_config(&dir, &with_unrelated_edit));
        assert_eq!(
            restore_interface_block_if_revision(&dir, &expected_current, &previous),
            InterfaceBlockCasOutcome::Applied
        );

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 1);
        assert_eq!(count_header(&content, "Field Radio"), 0);
        assert!(content.contains("port = /dev/ttyUSB0"));
        assert!(content.contains("loglevel = 5"));
    }

    #[test]
    fn interface_block_cas_rejects_a_stale_same_name_edit() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let previous = snapshot_interface_block(&dir, "Radio").unwrap();
        assert!(update_rnode_interface(
            &dir,
            "Radio",
            test_rnode_args("Field Radio", "/dev/ttyUSB1", 917_000_000),
        ));
        let expected_current = snapshot_interface_block(&dir, "Field Radio").unwrap();

        assert!(set_interface_enabled(&dir, "Field Radio", false));
        assert_eq!(
            restore_interface_block_if_revision(&dir, &expected_current, &previous),
            InterfaceBlockCasOutcome::Stale
        );
        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 0);
        assert_eq!(count_header(&content, "Field Radio"), 1);
        assert!(content.contains("enabled = false"));
    }

    #[test]
    fn interface_block_cas_rejects_a_rename_collision() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let previous = snapshot_interface_block(&dir, "Radio").unwrap();
        assert!(update_rnode_interface(
            &dir,
            "Radio",
            test_rnode_args("Field Radio", "/dev/ttyUSB1", 917_000_000),
        ));
        let expected_current = snapshot_interface_block(&dir, "Field Radio").unwrap();
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB2", 919_000_000),
        ));

        assert_eq!(
            restore_interface_block_if_revision(&dir, &expected_current, &previous),
            InterfaceBlockCasOutcome::Stale
        );
        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 1);
        assert_eq!(count_header(&content, "Field Radio"), 1);
        assert!(content.contains("port = /dev/ttyUSB2"));
    }

    #[test]
    fn interface_block_cas_remove_preserves_unrelated_edits() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let expected = snapshot_interface_block(&dir, "Radio").unwrap();

        let with_unrelated_edit = read_config(&dir)
            .unwrap()
            .replace("loglevel = 3", "loglevel = 5");
        assert!(write_config(&dir, &with_unrelated_edit));
        assert_eq!(
            remove_interface_block_if_revision(&dir, &expected),
            InterfaceBlockCasOutcome::Applied
        );

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 0);
        assert_eq!(count_header(&content, "Keep"), 1);
        assert!(content.contains("loglevel = 5"));
    }

    #[test]
    fn interface_block_cas_remove_rejects_stale_same_name_edit() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let expected = snapshot_interface_block(&dir, "Radio").unwrap();
        assert!(set_interface_enabled(&dir, "Radio", false));
        let before_remove = read_config(&dir).unwrap();

        assert_eq!(
            remove_interface_block_if_revision(&dir, &expected),
            InterfaceBlockCasOutcome::Stale
        );
        assert_eq!(read_config(&dir).unwrap(), before_remove);
        assert_eq!(count_header(&before_remove, "Radio"), 1);
        assert!(before_remove.contains("enabled = false"));
    }

    #[test]
    fn interface_block_cas_conditionally_disables_the_expected_revision() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        assert!(add_rnode_interface(
            &dir,
            test_rnode_args("Radio", "/dev/ttyUSB0", 915_000_000),
        ));
        let enabled = snapshot_interface_block(&dir, "Radio").unwrap();

        assert_eq!(
            set_interface_enabled_if_revision(&dir, &enabled, false),
            InterfaceBlockCasOutcome::Applied
        );
        assert!(read_config(&dir).unwrap().contains("enabled = false"));
        assert_eq!(
            set_interface_enabled_if_revision(&dir, &enabled, true),
            InterfaceBlockCasOutcome::Stale
        );
        assert!(read_config(&dir).unwrap().contains("enabled = false"));
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
    fn new_rnode_interface_defaults_to_roaming_mode() {
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
        assert!(content.contains("mode = roaming"));
    }

    #[test]
    fn new_rnode_interface_preserves_explicit_full_mode() {
        let dir = temp_config_dir();
        write_base_config(&dir);

        assert!(add_rnode_interface(
            &dir,
            RnodeInterfaceArgs {
                name: "Explicit Full Radio",
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
    fn update_tcp_client_preserves_unmanaged_keys_in_order() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[interfaces]\n  [[Hub]]\n    type = TCPClientInterface\n    target_host = old.example\n    target_port = 4242\n    recursive_prs = true\n    enabled = true\n    announces_from_internal = false\n    frobnicate = 7\n",
        );

        assert!(update_tcp_client(&dir, "Hub", "Hub", "new.example", 4243));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Hub"), 1);
        assert!(content.contains("target_host = new.example"));
        assert!(content.contains("target_port = 4243"));
        assert!(!content.contains("old.example"));
        assert_eq!(content.matches("enabled =").count(), 1);
        let recursive = content.find("recursive_prs = true").unwrap();
        let announces = content.find("announces_from_internal = false").unwrap();
        let frobnicate = content.find("frobnicate = 7").unwrap();
        assert!(recursive < announces && announces < frobnicate);
    }

    #[test]
    fn update_rnode_keeps_hand_set_mode_and_unmanaged_keys() {
        let dir = temp_config_dir();
        write_config(
            &dir,
            "[interfaces]\n  [[Radio]]\n    type = RNodeInterface\n    port = /dev/ttyUSB0\n    mode = internal\n    frequency = 915000000\n    bandwidth = 125000\n    spreadingfactor = 7\n    codingrate = 5\n    txpower = 17\n    enabled = true\n    announces_from_internal = false\n",
        );

        // The UI dropdown only submits known modes; the command glue threads
        // the existing unrecognized mode through verbatim (cfg_rnode_mode).
        assert!(update_rnode_interface(
            &dir,
            "Radio",
            RnodeInterfaceArgs {
                name: "Radio",
                port: "/dev/ttyUSB1",
                mode: Some("internal"),
                frequency: 915_000_000,
                bandwidth: 125_000,
                spreading_factor: 7,
                coding_rate: 5,
                tx_power: 17,
                region_key: None,
                preset_key: None,
                airtime_limit_short: None,
                airtime_limit_long: None,
                public_map: RnodePublicMapArgs::default(),
            },
        ));

        let content = read_config(&dir).unwrap();
        assert_eq!(count_header(&content, "Radio"), 1);
        assert!(content.contains("mode = internal"));
        assert!(!content.contains("mode = full"));
        assert!(content.contains("port = /dev/ttyUSB1"));
        assert!(content.contains("announces_from_internal = false"));
    }

    #[test]
    fn rnode_mode_helpers_cover_exposed_modes() {
        assert_eq!(normalize_rnode_interface_mode(None), Some("roaming"));
        assert_eq!(
            rnode_interface_mode_value(None),
            Some(rns_interface::traits::InterfaceMode::Roaming)
        );
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

    #[test]
    fn rnode_mode_passthrough_keeps_unknown_values_verbatim() {
        assert_eq!(rnode_interface_mode_passthrough(None), "roaming");
        assert_eq!(rnode_interface_mode_passthrough(Some("")), "roaming");
        assert_eq!(rnode_interface_mode_passthrough(Some("gw")), "gateway");
        assert_eq!(
            rnode_interface_mode_passthrough(Some("internal")),
            "internal"
        );
        assert_eq!(
            rnode_interface_mode_passthrough(Some(" internal ")),
            "internal"
        );
    }
    /// Either key set to yes enables a row; with neither, Ratspeak's client
    /// default keeps it on.
    #[test]
    fn interface_rows_report_the_client_enable_rule() {
        let dir = temp_config_dir();
        let block = |name: &str, flags: &str| {
            format!(
                "  [[{name}]]\n    type = TCPClientInterface\n    target_host = h\n    target_port = 1\n{flags}"
            )
        };
        std::fs::write(
            dir.join("config"),
            format!(
                "[interfaces]\n{}{}{}{}",
                block("Either", "    enabled = no\n    interface_enabled = yes\n"),
                block("Legacy", "    interface_enabled = yes\n"),
                block("Off", "    enabled = no\n"),
                block("No Flag", ""),
            ),
        )
        .unwrap();
        let listing = get_all_interfaces(&dir);
        let rows = listing["tcp_client"].as_array().unwrap();
        let enabled = |name: &str| {
            let row = rows.iter().find(|row| row["name"] == name).unwrap();
            assert!(row.get("interface_enabled").is_none(), "{row}");
            interface_entry_enabled(row)
        };
        assert!(enabled("Either"));
        assert!(enabled("Legacy"));
        assert!(!enabled("Off"));
        assert!(enabled("No Flag"));
    }

    /// The runtime follows upstream and skips a block without an enable key,
    /// so startup writes the client default into Ratspeak's own config.
    #[test]
    fn startup_writes_enabled_into_interfaces_without_a_setting() {
        let dir = temp_config_dir();
        let original = "[reticulum]\n  share_instance = Yes\n\n[interfaces]\n  [[Off]]\n    type = TCPClientInterface\n    enabled = no\n  [[Legacy]]\n    type = AutoInterface\n    interface_enabled = yes\n  [[No Flag]]\n    # kept as written\n    type = TCPClientInterface\n    target_host = h\n    target_port = 1\n  [[Last]]\n    type = AutoInterface";
        std::fs::write(dir.join("config"), original).unwrap();

        assert_eq!(enable_unflagged_interfaces(&dir), Ok(2));
        let written = read_config(&dir).unwrap();
        assert_eq!(
            written.as_str(),
            original
                .replace(
                    "    type = TCPClientInterface\n    target_host = h",
                    "    type = TCPClientInterface\n    enabled = true\n    target_host = h"
                )
                .replace(
                    "  [[Last]]\n    type = AutoInterface",
                    "  [[Last]]\n    type = AutoInterface\n    enabled = true\n"
                )
        );
        assert_eq!(enable_unflagged_interfaces(&dir), Ok(0), "idempotent");

        let config = rns_runtime::config::Config::parse(&written).unwrap();
        for (name, expected) in [
            ("Off", false),
            ("Legacy", true),
            ("No Flag", true),
            ("Last", true),
        ] {
            let section = config.subsection("interfaces", name).unwrap();
            assert_eq!(section.interface_enabled(), expected, "{name}");
        }
    }

    #[test]
    fn ifac_listing_uses_authoritative_quoted_keys_before_type_and_never_migrates_on_read() {
        let dir = temp_config_dir();
        let original = "[interfaces]\n [[\"Private TCP\"]] # peer\n 'ifac_size' = 16\n network_name = mesh\n type = TCPClientInterface\n note = '''\n [[Fake]]\n ifac_size = 8\n '''\n";
        std::fs::write(dir.join("config"), original).unwrap();
        let listing = get_all_interfaces(&dir);
        let rows = listing["tcp_client"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "Private TCP");
        assert!(rows[0].get("ifac_size").is_none());
        assert_eq!(rows[0]["ifac_migration"]["bytes"], 16);
        assert_eq!(rows[0]["ifac_migration"]["python_bytes"], 2);
        assert_eq!(read_config(&dir).unwrap().as_str(), original);
    }

    #[test]
    fn ifac_explicit_migration_preserves_other_ambiguity_backup_and_detects_stale_choice() {
        use rns_runtime::config::ifac::IfacSourceUnits;
        let dir = temp_config_dir();
        let original = "[interfaces]\r\n [[\"First\"]]\r\n ifac_size = 16 # keep\r\n type = TCPClientInterface\r\n [[Second]]\r\n type = TCPClientInterface\r\n ifac_size = 64\r\n";
        std::fs::write(dir.join("config"), original).unwrap();
        let hash = rns_crypto::sha::sha256(original.as_bytes());
        assert!(migrate_ifac_units(&dir, "First", IfacSourceUnits::Bytes, &hash).unwrap());
        let updated = read_config(&dir).unwrap();
        assert!(updated.contains("ifac_size = 128 # keep\r\n"));
        assert!(updated.ends_with(" ifac_size = 64\r\n"));
        let immutable = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .find(|item| {
                item.file_name()
                    .to_string_lossy()
                    .starts_with("config.backup.ifac.")
            })
            .unwrap()
            .path();
        assert_eq!(std::fs::read_to_string(&immutable).unwrap(), original);
        assert!(migrate_ifac_units(&dir, "Second", IfacSourceUnits::Bits, &hash).is_err());
        assert_eq!(read_config(&dir).unwrap(), updated);
        let next_hash = rns_crypto::sha::sha256(updated.as_bytes());
        assert!(!migrate_ifac_units(&dir, "First", IfacSourceUnits::Bytes, &next_hash).unwrap());
        assert!(migrate_ifac_units(&dir, "Second", IfacSourceUnits::Bits, &next_hash).unwrap());
        let listing = get_all_interfaces(&dir);
        assert_eq!(listing["tcp_client"][0]["ifac_size"], "16");
        assert_eq!(listing["tcp_client"][1]["ifac_size"], "8");
        assert_eq!(std::fs::read_to_string(&immutable).unwrap(), original);
    }

    #[test]
    fn ifac_semantic_errors_block_new_writes_and_enable_but_allow_pause_and_clear() {
        let dir = temp_config_dir();
        let original = "[interfaces]\n [[Peer]]\n type = TCPClientInterface\n target_host = peer\n target_port = 4242\n ifac_size = 16\n";
        assert!(!write_config(&dir, original));
        std::fs::write(dir.join("config"), original).unwrap();
        assert!(set_interface_enabled(&dir, "Peer", false));
        let paused = read_config(&dir).unwrap();
        assert!(!set_interface_enabled(&dir, "Peer", true));
        assert_eq!(read_config(&dir).unwrap(), paused);
        assert!(update_tcp_client_with_ifac(
            &dir,
            "Peer",
            "Peer",
            "peer",
            4242,
            InterfaceIfacArgs::default()
        ));
        assert!(!read_config(&dir).unwrap().contains("ifac_size"));
    }

    #[test]
    fn ifac_all_editor_families_preserve_bytes_then_clear_quoted_escape_keys() {
        for kind in [
            "TCPClientInterface",
            "TCPServerInterface",
            "BackboneInterface",
        ] {
            for server in [false, true] {
                if kind != "BackboneInterface" && server {
                    continue;
                }
                for bytes in [8, 16] {
                    let dir = temp_config_dir();
                    let original = format!(
                        "[interfaces]\n [['Old Peer']]\n 'ifac_size_bytes' = {bytes}\n network_name = mesh\n type = {kind}\n custom_note = '''\n [[Not a section]]\n 'ifac_size_unit' = not a declaration\n '''\n"
                    );
                    std::fs::write(dir.join("config"), original).unwrap();
                    for (old, new, size) in [
                        ("Old Peer", "New Peer", Some(bytes)),
                        ("New Peer", "New Peer", Some(bytes)),
                        ("New Peer", "New Peer", None),
                    ] {
                        let ifac = InterfaceIfacArgs {
                            network_name: size.map(|_| "mesh"),
                            passphrase: None,
                            ifac_size: size,
                        };
                        let ok = match (kind, server) {
                            ("TCPClientInterface", _) => {
                                update_tcp_client_with_ifac(&dir, old, new, "peer", 4242, ifac)
                            }
                            ("TCPServerInterface", _) => update_tcp_server_with_ifac(
                                &dir,
                                old,
                                new,
                                4242,
                                "0.0.0.0",
                                ifac,
                                HostDiscoveryArgs::default(),
                            ),
                            (_, true) => update_backbone_server_with_ifac(
                                &dir,
                                old,
                                new,
                                4242,
                                "0.0.0.0",
                                false,
                                None,
                                ifac,
                                HostDiscoveryArgs::default(),
                            ),
                            _ => update_backbone_client(
                                &dir,
                                old,
                                BackboneClientArgs {
                                    name: new,
                                    host: "peer",
                                    port: 4242,
                                    prefer_ipv6: false,
                                    connect_timeout: None,
                                    max_reconnect_tries: None,
                                    i2p_tunneled: false,
                                    ifac,
                                },
                            ),
                        };
                        assert!(ok, "{kind} server={server} size={size:?}");
                        let content = read_config(&dir).unwrap();
                        assert!(content.contains("custom_note = '''\n [[Not a section]]\n 'ifac_size_unit' = not a declaration\n '''"));
                        let parsed = rns_runtime::config::Config::parse(&content).unwrap();
                        let section = parsed.subsection("interfaces", new).unwrap();
                        assert_eq!(
                            rns_runtime::config::ifac::parse_ifac_size(section).unwrap(),
                            size
                        );
                        assert!(!section.has("ifac_size_bytes"));
                        assert_eq!(section.has("ifac_size_unit"), size.is_some());
                    }
                }
            }
        }
    }

    #[test]
    fn ifac_migration_lock_and_snapshot_prevent_competing_publication() {
        let dir = temp_config_dir();
        let source = "[interfaces]\n [[Peer]]\n type = TCPClientInterface\n ifac_size = 16\n";
        std::fs::write(dir.join("config"), source).unwrap();
        let lock =
            rns_runtime::config::ifac_lock::ConfigWriteLock::acquire(&dir.join("config")).unwrap();
        assert!(
            migrate_ifac_units(
                &dir,
                "Peer",
                rns_runtime::config::ifac::IfacSourceUnits::Bytes,
                &rns_crypto::sha::sha256(source.as_bytes())
            )
            .is_err()
        );
        assert_eq!(read_config(&dir).unwrap().as_str(), source);
        drop(lock);
        std::fs::write(dir.join("config"), format!("{source}# changed\n")).unwrap();
        assert!(write_config_if_unchanged(&dir, Some(source), "[interfaces]\n").is_err());
    }

    #[test]
    fn ifac_duplicate_syntax_is_not_a_last_wins_interface_list() {
        let dir = temp_config_dir();
        let content = "[interfaces]\n [[Peer]]\n type = TCPClientInterface\n ifac_size = 128\n 'ifac_size' = 64\n";
        std::fs::write(dir.join("config"), content).unwrap();
        let listing = get_all_interfaces(&dir);
        assert!(listing.get("config_error").is_some());
        assert_eq!(listing["tcp_client"].as_array().unwrap().len(), 0);
        assert!(!set_interface_enabled(&dir, "Peer", true));
        assert_eq!(read_config(&dir).unwrap().as_str(), content);
    }

    #[test]
    fn host_discovery_keys_are_written_when_on_and_absent_when_off() {
        let dir = temp_config_dir();
        write_base_config(&dir);
        let on = HostDiscoveryArgs {
            discoverable: true,
            reachable_on: Some("relay.example.org"),
            publish_ifac: true,
            discovery_encrypt: true,
        };
        let ifac = InterfaceIfacArgs {
            network_name: Some("mesh"),
            passphrase: Some("secret"),
            ifac_size: None,
        };
        assert!(add_tcp_server_with_ifac(
            &dir, "Host", 4242, "0.0.0.0", ifac, on
        ));
        assert!(add_backbone_server_with_ifac(
            &dir, "Spine", 4343, "0.0.0.0", false, None, ifac, on
        ));
        let content = read_config(&dir).unwrap();
        let parsed = rns_runtime::config::Config::parse(&content).unwrap();
        for name in ["Host", "Spine"] {
            let section = parsed.subsection("interfaces", name).unwrap();
            for key in ["discoverable", "publish_ifac", "discovery_encrypt"] {
                assert_eq!(section.get_bool(key), Some(true), "{name} {key}");
            }
            assert_eq!(section.get("reachable_on"), Some("relay.example.org"));
        }
        let listing = get_all_interfaces(&dir);
        for group in ["tcp_server", "backbone_server"] {
            let entry = &listing[group][0];
            assert_eq!(entry["discoverable"], "yes", "{group}");
            assert_eq!(entry["reachable_on"], "relay.example.org", "{group}");
            assert_eq!(entry["publish_ifac"], "yes", "{group}");
            assert_eq!(entry["discovery_encrypt"], "yes", "{group}");
        }

        // An update owns the keys: off drops them, and an alias goes with them.
        let edited = read_config(&dir).unwrap().replace(
            "    discovery_encrypt = yes\n",
            "    discovery_encrypt = yes\n    discovery_publish_ifac = yes\n    gravity = 2\n",
        );
        write_config(&dir, &edited);
        assert!(update_tcp_server_with_ifac(
            &dir,
            "Host",
            "Host",
            4242,
            "0.0.0.0",
            ifac,
            HostDiscoveryArgs::default(),
        ));
        assert!(update_backbone_server_with_ifac(
            &dir,
            "Spine",
            "Spine",
            4343,
            "0.0.0.0",
            false,
            None,
            ifac,
            HostDiscoveryArgs::default(),
        ));
        let content = read_config(&dir).unwrap();
        for key in [
            "discoverable",
            "reachable_on",
            "publish_ifac",
            "discovery_publish_ifac",
            "discovery_encrypt",
        ] {
            assert!(!content.contains(&format!("{key} =")), "{key}");
        }
        // Keys it does not own stay in each block.
        assert_eq!(content.matches("gravity = 2").count(), 2);
        let listing = get_all_interfaces(&dir);
        for group in ["tcp_server", "backbone_server"] {
            let entry = listing[group][0].as_object().unwrap();
            assert!(!entry.contains_key("discoverable"), "{group}");
            assert!(!entry.contains_key("discovery_encrypt"), "{group}");
        }
        // Off by default, as the plain writers are.
        assert!(add_tcp_server(&dir, "Plain", 4444, "0.0.0.0"));
        assert!(!read_config(&dir).unwrap().contains("discoverable"));
        // A reachable address is one config line.
        let bad = HostDiscoveryArgs {
            reachable_on: Some("relay.example.org # hidden"),
            ..on
        };
        assert!(!add_tcp_server_with_ifac(
            &dir, "Bad", 4545, "0.0.0.0", ifac, bad
        ));
    }

    #[test]
    fn the_network_identity_key_names_the_profile_file() {
        // A comma or `#` in the profile path must not split or cut the value.
        let dir = temp_config_dir().join("Name, Jr #2");
        write_base_config(&dir);
        let file = network_identity_path(&dir);
        assert!(file.ends_with("storage/identities/network"));
        assert_eq!(configured_network_identity_path(&dir), None);
        assert!(!apply_network_identity(&dir).unwrap());

        assert!(set_network_identity_key(&dir, true).unwrap());
        assert!(!set_network_identity_key(&dir, true).unwrap());
        let content = read_config(&dir).unwrap();
        assert_eq!(
            reticulum_value(&content, "network_identity"),
            Some(format!("\"{}\"", file.display()).as_str())
        );
        // Reticulum reads the same file back.
        assert_eq!(configured_network_identity_path(&dir), Some(file.clone()));
        assert!(load_network_identity(&dir).is_none());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let identity = rns_identity::identity::Identity::new();
        identity.to_file(&file).unwrap();
        assert_eq!(load_network_identity(&dir).unwrap().hash, identity.hash);

        assert!(set_network_identity_key(&dir, false).unwrap());
        assert_eq!(
            reticulum_value(&read_config(&dir).unwrap(), "network_identity"),
            None
        );
        // At start, an existing file gets its key back.
        assert!(apply_network_identity(&dir).unwrap());
        assert_eq!(configured_network_identity_path(&dir), Some(file.clone()));

        // A key naming another file is left as written while the profile has none.
        std::fs::remove_file(&file).unwrap();
        let other = dir.join("elsewhere");
        let hand = read_config(&dir).unwrap().replace(
            &format!("\"{}\"", file.display()),
            &format!("'{}'", other.display()),
        );
        write_config(&dir, &hand);
        assert!(!apply_network_identity(&dir).unwrap());
        assert_eq!(configured_network_identity_path(&dir), Some(other));
    }

    #[test]
    fn trusted_publishers_always_accept_this_devices_own_network() {
        use ratspeak_core::types::NetworkSettings;
        let dir = temp_config_dir();
        write_base_config(&dir);
        let file = network_identity_path(&dir);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let identity = rns_identity::identity::Identity::new();
        identity.to_file(&file).unwrap();
        assert!(set_network_identity_key(&dir, true).unwrap());
        let sources = |dir: &Path| {
            let cfg = rns_runtime::config::Config::parse(&read_config(dir).unwrap()).unwrap();
            let rc = rns_runtime::reticulum::ReticulumConfig::try_from_config(&cfg).unwrap();
            (rc.discover_interfaces, rc.interface_discovery_sources)
        };

        let trusted = NetworkSettings {
            discover: true,
            trusted_only: true,
            trusted_publishers: vec!["a".repeat(32)],
            ..NetworkSettings::default()
        };
        assert!(apply_network_settings(&dir, &trusted).unwrap());
        assert_eq!(sources(&dir), (true, vec![[0xaa; 16], identity.hash]));
        // Read back, the trusted list is the user's alone.
        assert_eq!(network_settings_from_config(&dir).unwrap(), trusted);

        // Trusting no one else still accepts the own network, and the setting survives a read.
        let nobody = NetworkSettings {
            trusted_publishers: vec![],
            ..trusted.clone()
        };
        apply_network_settings(&dir, &nobody).unwrap();
        assert_eq!(sources(&dir), (true, vec![identity.hash]));
        assert_eq!(network_settings_from_config(&dir).unwrap(), nobody);

        // Anyone needs no list; without a network identity the list is the trusted one.
        apply_network_settings(
            &dir,
            &NetworkSettings {
                trusted_only: false,
                ..trusted.clone()
            },
        )
        .unwrap();
        assert_eq!(sources(&dir), (true, vec![]));
        assert!(set_network_identity_key(&dir, false).unwrap());
        apply_network_settings(&dir, &trusted).unwrap();
        assert_eq!(sources(&dir), (true, vec![[0xaa; 16]]));
    }
}
