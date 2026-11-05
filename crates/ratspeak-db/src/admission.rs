//! Propagation-only durable admission. Normal interactive database work keeps
//! using its existing pool; no pooled connection has its pragmas changed here.
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const MAX_PROJECTION_BINDINGS: usize = 1024 * 1024;
/// Reserved qualification headroom, not an allocator-enforced SQLite limit.
pub const NATIVE_DB_HEADROOM: usize = 64 * 1024 * 1024;
pub const POLICY_RECEIPT_SECONDS: u64 = 90 * 86400;
const MAX_POLICY_PER_IDENTITY: i64 = 16_384;
const MAX_POLICY_GLOBAL: i64 = 65_536;
pub const MAX_SELECTED_DELETION: usize = 500;

/// A receipt plus the retained archive and media that must be revalidated.
pub type RecoverableAdmission = (AdmissionReceipt, Option<String>, Vec<FileReference>);
// Stored hash, archive path, effect kind, media manifest, and optional game reference.
type StoredEffectRow = (Vec<u8>, String, String, String, Option<String>);

pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS lxmf_inbound_effects (
 identity_id TEXT NOT NULL, message_id BLOB NOT NULL CHECK(length(message_id)=32),
 stored_wire_hash BLOB NOT NULL CHECK(length(stored_wire_hash)=32),
 archive_path TEXT NOT NULL, media_manifest TEXT NOT NULL DEFAULT '[]', game_reference TEXT, game_identity TEXT, game_session TEXT, effect_kind TEXT NOT NULL,
 committed_at INTEGER NOT NULL, PRIMARY KEY(identity_id,message_id)
);
CREATE TABLE IF NOT EXISTS lxmf_inbound_receipts (
 identity_id TEXT NOT NULL, transient_id BLOB NOT NULL CHECK(length(transient_id)=32),
 node_hash BLOB NOT NULL CHECK(length(node_hash)=16),
 message_id BLOB NOT NULL CHECK(length(message_id)=32),
 received_wire_hash BLOB NOT NULL CHECK(length(received_wire_hash)=32),
 stored_message_wire_hash BLOB CHECK(stored_message_wire_hash IS NULL OR length(stored_message_wire_hash)=32),
 disposition TEXT NOT NULL CHECK(disposition IN ('committed','duplicate','policy_discard','user_deleted')),
 committed_at INTEGER NOT NULL, expires_at INTEGER,
 PRIMARY KEY(identity_id,transient_id)
);
CREATE INDEX IF NOT EXISTS lxmf_receipts_message ON lxmf_inbound_receipts(identity_id,message_id);
CREATE TABLE IF NOT EXISTS lxmf_user_deleted (
 identity_id TEXT NOT NULL, message_id BLOB NOT NULL CHECK(length(message_id)=32),
 deleted_at INTEGER NOT NULL, PRIMARY KEY(identity_id,message_id)
);
CREATE TABLE IF NOT EXISTS lxmf_download_suppression (
 identity_id TEXT NOT NULL, node_hash BLOB NOT NULL CHECK(length(node_hash)=16),
 transient_id BLOB NOT NULL CHECK(length(transient_id)=32),
 record BLOB NOT NULL CHECK(length(record)<=512), expires_at INTEGER NOT NULL,
 PRIMARY KEY(identity_id,node_hash,transient_id)
);
CREATE TABLE IF NOT EXISTS lxmf_admission_barrier (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), nonce BLOB NOT NULL CHECK(length(nonce)=32)
);
CREATE TABLE IF NOT EXISTS lxmf_deleted_artifacts (
 id INTEGER PRIMARY KEY AUTOINCREMENT,
 identity_id TEXT NOT NULL CHECK(length(CAST(identity_id AS BLOB))<=128),
 kind TEXT NOT NULL CHECK(kind IN ('media','archive')),
 path TEXT NOT NULL CHECK(length(CAST(path AS BLOB))<=4096),
 UNIQUE(identity_id,kind,path)
);
CREATE TABLE IF NOT EXISTS lxmf_admission_epochs (
 identity_id TEXT PRIMARY KEY, storage_revision INTEGER NOT NULL DEFAULT 0
);
"#;

#[derive(Debug, thiserror::Error)]
pub enum AdmissionDbError {
    #[error("propagation message exceeds the local database projection budget")]
    TooLarge,
    #[error("another propagation admission is still running")]
    Busy,
    #[error("durable database admission is unavailable: {0}")]
    Unavailable(String),
    #[error("database commit is uncertain; reconcile before acknowledging: {0}")]
    CommitUncertain(String),
    #[error("local disposition ledger is full")]
    LedgerFull,
    #[error("retained message effect is missing or inconsistent")]
    MissingEffect,
    #[error("no active identity")]
    NoIdentity,
    #[error("a selected message is still sending")]
    StillSending,
}
impl From<rusqlite::Error> for AdmissionDbError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Unavailable(error.to_string())
    }
}
fn guard_semaphore() -> &'static Arc<Semaphore> {
    static GUARD: OnceLock<Arc<Semaphore>> = OnceLock::new();
    GUARD.get_or_init(|| Arc::new(Semaphore::new(1)))
}
/// A candidate under one local identity. For media `path` is a stored basename;
/// archives retain their exact database path and must be root-checked by the adapter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeletedArtifact {
    pub identity: String,
    pub path: String,
}
#[derive(Debug, Default)]
pub struct MessageDeletionArtifacts {
    pub media: Vec<DeletedArtifact>,
    pub archives: Vec<DeletedArtifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletedArtifactKind {
    Media,
    Archive,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDeletedArtifact {
    pub id: i64,
    pub kind: DeletedArtifactKind,
    pub artifact: DeletedArtifact,
}

/// Move into the actual blocking closure, including attachment staging. Dropping
/// the async awaiter never releases a closure that still owns this permit.
pub struct AdmissionGuard {
    _permit: OwnedSemaphorePermit,
}
pub fn try_reserve_admission() -> Result<AdmissionGuard, AdmissionDbError> {
    Ok(AdmissionGuard {
        _permit: guard_semaphore()
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdmissionDbError::Busy)?,
    })
}
/// Hold throughout database relocation/reset, after stopping new downloads.
pub async fn quiesce_admissions() -> Result<AdmissionGuard, AdmissionDbError> {
    Ok(AdmissionGuard {
        _permit: guard_semaphore()
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AdmissionDbError::Busy)?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Committed,
    Duplicate,
    PolicyDiscard,
    UserDeleted,
}
impl Disposition {
    fn name(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Duplicate => "duplicate",
            Self::PolicyDiscard => "policy_discard",
            Self::UserDeleted => "user_deleted",
        }
    }
    fn parse(value: &str) -> Result<Self, AdmissionDbError> {
        match value {
            "committed" => Ok(Self::Committed),
            "duplicate" => Ok(Self::Duplicate),
            "policy_discard" => Ok(Self::PolicyDiscard),
            "user_deleted" => Ok(Self::UserDeleted),
            _ => Err(AdmissionDbError::MissingEffect),
        }
    }
}
#[derive(Debug, Clone)]
pub struct AdmissionReceipt {
    pub identity: String,
    pub transient_id: [u8; 32],
    pub node: [u8; 16],
    pub message_id: [u8; 32],
    pub received_wire_hash: [u8; 32],
    pub stored_wire_hash: Option<[u8; 32]>,
    pub disposition: Disposition,
    pub committed_at: u64,
}
/// Files referenced by the visible effect. Paths are basenames relative to
/// the identity's attachment directory, never caller-selected absolute paths.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileReference {
    pub name: String,
    pub hash: [u8; 32],
    pub length: usize,
}
/// A retained game action, including the exact original envelope on nonce replay.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GameReference {
    pub identity: String,
    pub session: String,
    pub nonce: [u8; 8],
    pub envelope: Vec<u8>,
}
fn parse_game(encoded: &str) -> Result<GameReference, AdmissionDbError> {
    if encoded.len() > 128 * 1024 {
        return Err(AdmissionDbError::MissingEffect);
    }
    let value: GameReference =
        serde_json::from_str(encoded).map_err(|_| AdmissionDbError::MissingEffect)?;
    if value.identity.len() != 32
        || value.session.len() > 128
        || value.envelope.len() > lrgp::protocol::ENVELOPE_MAX_PACKED
    {
        return Err(AdmissionDbError::MissingEffect);
    }
    Ok(value)
}
fn verify_game(conn: &Connection, encoded: &str) -> Result<(), AdmissionDbError> {
    let game = parse_game(encoded)?;
    let retained: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM app_actions WHERE identity_id=?1 AND session_id=?2 AND envelope_mp=?3)",params![game.identity,game.session,game.envelope],|row|row.get(0))?;
    if retained {
        Ok(())
    } else {
        Err(AdmissionDbError::MissingEffect)
    }
}
fn parse_files(encoded: &str) -> Result<Vec<FileReference>, AdmissionDbError> {
    if encoded.len() > 16 * 1024 {
        return Err(AdmissionDbError::MissingEffect);
    }
    let files: Vec<FileReference> =
        serde_json::from_str(encoded).map_err(|_| AdmissionDbError::MissingEffect)?;
    if files.len() > 2
        || files.iter().any(|file| {
            file.name.len() > 128
                || file.name.is_empty()
                || !file
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                || file.name == "."
                || file.name == ".."
        })
    {
        return Err(AdmissionDbError::MissingEffect);
    }
    Ok(files)
}

