use crate::db;
use crate::state::AppState;

pub const ANNOUNCED_DISPLAY_NAME_MAX_BYTES: usize = 128;

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
    if trimmed.as_bytes().len() > ANNOUNCED_DISPLAY_NAME_MAX_BYTES {
        return Err(format!(
            "Display name must be {ANNOUNCED_DISPLAY_NAME_MAX_BYTES} UTF-8 bytes or less"
        ));
    }
    Ok(trimmed.to_string())
}

pub fn active_identity_id(state: &AppState) -> String {
    db::get_active_identity(&state.db)
        .and_then(|id| id.get("hash").and_then(|h| h.as_str()).map(String::from))
        .unwrap_or_default()
}

pub fn active_lxmf_hash(state: &AppState) -> String {
    db::get_active_identity(&state.db)
        .and_then(|id| {
            id.get("lxmf_hash")
                .and_then(|h| h.as_str())
                .map(String::from)
        })
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
}
