use crate::db;
use crate::state::AppState;

pub const ANNOUNCED_DISPLAY_NAME_MAX_BYTES: usize = 128;
pub const ANNOUNCED_STATUS_MAX_BYTES: usize = 50;

pub fn validate_hex(value: &str, min_len: usize, max_len: usize) -> bool {
    if value.len() < min_len || value.len() > max_len {
        return false;
    }
    value.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn sanitize_text(value: &str, max_len: usize) -> String {
    value
        .chars()
        .take(max_len)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn sanitize_announced_display_name(value: &str) -> Result<String, String> {
    let sanitized = value.replace('\0', "");
    let trimmed = sanitized.trim();
    if trimmed.len() > ANNOUNCED_DISPLAY_NAME_MAX_BYTES {
        return Err(format!(
            "Display name must be {ANNOUNCED_DISPLAY_NAME_MAX_BYTES} UTF-8 bytes or less"
        ));
    }
    Ok(trimmed.to_string())
}

pub fn sanitize_announced_status(value: &str) -> Result<String, String> {
    let sanitized = value.replace('\0', "").replace(['\r', '\n', '\t'], " ");
    let trimmed = sanitized.trim();
    if trimmed.len() > ANNOUNCED_STATUS_MAX_BYTES {
        return Err(format!(
            "Status must be {ANNOUNCED_STATUS_MAX_BYTES} UTF-8 bytes or less"
        ));
    }
    Ok(trimmed.to_string())
}

/// Active identity's (hash, lxmf_hash) via the generation-stamped cache.
/// Hot async paths call this every poll/announce/message; the sync DB read
/// only happens when an identity-table write bumped `db::identity_generation`.
pub fn active_identity_snapshot(state: &AppState) -> Option<(String, String)> {
    let generation = db::identity_generation();
    if let Ok(cache) = state.active_identity_cache.lock()
        && let Some((cached_generation, snapshot)) = cache.as_ref()
        && *cached_generation == generation
    {
        return snapshot.clone();
    }

    let snapshot = db::get_active_identity(&state.db).and_then(|id| {
        let hash = id.get("hash")?.as_str()?.to_string();
        let lxmf_hash = id
            .get("lxmf_hash")
            .and_then(|h| h.as_str())
            .unwrap_or_default()
            .to_string();
        Some((hash, lxmf_hash))
    });
    if let Ok(mut cache) = state.active_identity_cache.lock() {
        *cache = Some((generation, snapshot.clone()));
    }
    snapshot
}

pub fn active_identity_id(state: &AppState) -> String {
    active_identity_snapshot(state)
        .map(|(hash, _)| hash)
        .unwrap_or_default()
}

pub fn active_lxmf_hash(state: &AppState) -> String {
    active_identity_snapshot(state)
        .map(|(_, lxmf_hash)| lxmf_hash)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announced_display_name_limit_counts_utf8_bytes() {
        assert!(
            sanitize_announced_display_name(&"a".repeat(ANNOUNCED_DISPLAY_NAME_MAX_BYTES)).is_ok()
        );
        assert!(
            sanitize_announced_display_name(&"😀".repeat(ANNOUNCED_DISPLAY_NAME_MAX_BYTES / 4))
                .is_ok()
        );
        assert!(sanitize_announced_display_name(&"😀".repeat(33)).is_err());
    }

    #[test]
    fn announced_status_limit_counts_utf8_bytes_and_allows_empty() {
        assert!(sanitize_announced_status("").is_ok());
        assert!(sanitize_announced_status(&"a".repeat(ANNOUNCED_STATUS_MAX_BYTES)).is_ok());
        assert!(sanitize_announced_status(&"😀".repeat(ANNOUNCED_STATUS_MAX_BYTES / 4)).is_ok());
        assert!(sanitize_announced_status(&"😀".repeat(13)).is_err());
    }

    #[test]
    fn announced_status_is_single_line_metadata() {
        assert_eq!(
            sanitize_announced_status("  hello\nthere\tfriend\0  ").unwrap(),
            "hello there friend"
        );
    }
}
