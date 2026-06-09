use std::fs::{File, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent_policy::{AccessMode, dest_hash_from_conversation_id};
use crate::error::{CliError, CliResult};

const EVENT_LOG_FILE: &str = "ratspeakd-events.jsonl";
const EVENT_CURSOR_FILE: &str = "ratspeakd-events.cursor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub id: u64,
    pub created_at_unix: f64,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub payload: Value,
}

#[derive(Debug)]
pub struct EventStore {
    data_dir: PathBuf,
    cursor: Mutex<u64>,
    write_lock: Mutex<()>,
}

impl EventStore {
    pub fn open(data_root: PathBuf) -> CliResult<Arc<Self>> {
        let data_dir = data_root.join(".ratspeak");
        std::fs::create_dir_all(&data_dir)?;
        restrict_dir_permissions(&data_dir)?;
        let cursor = read_cursor(&data_dir)?;
        Ok(Arc::new(Self {
            data_dir,
            cursor: Mutex::new(cursor),
            write_lock: Mutex::new(()),
        }))
    }

    pub fn append_emitter_event(&self, event: &str, payload: Value) -> CliResult<EventRecord> {
        self.append(
            "runtime_event",
            Some(event.to_string()),
            infer_identity_id(&payload),
            infer_subject_hash(event, &payload),
            infer_message_id(event, &payload),
            payload,
        )
    }

    pub fn append_notification(&self, payload: Value) -> CliResult<EventRecord> {
        self.append(
            "notification",
            None,
            None,
            infer_notification_subject(&payload),
            None,
            payload,
        )
    }

    pub fn append_daemon_event(
        data_dir: &Path,
        event: &str,
        payload: Value,
    ) -> CliResult<EventRecord> {
        let data_root = data_dir
            .parent()
            .ok_or_else(|| CliError::failed("invalid Ratspeak data directory"))?
            .to_path_buf();
        let store = Self::open(data_root)?;
        store.append("daemon", Some(event.to_string()), None, None, None, payload)
    }

    fn append(
        &self,
        kind: impl Into<String>,
        event: Option<String>,
        identity_id: Option<String>,
        subject_hash: Option<String>,
        message_id: Option<String>,
        payload: Value,
    ) -> CliResult<EventRecord> {
        let _write_guard = self
            .write_lock
            .lock()
            .map_err(|_| CliError::failed("event store lock poisoned"))?;
        let mut cursor = self
            .cursor
            .lock()
            .map_err(|_| CliError::failed("event cursor lock poisoned"))?;
        *cursor += 1;
        let record = EventRecord {
            id: *cursor,
            created_at_unix: unix_now_secs(),
            kind: kind.into(),
            event,
            identity_id,
            subject_hash,
            message_id,
            payload,
        };
        let line = serde_json::to_string(&record)?;
        let log_path = event_log_path(&self.data_dir);
        let existed = log_path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        if !existed {
            restrict_file_permissions(&log_path)?;
        }
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()?;
        write_cursor(&self.data_dir, *cursor)?;
        Ok(record)
    }
}

pub fn event_log_path(data_dir: &Path) -> PathBuf {
    data_dir.join(EVENT_LOG_FILE)
}

pub fn read_events(
    data_dir: &Path,
    after_id: u64,
    limit: usize,
    access: &AccessMode,
) -> CliResult<Vec<EventRecord>> {
    let path = event_log_path(data_dir);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };

    let mut records = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let mut record: EventRecord = serde_json::from_str(&line)?;
        if record.id <= after_id {
            continue;
        }
        if !event_allowed(access, &record) {
            continue;
        }
        sanitize_record_for_access(access, &mut record);
        records.push(record);
        if records.len() >= limit {
            break;
        }
    }
    Ok(records)
}

pub fn latest_event_id(data_dir: &Path) -> CliResult<u64> {
    read_cursor(data_dir)
}

