//! Conversation list builder + broadcast helpers. Lives in runtime because
//! receive-path code (`handle_inbound_lxmf`, link-delivery, etc.) needs to
//! kick a `conversations_update` emit. The IPC command in
//! `ratspeak-tauri/commands/messaging.rs` wraps `build_conversations_payload`
//! so the WebView can call it as `api_lxmf_conversations`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::db;
use crate::helpers::active_identity_id;
use crate::state::AppState;

/// `Some(payload)` on success; `None` on any DB / timeout failure (already
/// logged). The Tauri command wraps this into an `AppError`.
pub async fn build_conversations_payload(state: &AppState) -> Option<Value> {
    let identity_id = active_identity_id(state);

    let lxmf_hash = state
        .lxmf
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|m| m.lxmf_hash.clone()))
        .unwrap_or_default();

    let announce_names: std::collections::HashMap<String, String> = state
        .announce_history
        .read()
        .map(|announces| {
            announces
                .values()
                .filter_map(|a| {
                    let hash = a.get("hash").and_then(|h| h.as_str())?.to_string();
                    let name = a
                        .get("display_name")
                        .or_else(|| a.get("aspect"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() {
                        None
                    } else {
                        Some((hash, name))
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let id_for_db = identity_id.clone();
    let lxmf_hash_for_db = lxmf_hash.clone();
    let fetch = db::spawn_db(state.db.clone(), move |pool| {
        let conn = pool.get().map_err(|_| "pool unavailable".to_string())?;

        // One row per conversation (latest message); preview trimmed in SQL.
        let mut stmt = conn
            .prepare(
                "SELECT m.source,
                        m.destination,
                        substr(m.content, 1, 60) AS preview,
                        m.timestamp,
                        m.direction
                 FROM messages m
                 WHERE m.identity_id = ?1
                   AND m.rowid IN (
                       SELECT rowid FROM (
                           SELECT rowid,
                                  ROW_NUMBER() OVER (
                                      PARTITION BY CASE WHEN direction = 'inbound' THEN source ELSE destination END
                                      ORDER BY timestamp DESC
                                  ) AS rn
                           FROM messages
                           WHERE identity_id = ?1
                       ) WHERE rn = 1
                   )
                   AND CASE WHEN m.direction = 'inbound' THEN m.source ELSE m.destination END
                       NOT IN (SELECT dest_hash FROM hidden_conversations WHERE identity_id = ?1)
                   AND CASE WHEN m.direction = 'inbound' THEN m.source ELSE m.destination END
                       NOT IN (SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?1)
                 ORDER BY m.timestamp DESC",
            )
            .map_err(|e| format!("prepare messages failed: {e}"))?;

        let contact_lookup: std::collections::HashMap<String, String> = conn
            .prepare("SELECT dest_hash, display_name FROM contacts WHERE identity_id = ?1")
            .ok()
            .and_then(|mut s| {
                s.query_map(rusqlite::params![id_for_db], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1).unwrap_or_default(),
                    ))
                })
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();

        let unread_counts: std::collections::HashMap<String, i64> = conn
            .prepare(
                "SELECT source, COUNT(*) FROM messages
                 WHERE direction = 'inbound' AND state != 'read' AND identity_id = ?1
                 GROUP BY source",
            )
            .ok()
            .and_then(|mut s| {
                s.query_map(rusqlite::params![id_for_db], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
            })
            .unwrap_or_default();

        let mut conversations: std::collections::HashMap<String, Value> =
            std::collections::HashMap::new();
        let mut activity_name_stmt = conn
            .prepare(
                "SELECT display_name
                 FROM identity_activity
                 WHERE dest_hash = ?1 AND COALESCE(display_name, '') != ''",
            )
            .ok();

        if let Ok(rows) = stmt.query_map(rusqlite::params![id_for_db], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)
                    .unwrap_or_else(|_| "outbound".into()),
            ))
        }) {
            for r in rows.flatten() {
                let (source, destination, preview, timestamp, direction) = r;
                let other = if direction == "inbound" {
                    source.clone()
                } else {
                    destination.clone()
                };
                // Self-message: pick the opposite side as the remote party.
                let other = if other == lxmf_hash_for_db {
                    if source != lxmf_hash_for_db {
                        source.clone()
                    } else {
                        destination.clone()
                    }
                } else {
                    other
                };

                if !conversations.contains_key(&other) {
                    let is_contact = contact_lookup.contains_key(&other);
                    let display_name = contact_lookup
                        .get(&other)
                        .cloned()
                        .filter(|s| !s.is_empty())
                        .or_else(|| announce_names.get(&other).cloned())
                        .or_else(|| {
                            activity_name_stmt.as_mut().and_then(|stmt| {
                                stmt.query_row(rusqlite::params![&other], |row| {
                                    row.get::<_, String>(0)
                                })
                                .ok()
                                .filter(|s| !s.is_empty())
                            })
                        });

                    conversations.insert(
                        other.clone(),
                        json!({
                            "hash": other,
                            "display_name": display_name,
                            "last_message": preview,
                            "last_direction": direction,
                            "timestamp": timestamp,
                            "unread": unread_counts.get(&other).copied().unwrap_or(0),
                            "is_contact": is_contact,
                        }),
                    );
                }
            }
        }

        Ok::<_, String>(conversations)
    });

    let conversations = match tokio::time::timeout(Duration::from_secs(5), fetch).await {
        Ok(Ok(Ok(c))) => c,
        Ok(Ok(Err(e))) => {
            tracing::warn!(?e, "build_conversations_payload db query failed");
            return None;
        }
        Ok(Err(e)) => {
            tracing::warn!(?e, "build_conversations_payload db task panicked");
            return None;
        }
        Err(_) => {
            tracing::warn!("build_conversations_payload timed out after 5s");
            return None;
        }
    };

    let mut result: Vec<Value> = conversations.into_values().collect();
    result.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let tb = b.get("timestamp").and_then(|v| v.as_f64()).unwrap_or(0.0);
        tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
    });

    Some(json!(result))
}