/// Measured variable-size SQL binding occurrences plus fixed receipt/effect
/// lookups. Construct before allocating/staging media or entering SQLite.
pub struct PreparedAdmission {
    identity: String,
    transient_id: [u8; 32],
    node: [u8; 16],
    message_id: [u8; 32],
    received_wire_hash: [u8; 32],
    archive_path: String,
    media_manifest: String,
    game_reference: Option<String>,
    binding_bytes: usize,
    committed_at: u64,
}
impl PreparedAdmission {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: String,
        transient_id: [u8; 32],
        node: [u8; 16],
        message_id: [u8; 32],
        received_wire_hash: [u8; 32],
        archive_path: String,
        committed_at: u64,
        projection_binding_occurrences: impl IntoIterator<Item = usize>,
    ) -> Result<Self, AdmissionDbError> {
        if identity.len() != 32
            || !identity.bytes().all(|v| v.is_ascii_hexdigit())
            || archive_path.len() > 4096
        {
            return Err(AdmissionDbError::Unavailable(
                "invalid admission identity/path".into(),
            ));
        }
        // Includes all receipt/effect/tombstone/suppression queries on either
        // branch, even when a particular admission does not execute them all.
        let fixed = 4096usize
            .checked_add(archive_path.len())
            .ok_or(AdmissionDbError::TooLarge)?;
        let total = projection_binding_occurrences
            .into_iter()
            .try_fold(fixed, |sum, len| sum.checked_add(len))
            .ok_or(AdmissionDbError::TooLarge)?;
        if total > MAX_PROJECTION_BINDINGS {
            return Err(AdmissionDbError::TooLarge);
        }
        Ok(Self {
            identity,
            transient_id,
            node,
            message_id,
            received_wire_hash,
            archive_path,
            media_manifest: "[]".into(),
            game_reference: None,
            binding_bytes: total,
            committed_at,
        })
    }
    pub fn with_game(mut self, game: &GameReference) -> Result<Self, AdmissionDbError> {
        let encoded = bounded_json(game, 128 * 1024)?;
        parse_game(&encoded)?;
        let extra = encoded
            .len()
            .checked_add(game.envelope.len())
            .and_then(|n| n.checked_add(game.identity.len() + game.session.len()))
            .ok_or(AdmissionDbError::TooLarge)?;
        if self
            .binding_bytes
            .checked_add(extra)
            .is_none_or(|n| n > MAX_PROJECTION_BINDINGS)
        {
            return Err(AdmissionDbError::TooLarge);
        }
        self.binding_bytes += extra;
        self.game_reference = Some(encoded);
        Ok(self)
    }
    pub fn with_files(mut self, files: &[FileReference]) -> Result<Self, AdmissionDbError> {
        let encoded = bounded_json(&files, 16 * 1024)?;
        parse_files(&encoded)?;
        if self
            .binding_bytes
            .checked_add(encoded.len())
            .is_none_or(|size| size > MAX_PROJECTION_BINDINGS)
        {
            return Err(AdmissionDbError::TooLarge);
        }
        self.binding_bytes += encoded.len();
        self.media_manifest = encoded;
        Ok(self)
    }
}