fn event_allowed(access: &AccessMode, record: &EventRecord) -> bool {
    let Some(principal) = access.principal() else {
        return true;
    };
    match record.kind.as_str() {
        "daemon" => principal.has_scope("status:read") || principal.has_scope("events:read"),
        "notification" => {
            principal.has_scope("events:read")
                && record
                    .subject_hash
                    .as_deref()
                    .is_some_and(|hash| principal.allows_subject(hash))
        }
        "runtime_event" => match record.event.as_deref().unwrap_or_default() {
            "lxmf_message" => {
                principal.has_scope("messages:read")
                    && record
                        .subject_hash
                        .as_deref()
                        .is_some_and(|hash| principal.allows_subject(hash))
            }
            "lxmf_delivery_progress" | "lxmf_step" => {
                principal.has_scope("messages:read")
                    && record
                        .subject_hash
                        .as_deref()
                        .is_none_or(|hash| principal.allows_subject(hash))
            }
            "unread_total" | "system_status" => principal.has_scope("status:read"),
            "stats_update" | "propagation_update" => principal.has_scope("network:read"),
            _ => principal.has_scope("events:read") && record.subject_hash.is_none(),
        },
        _ => false,
    }
}

fn sanitize_record_for_access(access: &AccessMode, record: &mut EventRecord) {
    if !access.is_agent() {
        return;
    }
    if record.event.as_deref() == Some("lxmf_message") {
        record.payload = sanitize_message_payload(&record.payload);
    } else if record.kind == "notification" {
        record.payload = json!({
            "thread_id": record.payload.get("thread_id"),
            "notification_id": record.payload.get("notification_id"),
            "kind": record.payload.get("kind"),
            "agent_safety": {
                "content_redacted": true,
                "reason": "notification text is not part of the agent-safe event contract"
            }
        });
    }
}

fn sanitize_message_payload(payload: &Value) -> Value {
    let source = payload
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let destination = payload
        .get("destination")
        .and_then(Value::as_str)
        .unwrap_or_default();
    json!({
        "id": payload.get("id"),
        "conversation_id": infer_subject_hash("lxmf_message", payload).map(|hash| crate::agent_policy::conversation_id_for_dest(&hash)),
        "source": source,
        "source_display_name": untrusted_text(payload.get("source_display_name")),
        "destination": destination,
        "content": untrusted_text(payload.get("content")),
        "title": untrusted_text(payload.get("title")),
        "timestamp": payload.get("timestamp"),
        "state": payload.get("state"),
        "direction": payload.get("direction"),
        "reply_to_id": payload.get("reply_to_id"),
        "reply_to_preview": untrusted_text(payload.get("reply_to_preview")),
        "has_image": payload.get("image").is_some(),
        "attachment_count": payload
            .get("attachments")
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0),
        "agent_safety": {
            "untrusted_fields": [
                "content.text",
                "title.text",
                "source_display_name.text",
                "reply_to_preview.text"
            ],
            "stored_file_paths_redacted": true
        }
    })
}

fn untrusted_text(value: Option<&Value>) -> Value {
    json!({
        "text": value.and_then(Value::as_str).unwrap_or_default(),
        "untrusted": true
    })
}

fn infer_subject_hash(event: &str, payload: &Value) -> Option<String> {
    match event {
        "lxmf_message" => payload
            .get("source")
            .or_else(|| payload.get("destination"))
            .and_then(Value::as_str)
            .map(str::to_string),
        "lxmf_delivery_progress" => payload
            .get("dest_hash")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => payload
            .get("subject_hash")
            .or_else(|| payload.get("dest_hash"))
            .or_else(|| payload.get("hash"))
            .and_then(Value::as_str)
            .and_then(|hash| dest_hash_from_conversation_id(hash)),
    }
}

fn infer_notification_subject(payload: &Value) -> Option<String> {
    payload
        .get("thread_id")
        .and_then(Value::as_str)
        .and_then(|value| dest_hash_from_conversation_id(value).or_else(|| Some(value.to_string())))
}

fn infer_message_id(event: &str, payload: &Value) -> Option<String> {
    match event {
        "lxmf_message" => payload.get("id").and_then(Value::as_str),
        _ => payload
            .get("msg_id")
            .or_else(|| payload.get("message_id"))
            .and_then(Value::as_str),
    }
    .map(str::to_string)
}

fn infer_identity_id(payload: &Value) -> Option<String> {
    payload
        .get("identity_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn read_cursor(data_dir: &Path) -> CliResult<u64> {
    let path = data_dir.join(EVENT_CURSOR_FILE);
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    raw.trim()
        .parse::<u64>()
        .map_err(|_| CliError::failed("invalid daemon event cursor"))
}

fn write_cursor(data_dir: &Path, cursor: u64) -> CliResult<()> {
    let path = data_dir.join(EVENT_CURSOR_FILE);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, cursor.to_string())?;
    restrict_file_permissions(&tmp)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn restrict_file_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn restrict_dir_permissions(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn unix_now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}