/// Coalesced fire-and-forget conversations broadcast (100ms debounce).
pub fn broadcast_conversations(state: Arc<AppState>) {
    use std::sync::atomic::Ordering;
    if state
        .conversations_broadcast_pending
        .swap(true, Ordering::AcqRel)
    {
        return;
    }
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        state
            .conversations_broadcast_pending
            .store(false, Ordering::Release);
        if let Some(payload) = build_conversations_payload(&state).await {
            state.emit_to_all("conversations_update", payload);
        }
    });
}

/// Awaiting variant; same coalescing flag.
pub async fn broadcast_conversations_now(state: &AppState) {
    use std::sync::atomic::Ordering;
    if state
        .conversations_broadcast_pending
        .swap(true, Ordering::AcqRel)
    {
        return;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    state
        .conversations_broadcast_pending
        .store(false, Ordering::Release);
    if let Some(payload) = build_conversations_payload(state).await {
        state.emit_to_all("conversations_update", payload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DashboardConfig;
    use r2d2_sqlite::SqliteConnectionManager;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_STATE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_state() -> AppState {
        let unique = TEMP_STATE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "ratspeak-runtime-msg-test-{}-{}-{unique}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = DashboardConfig::from_env_and_defaults(tmp);
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(2).build(mgr).unwrap();
        crate::db::init_schema(&pool).unwrap();
        AppState::new(
            config,
            pool,
            Arc::new(ratspeak_core::NoopEmitter),
            Arc::new(ratspeak_core::NoopNotifier),
        )
    }

    #[tokio::test]
    async fn conversations_use_persisted_activity_name_for_non_contacts() {
        let state = make_state();
        crate::db::save_identity(
            &state.db,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "me",
            "Me",
        );
        crate::db::set_active_identity(&state.db, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let peer = "11111111111111111111111111111111";
        {
            let conn = state.db.get().unwrap();
            conn.execute(
                "INSERT INTO messages
                    (id, source, destination, content, timestamp, state, direction, identity_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    "msg-1",
                    peer,
                    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "hello",
                    100.0,
                    "delivered",
                    "inbound",
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ],
            )
            .unwrap();
        }
        crate::db::touch_identity_activity(
            &state.db,
            &[(peer.to_string(), 90.0, Some("RatDeck".to_string()), None)],
        );

        let payload = build_conversations_payload(&state).await.unwrap();
        let rows = payload.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("display_name").and_then(|v| v.as_str()),
            Some("RatDeck")
        );
        assert_eq!(
            rows[0].get("is_contact").and_then(|v| v.as_bool()),
            Some(false)
        );
    }
}