/// Application effects must be applied on the provided connection in the same
/// transaction. Policy discard has no visible effect and no archive reference.
pub enum EffectCommit {
    Applied(&'static str),
    PolicyDiscard,
}

#[derive(Clone, Debug)]
pub struct DurableInboundStore {
    path: PathBuf,
}
impl DurableInboundStore {
    /// The runtime first qualifies this directory through the native strict
    /// storage backend. Schema initialization must already have completed.
    pub fn open_existing(path: &Path) -> Result<Self, AdmissionDbError> {
        let path = std::fs::canonicalize(path)
            .map_err(|e| AdmissionDbError::Unavailable(e.to_string()))?;
        let store = Self { path };
        let _ = store.connection()?;
        Ok(store)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn connection(&self) -> Result<Connection, AdmissionDbError> {
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let version: i64 =
            conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })?;
        if version != crate::db::SCHEMA_VERSION {
            return Err(AdmissionDbError::Unavailable(
                "database schema changed".into(),
            ));
        }
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(AdmissionDbError::Unavailable(
                "database is not in WAL mode".into(),
            ));
        }
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=30000; PRAGMA synchronous=FULL; PRAGMA cache_size=-2048;")?;
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        conn.execute_batch("PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON;")?;
        for (pragma, expected) in [
            ("synchronous", 2i64),
            ("foreign_keys", 1),
            ("busy_timeout", 30000),
        ] {
            let actual: i64 = conn.pragma_query_value(None, pragma, |row| row.get(0))?;
            if actual != expected {
                return Err(AdmissionDbError::Unavailable(format!(
                    "{pragma} could not be enabled"
                )));
            }
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if conn.pragma_query_value(None, "fullfsync", |row| row.get::<_, i64>(0))? != 1 {
            return Err(AdmissionDbError::Unavailable(
                "fullfsync could not be enabled".into(),
            ));
        }
        Ok(conn)
    }
    pub fn commit<F, V>(
        &self,
        guard: AdmissionGuard,
        plan: PreparedAdmission,
        verify_retained: V,
        apply: F,
    ) -> Result<AdmissionReceipt, AdmissionDbError>
    where
        F: FnOnce(&Connection) -> Result<EffectCommit, AdmissionDbError>,
        V: FnOnce(&str, [u8; 32], &[FileReference]) -> Result<(), AdmissionDbError>,
    {
        let _guard = guard;
        let game = plan.game_reference.as_deref().map(parse_game).transpose()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM lxmf_inbound_receipts WHERE rowid IN (SELECT rowid FROM lxmf_inbound_receipts WHERE expires_at IS NOT NULL AND expires_at<=?1 LIMIT 256)",
            [plan.committed_at],
        )?;
        let prior:Option<([u8;32],[u8;32])>=tx.query_row("SELECT message_id,received_wire_hash FROM lxmf_inbound_receipts WHERE identity_id=?1 AND transient_id=?2",params![plan.identity,plan.transient_id.as_slice()],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if prior.is_some_and(|(id, wire)| id != plan.message_id || wire != plan.received_wire_hash)
        {
            return Err(AdmissionDbError::MissingEffect);
        }
        let deleted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM lxmf_user_deleted WHERE identity_id=?1 AND message_id=?2)",
            params![plan.identity, plan.message_id.as_slice()],
            |row| row.get(0),
        )?;
        let existing:Option<StoredEffectRow>=tx.query_row("SELECT stored_wire_hash,CASE WHEN length(CAST(archive_path AS BLOB))<=4096 THEN archive_path END,effect_kind,CASE WHEN length(CAST(media_manifest AS BLOB))<=16384 THEN media_manifest END,CASE WHEN length(CAST(game_reference AS BLOB))<=131072 THEN game_reference END FROM lxmf_inbound_effects WHERE identity_id=?1 AND message_id=?2",params![plan.identity,plan.message_id.as_slice()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        let (disposition, stored_wire_hash) = if deleted {
            (Disposition::UserDeleted, None)
        } else if let Some((hash, path, kind, files, game)) = existing {
            let hash: [u8; 32] = hash
                .try_into()
                .map_err(|_| AdmissionDbError::MissingEffect)?;
            verify_retained(&path, hash, &parse_files(&files)?)?;
            if kind == "game" {
                verify_game(&tx, game.as_deref().ok_or(AdmissionDbError::MissingEffect)?)?;
            }
            if kind == "message" {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE identity_id=?1 AND id=?2)",
                    params![plan.identity, hex::encode(plan.message_id)],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(AdmissionDbError::MissingEffect);
                }
            }
            (Disposition::Duplicate, Some(hash))
        } else {
            match apply(&tx)? {
                EffectCommit::PolicyDiscard => (Disposition::PolicyDiscard, None),
                EffectCommit::Applied(kind) => {
                    if !matches!(kind, "message" | "reaction" | "game" | "service") {
                        return Err(AdmissionDbError::MissingEffect);
                    }
                    if kind == "game" {
                        verify_game(
                            &tx,
                            plan.game_reference
                                .as_deref()
                                .ok_or(AdmissionDbError::MissingEffect)?,
                        )?;
                    }
                    tx.execute("INSERT INTO lxmf_inbound_effects(identity_id,message_id,stored_wire_hash,archive_path,effect_kind,committed_at,media_manifest,game_reference,game_identity,game_session) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![plan.identity,plan.message_id.as_slice(),plan.received_wire_hash.as_slice(),plan.archive_path,kind,plan.committed_at,plan.media_manifest,plan.game_reference,game.as_ref().map(|g|g.identity.as_str()),game.as_ref().map(|g|g.session.as_str())])?;
                    (Disposition::Committed, Some(plan.received_wire_hash))
                }
            }
        };
        if matches!(
            disposition,
            Disposition::PolicyDiscard | Disposition::UserDeleted
        ) {
            let (global,identity):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(identity_id=?1),0) FROM lxmf_inbound_receipts WHERE disposition IN ('policy_discard','user_deleted')",[&plan.identity],|row|Ok((row.get(0)?,row.get(1)?)))?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM lxmf_inbound_receipts WHERE identity_id=?1 AND transient_id=?2)",params![plan.identity,plan.transient_id.as_slice()],|row|row.get(0))?;
            if !exists && (global >= MAX_POLICY_GLOBAL || identity >= MAX_POLICY_PER_IDENTITY) {
                return Err(AdmissionDbError::LedgerFull);
            }
        }
        let expires = matches!(
            disposition,
            Disposition::PolicyDiscard | Disposition::UserDeleted
        )
        .then_some(plan.committed_at.saturating_add(POLICY_RECEIPT_SECONDS));
        tx.execute("INSERT INTO lxmf_inbound_receipts(identity_id,transient_id,node_hash,message_id,received_wire_hash,stored_message_wire_hash,disposition,committed_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(identity_id,transient_id) DO UPDATE SET node_hash=excluded.node_hash,stored_message_wire_hash=excluded.stored_message_wire_hash,disposition=excluded.disposition,committed_at=excluded.committed_at,expires_at=excluded.expires_at",params![plan.identity,plan.transient_id.as_slice(),plan.node.as_slice(),plan.message_id.as_slice(),plan.received_wire_hash.as_slice(),stored_wire_hash.as_ref().map(|h|h.as_slice()),disposition.name(),plan.committed_at,expires])?;
        tx.execute(
            "DELETE FROM lxmf_download_suppression WHERE identity_id=?1 AND transient_id=?2",
            params![plan.identity, plan.transient_id.as_slice()],
        )?;
        tx.commit()
            .map_err(|e| AdmissionDbError::CommitUncertain(e.to_string()))?;
        // The FULL connection is closed before releasing the one-worker guard.
        drop(conn);
        Ok(AdmissionReceipt {
            identity: plan.identity,
            transient_id: plan.transient_id,
            node: plan.node,
            message_id: plan.message_id,
            received_wire_hash: plan.received_wire_hash,
            stored_wire_hash,
            disposition,
            committed_at: plan.committed_at,
        })
    }
    /// Candidate haves must also pass the runtime's archive/hash verification.
    /// Renew a FULL WAL commit before rehydrating receipts after a restart or
    /// uncertain commit. A real nonce change dirties the transaction; a read or
    /// no-op update cannot establish this barrier. File references still need
    /// independent native reconciliation before they authorize haves.
    pub fn reconcile_barrier(&self) -> Result<(), AdmissionDbError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO lxmf_admission_barrier(singleton,nonce) VALUES(1,randomblob(32)) ON CONFLICT(singleton) DO UPDATE SET nonce=excluded.nonce", [])?;
        tx.commit()
            .map_err(|error| AdmissionDbError::CommitUncertain(error.to_string()))
    }

    pub fn receipts(
        &self,
        identity: &str,
        now: u64,
    ) -> Result<Vec<RecoverableAdmission>, AdmissionDbError> {
        let conn = self.connection()?;
        let mut query=conn.prepare("SELECT r.transient_id,r.node_hash,r.message_id,r.received_wire_hash,r.stored_message_wire_hash,r.disposition,r.committed_at,e.archive_path,COALESCE(e.media_manifest,'[]'),e.game_reference,e.effect_kind FROM lxmf_inbound_receipts r LEFT JOIN lxmf_inbound_effects e ON e.identity_id=r.identity_id AND e.message_id=r.message_id WHERE r.identity_id=?1 AND length(r.disposition)<=32 AND (e.archive_path IS NULL OR length(CAST(e.archive_path AS BLOB))<=4096) AND (e.media_manifest IS NULL OR length(CAST(e.media_manifest AS BLOB))<=16384) AND (e.game_reference IS NULL OR length(CAST(e.game_reference AS BLOB))<=131072) AND (e.effect_kind IS NULL OR length(e.effect_kind)<=16) AND (r.expires_at IS NULL OR r.expires_at>?2) AND (r.disposition IN ('policy_discard','user_deleted') OR (e.stored_wire_hash=r.stored_message_wire_hash AND (e.effect_kind!='message' OR EXISTS(SELECT 1 FROM messages m WHERE m.identity_id=e.identity_id AND m.id=lower(hex(e.message_id))))))")?;
        let rows = query.query_map(params![identity, now], |row| {
            Ok((
                row.get::<_, [u8; 32]>(0)?,
                row.get::<_, [u8; 16]>(1)?,
                row.get::<_, [u8; 32]>(2)?,
                row.get::<_, [u8; 32]>(3)?,
                row.get::<_, Option<[u8; 32]>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, u64>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (tid, node, id, received, stored, disposition, at, path, files, game, kind) = row?;
            if kind.as_deref() == Some("game")
                && game
                    .as_deref()
                    .is_none_or(|game| verify_game(&conn, game).is_err())
            {
                continue;
            }
            result.push((
                AdmissionReceipt {
                    identity: identity.into(),
                    transient_id: tid,
                    node,
                    message_id: id,
                    received_wire_hash: received,
                    stored_wire_hash: stored,
                    disposition: Disposition::parse(&disposition)?,
                    committed_at: at,
                },
                path,
                parse_files(&files)?,
            ));
        }
        Ok(result)
    }
}

#[derive(Clone, Debug)]
pub struct SuppressionRow {
    pub identity: String,
    pub node: [u8; 16],
    pub transient_id: [u8; 32],
    pub encoded: Vec<u8>,
    pub expires_at: u64,
}
impl DurableInboundStore {
    pub fn from_pool(pool: &crate::db::DbPool) -> Result<Self, AdmissionDbError> {
        let path =
            {
                let conn = pool
                    .get()
                    .map_err(|e| AdmissionDbError::Unavailable(e.to_string()))?;
                PathBuf::from(conn.path().ok_or_else(|| {
                    AdmissionDbError::Unavailable("database is not a file".into())
                })?)
            };
        Self::open_existing(&path)
    }
    pub fn policy_state(&self, identity: &str) -> Result<(Vec<String>, u64), AdmissionDbError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT dest_hash FROM blocked_contacts WHERE identity_id=?1 ORDER BY dest_hash",
        )?;
        let blocked = stmt
            .query_map([identity], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let revision = conn
            .query_row(
                "SELECT storage_revision FROM lxmf_admission_epochs WHERE identity_id=?1",
                [identity],
                |row| row.get::<_, u64>(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok((blocked, revision))
    }
    /// Reference checks for bounded orphan-intent cleanup. No receipt is minted
    /// by this path; referenced payloads are never removed.
    pub fn references_archive(&self, identity: &str, path: &str) -> Result<bool, AdmissionDbError> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM lxmf_inbound_effects WHERE identity_id=?1 AND archive_path=?2)",params![identity,path],|row|row.get(0))?)
    }
    pub fn references_media(&self, identity: &str, name: &str) -> Result<bool, AdmissionDbError> {
        Ok(self.connection()?.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE identity_id=?1 AND (attachment_stored_name=?2 OR image_stored_name=?2 OR audio_stored_name=?2))",params![identity,name],|row|row.get(0))?)
    }
    /// Explicit retry clears only the selected identity/node. Each FULL batch
    /// removes at most 256 records; partial failure keeps the client paused.
    pub fn clear_suppression(
        &self,
        identity: &str,
        node: [u8; 16],
    ) -> Result<(), AdmissionDbError> {
        let _guard = try_reserve_admission()?;
        let mut conn = self.connection()?;
        loop {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed=tx.execute("DELETE FROM lxmf_download_suppression WHERE rowid IN (SELECT rowid FROM lxmf_download_suppression WHERE identity_id=?1 AND node_hash=?2 LIMIT 256)",params![identity,node.as_slice()])?;
            tx.commit()
                .map_err(|e| AdmissionDbError::CommitUncertain(e.to_string()))?;
            if changed < 256 {
                break;
            }
        }
        Ok(())
    }
    pub fn storage_recovered(
        &self,
        _guard: &AdmissionGuard,
        identity: &str,
    ) -> Result<(), AdmissionDbError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO lxmf_admission_epochs(identity_id,storage_revision) VALUES(?1,1) ON CONFLICT(identity_id) DO UPDATE SET storage_revision=storage_revision+1 WHERE storage_revision<9223372036854775807",[identity])?;
        tx.commit()
            .map_err(|e| AdmissionDbError::CommitUncertain(e.to_string()))
    }
    pub fn suppression_rows(
        &self,
        identity: &str,
        now: u64,
    ) -> Result<Vec<Vec<u8>>, AdmissionDbError> {
        let conn = self.connection()?;
        let mut statement=conn.prepare("SELECT CASE WHEN length(record)<=512 THEN record END FROM lxmf_download_suppression WHERE identity_id=?1 AND expires_at>?2 ORDER BY node_hash,transient_id LIMIT 16385")?;
        let rows = statement
            .query_map(params![identity, now], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<Vec<u8>>>>()?;
        if rows.len() > 16_384 || rows.iter().any(|row| row.len() > 512) {
            return Err(AdmissionDbError::LedgerFull);
        }
        Ok(rows)
    }
    pub fn write_suppression(
        &self,
        rows: &[SuppressionRow],
        remove: &[(String, [u8; 16], [u8; 32])],
        now: u64,
    ) -> Result<(), AdmissionDbError> {
        if rows.len() + remove.len() > 256
            || rows
                .iter()
                .any(|row| row.encoded.len() > 512 || row.identity.len() != 32)
        {
            return Err(AdmissionDbError::TooLarge);
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM lxmf_download_suppression WHERE rowid IN (SELECT rowid FROM lxmf_download_suppression WHERE expires_at<=?1 LIMIT 256)",
            [now],
        )?;
        for (identity, node, tid) in remove {
            tx.execute("DELETE FROM lxmf_download_suppression WHERE identity_id=?1 AND node_hash=?2 AND transient_id=?3",params![identity,node.as_slice(),tid.as_slice()])?;
        }
        for row in rows {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM lxmf_download_suppression WHERE identity_id=?1 AND node_hash=?2 AND transient_id=?3)",params![row.identity,row.node.as_slice(),row.transient_id.as_slice()],|row|row.get(0))?;
            if !exists {
                let(global,identity,node):(i64,i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(identity_id=?1),0),COALESCE(SUM(identity_id=?1 AND node_hash=?2),0) FROM lxmf_download_suppression",params![row.identity,row.node.as_slice()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                if global >= 65_536 || identity >= 16_384 || node >= 4096 {
                    return Err(AdmissionDbError::LedgerFull);
                }
            }
            tx.execute("INSERT INTO lxmf_download_suppression(identity_id,node_hash,transient_id,record,expires_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(identity_id,node_hash,transient_id) DO UPDATE SET record=excluded.record,expires_at=excluded.expires_at",params![row.identity,row.node.as_slice(),row.transient_id.as_slice(),row.encoded,row.expires_at])?;
        }
        tx.commit()
            .map_err(|e| AdmissionDbError::CommitUncertain(e.to_string()))?;
        Ok(())
    }
    /// Delete a terminal game and revoke its propagated effects atomically.
    /// The local game identity is the LXMF destination, not the RNS identity.
    pub fn delete_game_session(
        &self,
        identity: &str,
        session: &str,
        now: u64,
    ) -> Result<bool, AdmissionDbError> {
        let _guard = try_reserve_admission()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM app_sessions WHERE identity_id=?1 AND session_id=?2",
                params![identity, session],
                |row| row.get(0),
            )
            .optional()?;
        if !status
            .is_some_and(|status| matches!(status.as_str(), "completed" | "declined" | "expired"))
        {
            return Ok(false);
        }
        tx.execute("INSERT INTO lxmf_deleted_artifacts(identity_id,kind,path) SELECT identity_id,'archive',archive_path FROM lxmf_inbound_effects WHERE game_identity=?1 AND game_session=?2 ON CONFLICT(identity_id,kind,path) DO NOTHING",params![identity,session])?;
        tx.execute("INSERT INTO lxmf_user_deleted(identity_id,message_id,deleted_at) SELECT identity_id,message_id,?3 FROM lxmf_inbound_effects WHERE game_identity=?1 AND game_session=?2 ON CONFLICT(identity_id,message_id) DO NOTHING", params![identity,session,now])?;
        tx.execute("DELETE FROM lxmf_inbound_receipts WHERE (identity_id,message_id) IN (SELECT identity_id,message_id FROM lxmf_inbound_effects WHERE game_identity=?1 AND game_session=?2)", params![identity,session])?;
        tx.execute(
            "DELETE FROM lxmf_inbound_effects WHERE game_identity=?1 AND game_session=?2",
            params![identity, session],
        )?;
        tx.execute(
            "DELETE FROM app_actions WHERE identity_id=?1 AND session_id=?2",
            params![identity, session],
        )?;
        tx.execute(
            "DELETE FROM app_sessions WHERE identity_id=?1 AND session_id=?2",
            params![identity, session],
        )?;
        tx.commit()
            .map_err(|error| AdmissionDbError::CommitUncertain(error.to_string()))?;
        Ok(true)
    }

    /// Read at most256 durable cleanup candidates. Continue after the last ID;
    /// when a scan returns empty, restart at zero so referenced items are retried.
    pub fn pending_deleted_artifacts(
        &self,
        _guard: &AdmissionGuard,
        identity: Option<&str>,
        after: i64,
        limit: usize,
    ) -> Result<Vec<PendingDeletedArtifact>, AdmissionDbError> {
        if limit == 0 || limit > 256 || after < 0 {
            return Err(AdmissionDbError::TooLarge);
        }
        let conn = self.connection()?;
        let mut stmt=conn.prepare("SELECT id,kind,identity_id,path FROM lxmf_deleted_artifacts WHERE id>?1 AND (?2 IS NULL OR identity_id=?2) ORDER BY id LIMIT ?3")?;
        let rows = stmt.query_map(params![after, identity, limit as i64], |row| {
            let kind = row.get_ref(1)?.as_str()?;
            let identity = row.get_ref(2)?.as_str()?;
            let path = row.get_ref(3)?.as_str()?;
            if identity.len() > 128 || path.len() > 4096 {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let kind = match kind {
                "media" => DeletedArtifactKind::Media,
                "archive" => DeletedArtifactKind::Archive,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(PendingDeletedArtifact {
                id: row.get(0)?,
                kind,
                artifact: DeletedArtifact {
                    identity: identity.to_owned(),
                    path: path.to_owned(),
                },
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
    /// Complete only records whose artifacts were safely removed or already
    /// absent under renewed namespace barriers. References keep their records.
    pub fn complete_deleted_artifacts(
        &self,
        _guard: &AdmissionGuard,
        ids: &[i64],
    ) -> Result<(), AdmissionDbError> {
        if ids.len() > 256 || ids.iter().any(|id| *id <= 0) {
            return Err(AdmissionDbError::TooLarge);
        }
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for id in ids {
            tx.execute("DELETE FROM lxmf_deleted_artifacts WHERE id=?1", [id])?;
        }
        tx.commit()
            .map_err(|error| AdmissionDbError::CommitUncertain(error.to_string()))?;
        Ok(())
    }

    /// Logical deletion without filesystem cleanup. Call the guarded variant
    /// when removing artifacts so its admission exclusion survives the cleanup.
    pub fn delete_messages(
        &self,
        identity: &str,
        peer: Option<&str>,
        now: u64,
    ) -> Result<(), AdmissionDbError> {
        let guard = try_reserve_admission()?;
        self.delete_messages_guarded(&guard, identity, peer, now)
            .map(|_| ())
    }

    /// Commit stable deletion history and return identity-scoped cleanup
    /// candidates. The caller retains `guard`, rechecks remaining database and
    /// staging-intent references, and performs safe filesystem cleanup before
    /// releasing it. Returned paths alone are never deletion authority.
    pub fn delete_messages_guarded(
        &self,
        _guard: &AdmissionGuard,
        identity: &str,
        peer: Option<&str>,
        now: u64,
    ) -> Result<MessageDeletionArtifacts, AdmissionDbError> {
        self.delete_scoped(identity, peer, None, None, now)
            .map(|(artifacts, _)| artifacts)
    }

    /// As `delete_messages_guarded`, limited to one conversation with `peer`
    /// (`""` is Main). An extra conversation is forgotten with its messages.
    pub fn delete_thread_messages_guarded(
        &self,
        _guard: &AdmissionGuard,
        identity: &str,
        peer: &str,
        thread: &str,
        now: u64,
    ) -> Result<MessageDeletionArtifacts, AdmissionDbError> {
        self.delete_scoped(identity, Some(peer), Some(thread), None, now)
            .map(|(artifacts, _)| artifacts)
    }

    /// As `delete_messages_guarded`, limited to `ids` among the identity's
    /// messages with `peer`; other ids are ignored. Refuses the whole set
    /// while any of them is still sending. Also returns the ids deleted.
    pub fn delete_selected_messages_guarded(
        &self,
        _guard: &AdmissionGuard,
        identity: &str,
        peer: &str,
        ids: &[String],
        now: u64,
    ) -> Result<(MessageDeletionArtifacts, Vec<String>), AdmissionDbError> {
        if ids.len() > MAX_SELECTED_DELETION {
            return Err(AdmissionDbError::TooLarge);
        }
        let ids = serde_json::to_string(ids).map_err(|_| AdmissionDbError::TooLarge)?;
        self.delete_scoped(identity, Some(peer), None, Some(&ids), now)
    }

    fn delete_scoped(
        &self,
        identity: &str,
        peer: Option<&str>,
        thread: Option<&str>,
        ids: Option<&str>,
        now: u64,
    ) -> Result<(MessageDeletionArtifacts, Vec<String>), AdmissionDbError> {
        const SCOPE: &str = "identity_id=?1 AND (?2 IS NULL OR source=?2 OR destination=?2) AND (?3 IS NULL OR id IN (SELECT value FROM json_each(?3))) AND (?4 IS NULL OR thread=?4)";
        fn text(row: &rusqlite::Row<'_>, column: usize, limit: usize) -> rusqlite::Result<String> {
            let value = row.get_ref(column)?.as_str()?;
            if value.len() > limit {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    column,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "deletion metadata exceeds bounded column length",
                    )),
                ));
            }
            Ok(value.to_owned())
        }
        // An empty identity must never widen deletion to every identity.
        if identity.is_empty() {
            return Err(AdmissionDbError::NoIdentity);
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Matches the outbound states the client can still cancel.
        if ids.is_some()
            && tx.query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM messages WHERE {SCOPE} AND direction='outbound' AND state NOT IN ('delivered','propagated','failed','cancelled','rejected','timeout','read') AND NOT (state='sent' AND lower(COALESCE(delivery_method,''))<>'direct'))"),
                params![identity, peer, ids, thread],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Err(AdmissionDbError::StillSending);
        }
        let mut artifacts = MessageDeletionArtifacts::default();
        let mut deleted = Vec::new();
        {
            let mut stmt = tx.prepare(&format!("SELECT id,identity_id,attachment_stored_name,image_stored_name,audio_stored_name FROM messages WHERE {SCOPE}"))?;
            let rows = stmt.query_map(params![identity, peer, ids, thread], |row| {
                Ok((
                    text(row, 0, 128)?,
                    text(row, 1, 128)?,
                    text(row, 2, 1024)?,
                    text(row, 3, 1024)?,
                    text(row, 4, 1024)?,
                ))
            })?;
            for row in rows {
                let (id, iid, attachment, image, audio) = row?;
                if let Ok(id) = hex::decode(&id)
                    && id.len() == 32
                {
                    let archive=tx.query_row("SELECT archive_path FROM lxmf_inbound_effects WHERE identity_id=?1 AND message_id=?2",params![iid,id],|row|text(row,0,4096)).optional()?;
                    if let Some(path) = archive {
                        artifacts.archives.push(DeletedArtifact {
                            identity: iid.clone(),
                            path,
                        });
                    }
                    tx.execute("INSERT INTO lxmf_user_deleted(identity_id,message_id,deleted_at) VALUES(?1,?2,?3) ON CONFLICT(identity_id,message_id) DO NOTHING",params![iid,id,now])?;
                    tx.execute(
                        "DELETE FROM lxmf_inbound_receipts WHERE identity_id=?1 AND message_id=?2",
                        params![iid, id],
                    )?;
                    tx.execute(
                        "DELETE FROM lxmf_inbound_effects WHERE identity_id=?1 AND message_id=?2",
                        params![iid, id],
                    )?;
                }
                for path in [attachment, image, audio] {
                    if !path.is_empty() && path != crate::db::ATTACHMENT_UNAVAILABLE_STORED_NAME {
                        artifacts.media.push(DeletedArtifact {
                            identity: iid.clone(),
                            path,
                        });
                    }
                }
                if ids.is_some() {
                    deleted.push(id);
                }
            }
        }
        for artifact in &artifacts.media {
            tx.execute("INSERT INTO lxmf_deleted_artifacts(identity_id,kind,path) VALUES(?1,'media',?2) ON CONFLICT(identity_id,kind,path) DO NOTHING",params![artifact.identity,artifact.path])?;
        }
        for artifact in &artifacts.archives {
            tx.execute("INSERT INTO lxmf_deleted_artifacts(identity_id,kind,path) VALUES(?1,'archive',?2) ON CONFLICT(identity_id,kind,path) DO NOTHING",params![artifact.identity,artifact.path])?;
        }
        tx.execute(
            &format!("DELETE FROM reactions WHERE identity_id=?1 AND message_id IN (SELECT id FROM messages WHERE {SCOPE})"),
            params![identity, peer, ids, thread],
        )?;
        tx.execute(
            &format!("DELETE FROM messages WHERE {SCOPE}"),
            params![identity, peer, ids, thread],
        )?;
        if ids.is_none() {
            match (peer, thread) {
                (Some(peer), None) => {
                    tx.execute(
                        "DELETE FROM hidden_conversations WHERE identity_id=?1 AND dest_hash=?2",
                        params![identity, peer],
                    )?;
                    tx.execute(
                        "DELETE FROM conversation_threads WHERE identity_id=?1 AND peer_hash=?2",
                        params![identity, peer],
                    )?;
                }
                (Some(peer), Some(thread)) => {
                    tx.execute(
                        "DELETE FROM conversation_threads WHERE identity_id=?1 AND peer_hash=?2 AND thread=?3",
                        params![identity, peer, thread],
                    )?;
                }
                (None, _) => {
                    tx.execute(
                        "DELETE FROM conversation_threads WHERE identity_id=?1",
                        params![identity],
                    )?;
                }
            }
        }
        tx.commit()
            .map_err(|error| AdmissionDbError::CommitUncertain(error.to_string()))?;
        artifacts.media.sort();
        artifacts.media.dedup();
        artifacts.archives.sort();
        artifacts.archives.dedup();
        deleted.sort();
        Ok((artifacts, deleted))
    }
}

/// Bounded serialization for game/reaction metadata. No large temporary JSON
/// string is built merely to discover that it exceeds the SQL plan cap.
pub fn bounded_json(
    value: &impl serde::Serialize,
    limit: usize,
) -> Result<String, AdmissionDbError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|len| len > self.limit)
            {
                return Err(std::io::Error::other("projection budget exceeded"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded {
        bytes: Vec::new(),
        limit: limit.min(MAX_PROJECTION_BINDINGS),
    };
    serde_json::to_writer(&mut output, value).map_err(|_| AdmissionDbError::TooLarge)?;
    String::from_utf8(output.bytes)
        .map_err(|_| AdmissionDbError::Unavailable("invalid JSON encoding".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fn fixture() -> (PathBuf, crate::db::DbPool, DurableInboundStore) {
        let path = std::env::temp_dir().join(format!(
            "ratspeak-db-admission-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let pool = crate::db::init_pool(&path).unwrap();
        crate::db::init_schema(&pool).unwrap();
        let store = DurableInboundStore::from_pool(&pool).unwrap();
        (path, pool, store)
    }
    fn plan(tid: u8, hash: u8) -> PreparedAdmission {
        PreparedAdmission::new(
            "11".repeat(16),
            [tid; 32],
            [2; 16],
            [3; 32],
            [hash; 32],
            "archive".into(),
            1000,
            [128],
        )
        .unwrap()
    }
    #[test]
    fn full_connection_is_dedicated_and_effect_receipt_commit_together() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let receipt = store
            .commit(
                try_reserve_admission().unwrap(),
                plan(4, 5),
                |_, _, _| Ok(()),
                |conn| {
                    assert_eq!(
                        conn.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                            .unwrap(),
                        2
                    );
                    conn.execute(
                        "INSERT INTO settings(key,value) VALUES('admission-test','effect')",
                        [],
                    )?;
                    Ok(EffectCommit::Applied("service"))
                },
            )
            .unwrap();
        assert_eq!(receipt.disposition, Disposition::Committed);
        assert_eq!(store.receipts(&"11".repeat(16), 1000).unwrap().len(), 1);
        assert_eq!(
            pool.get()
                .unwrap()
                .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn failed_effect_has_no_receipt_or_partial_database_effect() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let result = store.commit(
            try_reserve_admission().unwrap(),
            plan(4, 5),
            |_, _, _| Ok(()),
            |conn| {
                conn.execute(
                    "INSERT INTO settings(key,value) VALUES('admission-test','effect')",
                    [],
                )?;
                Err(AdmissionDbError::Unavailable("injected failure".into()))
            },
        );
        assert!(result.is_err());
        assert!(store.receipts(&"11".repeat(16), 1000).unwrap().is_empty());
        assert_eq!(
            pool.get()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM settings WHERE key='admission-test'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn duplicate_stamp_image_keeps_first_archive_and_requires_retained_effect() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        store
            .commit(
                try_reserve_admission().unwrap(),
                plan(4, 5),
                |_, _, _| Ok(()),
                |_| Ok(EffectCommit::Applied("service")),
            )
            .unwrap();
        let second = store
            .commit(
                try_reserve_admission().unwrap(),
                plan(6, 7),
                |path, hash, _files| {
                    assert_eq!(path, "archive");
                    assert_eq!(hash, [5; 32]);
                    Ok(())
                },
                |_| panic!("duplicate effect reapplied"),
            )
            .unwrap();
        assert_eq!(second.received_wire_hash, [7; 32]);
        assert_eq!(second.stored_wire_hash, Some([5; 32]));
        assert_eq!(second.disposition, Disposition::Duplicate);
        assert!(
            store
                .commit(
                    try_reserve_admission().unwrap(),
                    plan(8, 9),
                    |_, _, _| Err(AdmissionDbError::MissingEffect),
                    |_| panic!("missing archive accepted")
                )
                .is_err()
        );
        assert_eq!(store.receipts(&"11".repeat(16), 1000).unwrap().len(), 2);
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn admission_guard_stays_owned_by_canceled_blocking_worker() {
        let _serial = SERIAL.lock().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let guard = try_reserve_admission().unwrap();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let task = tokio::task::spawn_blocking(move || {
                let _guard = guard;
                let _ = start_tx.send(());
                release_rx.recv().unwrap();
            });
            start_rx.await.unwrap();
            task.abort();
            assert!(matches!(
                try_reserve_admission(),
                Err(AdmissionDbError::Busy)
            ));
            release_tx.send(()).unwrap();
            let _ = task.await;
            assert!(try_reserve_admission().is_ok());
        });
    }
    #[test]
    fn projection_budget_counts_repeated_bindings_before_open_or_effects() {
        let _serial = SERIAL.lock().unwrap();
        assert!(matches!(
            PreparedAdmission::new(
                "11".repeat(16),
                [1; 32],
                [2; 16],
                [3; 32],
                [4; 32],
                "archive".into(),
                1,
                [600_000, 600_000]
            ),
            Err(AdmissionDbError::TooLarge)
        ));
        assert!(matches!(
            bounded_json(&"x".repeat(1024), 100),
            Err(AdmissionDbError::TooLarge)
        ));
    }
    #[test]
    fn thread_deletion_keeps_other_conversations_and_forgets_only_its_own() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let identity = "11".repeat(16);
        let side = hex::encode([9; 32]);
        for (id, thread) in [([3; 32], ""), ([4; 32], side.as_str())] {
            crate::db::try_save_message_with_audio(
                &pool,
                &hex::encode(id),
                "peer",
                "local",
                "m",
                "",
                1.,
                "received",
                "inbound",
                &identity,
                "",
                "",
                "",
                "",
                "",
                "",
                None,
                None,
                "",
                None,
                thread,
            )
            .unwrap();
        }
        let guard = try_reserve_admission().unwrap();
        store
            .delete_thread_messages_guarded(&guard, &identity, "peer", "", 1001)
            .unwrap();
        assert!(crate::db::get_conversation_in_thread(&pool, "peer", "", &identity, 10).is_empty());
        assert_eq!(
            crate::db::get_conversation(&pool, "peer", &identity, 10).len(),
            1
        );
        assert_eq!(
            crate::db::conversation_threads(&pool, &identity, "peer")
                .unwrap()
                .len(),
            2
        );
        store
            .delete_thread_messages_guarded(&guard, &identity, "peer", &side, 1001)
            .unwrap();
        assert!(crate::db::get_conversation(&pool, "peer", &identity, 10).is_empty());
        assert_eq!(
            crate::db::conversation_threads(&pool, &identity, "peer")
                .unwrap()
                .len(),
            1
        );
        drop(guard);
        drop(store);
        drop(pool);
        let _ = std::fs::remove_dir_all(path);
    }
    #[test]
    fn guarded_deletion_returns_exact_archive_and_keeps_admission_excluded() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let identity = "11".repeat(16);
        let id = hex::encode([3; 32]);
        crate::db::try_save_message(
            &pool,
            &id,
            "peer",
            "local",
            "old",
            "",
            1.,
            "received",
            "inbound",
            &identity,
            "file",
            "shared.bin",
            "",
            "",
            "",
            "",
            None,
        )
        .unwrap();
        store
            .commit(
                try_reserve_admission().unwrap(),
                plan(4, 5),
                |_, _, _| Ok(()),
                |_| Ok(EffectCommit::Applied("service")),
            )
            .unwrap();
        let guard = try_reserve_admission().unwrap();
        let artifacts = store
            .delete_messages_guarded(&guard, &identity, Some("peer"), 1001)
            .unwrap();
        assert_eq!(
            artifacts.media,
            vec![DeletedArtifact {
                identity: identity.clone(),
                path: "shared.bin".into()
            }]
        );
        assert_eq!(
            artifacts.archives,
            vec![DeletedArtifact {
                identity: identity.clone(),
                path: "archive".into()
            }]
        );
        assert!(!store.references_archive(&identity, "archive").unwrap());
        assert!(store.receipts(&identity, 1001).unwrap().is_empty());
        assert!(
            matches!(try_reserve_admission(), Err(AdmissionDbError::Busy)),
            "cleanup owner still excludes staging"
        );
        let reopened = DurableInboundStore::from_pool(&pool).unwrap();
        let pending = reopened
            .pending_deleted_artifacts(&guard, Some(&identity), 0, 256)
            .unwrap();
        assert_eq!(
            pending.len(),
            2,
            "crash after DB commit retains both cleanup candidates"
        );
        assert!(
            pending
                .iter()
                .any(|entry| entry.kind == DeletedArtifactKind::Archive
                    && entry.artifact.path == "archive")
        );
        reopened
            .complete_deleted_artifacts(&guard, &[pending[0].id])
            .unwrap();
        assert_eq!(
            reopened
                .pending_deleted_artifacts(&guard, Some(&identity), 0, 256)
                .unwrap()
                .len(),
            1
        );
        drop(reopened);
        drop(guard);
        assert!(try_reserve_admission().is_ok());
        drop(store);
        drop(pool);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn deleting_without_an_identity_is_refused() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let identity = "11".repeat(16);
        let id = hex::encode([3; 32]);
        crate::db::try_save_message(
            &pool, &id, "peer", "local", "kept", "", 1.0, "received", "inbound", &identity, "", "",
            "", "", "", "", None,
        )
        .unwrap();
        for peer in [None, Some("peer")] {
            assert!(matches!(
                store.delete_messages("", peer, 1000),
                Err(AdmissionDbError::NoIdentity)
            ));
        }
        assert_eq!(
            crate::db::get_conversation(&pool, "peer", &identity, 10).len(),
            1
        );
        let tombstones: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM lxmf_user_deleted", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(tombstones, 0);
        let _ = std::fs::remove_dir_all(path);
    }

    /// A "received" message is inbound; any other state is outbound.
    fn message(
        pool: &crate::db::DbPool,
        identity: &str,
        peer: &str,
        id: &str,
        (state, method): (&str, Option<&str>),
        stored: &str,
    ) {
        let (source, destination, direction) = if state == "received" {
            (peer, "local", "inbound")
        } else {
            ("local", peer, "outbound")
        };
        crate::db::try_save_message(
            pool,
            id,
            source,
            destination,
            "text",
            "",
            1.0,
            state,
            direction,
            identity,
            if stored.is_empty() { "" } else { "file" },
            stored,
            "",
            "",
            "",
            "",
            method,
        )
        .unwrap();
        crate::db::save_reaction(pool, id, peer, "👍", identity);
    }

    fn count(pool: &crate::db::DbPool, sql: &str) -> i64 {
        pool.get()
            .unwrap()
            .query_row(sql, [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn selected_deletion_stays_within_identity_and_peer() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let (a, b) = ("11".repeat(16), "22".repeat(16));
        let [m1, m3, other] = [1u8, 3, 4].map(|n| hex::encode([n; 32]));
        let m2 = format!("out_{}", hex::encode([2u8; 16]));
        message(&pool, &a, "peer", &m1, ("received", None), "m1.bin");
        message(&pool, &a, "peer", &m2, ("delivered", None), "m2.bin");
        message(&pool, &a, "peer", &m3, ("received", None), "");
        message(&pool, &a, "other", &other, ("received", None), "");
        message(&pool, &b, "peer", &m1, ("received", None), "m1.bin");
        let guard = try_reserve_admission().unwrap();
        let ids = [&m1, &m2, &other, &"ff".repeat(32)].map(|id| id.to_string());
        let (artifacts, deleted) = store
            .delete_selected_messages_guarded(&guard, &a, "peer", &ids, 1001)
            .unwrap();
        assert_eq!(deleted, vec![m1.clone(), m2.clone()]);
        assert_eq!(
            artifacts.media,
            ["m1.bin", "m2.bin"].map(|path| DeletedArtifact {
                identity: a.clone(),
                path: path.into()
            })
        );
        assert!(!crate::db::message_exists_for_identity(&pool, &m1, &a));
        assert!(!crate::db::message_exists_for_identity(&pool, &m2, &a));
        assert!(crate::db::message_exists_for_identity(&pool, &m3, &a));
        assert!(crate::db::message_exists_for_identity(&pool, &other, &a));
        assert!(crate::db::message_exists_for_identity(&pool, &m1, &b));
        assert_eq!(
            count(&pool, "SELECT COUNT(*) FROM lxmf_user_deleted"),
            1,
            "tombstones only for deleted downloadable rows"
        );
        assert_eq!(
            count(
                &pool,
                &format!("SELECT COUNT(*) FROM reactions WHERE identity_id='{a}'")
            ),
            2
        );
        assert_eq!(
            crate::db::get_reactions_for_message(&pool, &m1, &b).len(),
            1
        );
        assert_eq!(
            store
                .pending_deleted_artifacts(&guard, Some(&b), 0, 256)
                .unwrap()
                .len(),
            0
        );
        let too_many = vec![m3.clone(); MAX_SELECTED_DELETION + 1];
        assert!(matches!(
            store.delete_selected_messages_guarded(&guard, &a, "peer", &too_many, 1001),
            Err(AdmissionDbError::TooLarge)
        ));
        drop(guard);
        drop(store);
        drop(pool);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn selected_deletion_refuses_the_whole_set_while_one_is_sending() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let a = "11".repeat(16);
        let [done, pending, direct, opportunistic, read] =
            [1u8, 2, 3, 4, 5].map(|n| format!("out_{}", hex::encode([n; 16])));
        message(&pool, &a, "peer", &done, ("failed", None), "");
        message(&pool, &a, "peer", &pending, ("sending", None), "");
        message(&pool, &a, "peer", &direct, ("sent", Some("direct")), "");
        message(&pool, &a, "peer", &opportunistic, ("sent", None), "");
        message(&pool, &a, "peer", &read, ("read", None), "");
        let guard = try_reserve_admission().unwrap();
        for sending in [&pending, &direct] {
            let ids = [done.clone(), sending.clone()];
            assert!(matches!(
                store.delete_selected_messages_guarded(&guard, &a, "peer", &ids, 1001),
                Err(AdmissionDbError::StillSending)
            ));
        }
        assert!(crate::db::message_exists_for_identity(&pool, &done, &a));
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM lxmf_user_deleted"), 0);
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM reactions"), 5);
        let ids = [done.clone(), opportunistic.clone(), read.clone()];
        let (_, deleted) = store
            .delete_selected_messages_guarded(&guard, &a, "peer", &ids, 1001)
            .unwrap();
        assert_eq!(deleted.len(), 3);
        drop(guard);
        drop(store);
        drop(pool);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn conversation_deletion_also_drops_its_reactions() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let (a, b) = ("11".repeat(16), "22".repeat(16));
        let [m1, m2] = [1u8, 2].map(|n| hex::encode([n; 32]));
        message(&pool, &a, "peer", &m1, ("received", None), "");
        message(&pool, &a, "other", &m2, ("received", None), "");
        message(&pool, &b, "peer", &m1, ("received", None), "");
        crate::db::hide_conversation(&pool, "peer", &a);
        store.delete_messages(&a, Some("peer"), 1000).unwrap();
        assert!(crate::db::get_reactions_for_message(&pool, &m1, &a).is_empty());
        assert_eq!(
            crate::db::get_reactions_for_message(&pool, &m2, &a).len(),
            1
        );
        assert_eq!(
            crate::db::get_reactions_for_message(&pool, &m1, &b).len(),
            1
        );
        assert_eq!(count(&pool, "SELECT COUNT(*) FROM hidden_conversations"), 0);
        store.delete_messages(&a, None, 1000).unwrap();
        assert!(crate::db::get_reactions_for_message(&pool, &m2, &a).is_empty());
        assert_eq!(
            crate::db::get_reactions_for_message(&pool, &m1, &b).len(),
            1
        );
        drop(store);
        drop(pool);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn deletion_tombstone_survives_expired_transient_authority() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let identity = "11".repeat(16);
        let id = hex::encode([3; 32]);
        crate::db::try_save_message(
            &pool, &id, "peer", "local", "old", "", 1.0, "received", "inbound", &identity, "", "",
            "", "", "", "", None,
        )
        .unwrap();
        store.delete_messages(&identity, None, 1000).unwrap();
        let receipt = store
            .commit(
                try_reserve_admission().unwrap(),
                plan(4, 5),
                |_, _, _| panic!("deleted archive used"),
                |_| panic!("deleted message recreated"),
            )
            .unwrap();
        assert_eq!(receipt.disposition, Disposition::UserDeleted);
        assert!(
            store
                .receipts(&identity, 1000 + POLICY_RECEIPT_SECONDS + 1)
                .unwrap()
                .is_empty()
        );
        let mut again = plan(6, 7);
        again.committed_at = 1000 + POLICY_RECEIPT_SECONDS + 2;
        assert_eq!(
            store
                .commit(
                    try_reserve_admission().unwrap(),
                    again,
                    |_, _, _| Ok(()),
                    |_| panic!("expired TID resurrected deletion")
                )
                .unwrap()
                .disposition,
            Disposition::UserDeleted
        );
        assert!(!crate::db::message_exists_for_identity(
            &pool, &id, &identity
        ));
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn reconciliation_changes_a_real_full_wal_barrier() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        store.reconcile_barrier().unwrap();
        let first: Vec<u8> = pool
            .get()
            .unwrap()
            .query_row("SELECT nonce FROM lxmf_admission_barrier", [], |r| r.get(0))
            .unwrap();
        store.reconcile_barrier().unwrap();
        let second: Vec<u8> = pool
            .get()
            .unwrap()
            .query_row("SELECT nonce FROM lxmf_admission_barrier", [], |r| r.get(0))
            .unwrap();
        assert_eq!(first.len(), 32);
        assert_ne!(first, second);
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn duplicate_requires_the_original_media_manifest_and_corruption_is_withheld() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let file = FileReference {
            name: "abc.bin".into(),
            hash: [7; 32],
            length: 73,
        };
        store
            .commit(
                try_reserve_admission().unwrap(),
                plan(4, 5).with_files(&[file]).unwrap(),
                |_, _, _| Ok(()),
                |_| Ok(EffectCommit::Applied("service")),
            )
            .unwrap();
        let failure = store.commit(
            try_reserve_admission().unwrap(),
            plan(6, 7),
            |_, _, files| {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].hash, [7; 32]);
                assert_eq!(files[0].length, 73);
                Err(AdmissionDbError::MissingEffect)
            },
            |_| panic!("duplicate media failure reapplied the message"),
        );
        assert!(failure.is_err());
        pool.get()
            .unwrap()
            .execute(
                "UPDATE lxmf_inbound_effects SET media_manifest=?1",
                ["x".repeat(16385)],
            )
            .unwrap();
        assert!(store.receipts(&"11".repeat(16), 1000).unwrap().is_empty());
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn game_receipt_requires_retained_action_and_user_delete_prevents_replay() {
        let _serial = SERIAL.lock().unwrap();
        let (path, pool, store) = fixture();
        let reference = GameReference {
            identity: "22".repeat(16),
            session: "session".into(),
            nonce: [1; 8],
            envelope: vec![0x81, 0xa1, b'x', 1],
        };
        assert!(
            store
                .commit(
                    try_reserve_admission().unwrap(),
                    plan(4, 5).with_game(&reference).unwrap(),
                    |_, _, _| Ok(()),
                    |_| Ok(EffectCommit::Applied("game"))
                )
                .is_err()
        );
        assert!(store.receipts(&"11".repeat(16), 1000).unwrap().is_empty());
        store.commit(try_reserve_admission().unwrap(),plan(4,5).with_game(&reference).unwrap(),|_,_,_|Ok(()),|conn| {
            conn.execute("INSERT INTO app_sessions(session_id,identity_id,app_id,contact_hash,status) VALUES('session',?1,'test','peer','completed')",[&reference.identity])?;
            conn.execute("INSERT INTO app_actions(session_id,identity_id,action_num,command,sender,envelope_mp) VALUES('session',?1,1,'move','peer',?2)",params![reference.identity,reference.envelope])?;
            Ok(EffectCommit::Applied("game"))
        }).unwrap();
        assert_eq!(store.receipts(&"11".repeat(16), 1000).unwrap().len(), 1);
        assert!(
            store
                .delete_game_session(&reference.identity, "session", 1001)
                .unwrap()
        );
        assert!(store.receipts(&"11".repeat(16), 1001).unwrap().is_empty());
        let receipt = store
            .commit(
                try_reserve_admission().unwrap(),
                plan(6, 7).with_game(&reference).unwrap(),
                |_, _, _| panic!("deleted game archive used"),
                |_| panic!("deleted game resurrected"),
            )
            .unwrap();
        assert_eq!(receipt.disposition, Disposition::UserDeleted);
        assert_eq!(
            pool.get()
                .unwrap()
                .query_row("SELECT count(*) FROM app_actions", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(pool);
        drop(store);
        std::fs::remove_dir_all(path).unwrap();
    }
}
