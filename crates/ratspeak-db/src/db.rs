use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, params};
use tokio::task::JoinError;

pub type DbPool = Pool<SqliteConnectionManager>;

const SCHEMA_VERSION: i64 = 37;

pub const PEER_SERVICE_LXMF_DELIVERY: &str = ratspeak_core::LXMF_DELIVERY_APP_NAME;
pub const PEER_SERVICE_LXST_TELEPHONY: &str = "lxst.telephony";
pub const PEER_SERVICE_RATSPEAK_CLIENT: &str = "ratspeak.client";
pub const PEER_SERVICE_RATSPEAK_GAMES: &str = "ratspeak.games";
pub const PEER_SERVICE_RATSPEAK_CHAT: &str = "ratspeak.chat";
pub const LXMF_COMPRESSION_SUPPORT_SUPPORTED: &str = "supported";
pub const LXMF_COMPRESSION_SUPPORT_UNSUPPORTED: &str = "unsupported";

const IDENTITY_SELECT_COLUMNS: &str = "hash,
    lxmf_hash,
    nickname,
    display_name,
    COALESCE(status, '') AS status,
    created_at,
    last_used,
    is_active,
    propagation_node,
    propagation_enabled,
    propagation_mode,
    propagation_auto_favor_static";

const POOL_MAX_SIZE: u32 = 32;

/// Run sync `db::*` work on the blocking pool. Wrap multi-statement critical
/// sections in a single call so they share one `Connection`.
pub async fn spawn_db<F, R>(pool: DbPool, f: F) -> Result<R, JoinError>
where
    F: FnOnce(DbPool) -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(move || f(pool)).await
}

fn now_ts() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub fn init_pool(data_dir: &Path) -> Result<DbPool, Box<dyn std::error::Error + Send + Sync>> {
    let ratspeak_dir = data_dir.join(".ratspeak");
    std::fs::create_dir_all(&ratspeak_dir)?;

    let db_path = ratspeak_dir.join("ratspeak.db");
    let manager = SqliteConnectionManager::file(&db_path).with_init(|conn| {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA busy_timeout=30000;
                 PRAGMA synchronous=NORMAL;",
        )
    });
    let pool = Pool::builder().max_size(POOL_MAX_SIZE).build(manager)?;

    tracing::info!("Database pool initialized");
    Ok(pool)
}

pub fn init_schema(pool: &DbPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let conn = pool.get()?;

    let has_schema: bool = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='schema_version'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;

    if has_schema {
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);

        if version < SCHEMA_VERSION {
            run_migrations(&conn, version)?;
        }
    }

    conn.execute_batch(SCHEMA_SQL)?;

    let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))?;
    if count == 0 {
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;
    }

    tracing::info!("Database schema initialized (version {SCHEMA_VERSION})");
    Ok(())
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS identities (
    hash TEXT PRIMARY KEY,
    lxmf_hash TEXT,
    nickname TEXT DEFAULT '',
    display_name TEXT DEFAULT '',
    status TEXT NOT NULL DEFAULT '',
    created_at REAL NOT NULL,
    last_used REAL,
    is_active INTEGER DEFAULT 0,
    propagation_node TEXT DEFAULT '',
    propagation_enabled INTEGER DEFAULT 0,
    propagation_mode TEXT NOT NULL DEFAULT 'auto',
    propagation_auto_favor_static INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE IF NOT EXISTS contacts (
    dest_hash TEXT NOT NULL,
    identity_id TEXT DEFAULT '',
    display_name TEXT,
    identity_pubkey TEXT,
    first_seen REAL,
    last_seen REAL,
    trust TEXT DEFAULT 'pending',
    notes TEXT DEFAULT '',
    UNIQUE(dest_hash, identity_id)
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT NOT NULL,
    source TEXT NOT NULL,
    destination TEXT NOT NULL,
    content TEXT DEFAULT '',
    title TEXT DEFAULT '',
    timestamp REAL NOT NULL,
    state TEXT DEFAULT 'unknown',
    direction TEXT DEFAULT 'outbound',
    rtt_ms REAL,
    hops INTEGER,
    path TEXT,
    identity_id TEXT NOT NULL DEFAULT '',
    attachment_name TEXT DEFAULT '',
    attachment_stored_name TEXT DEFAULT '',
    image_name TEXT DEFAULT '',
    image_stored_name TEXT DEFAULT '',
    reply_to_id TEXT DEFAULT '',
    reply_to_preview TEXT DEFAULT '',
    game_id TEXT DEFAULT '',
    game_action TEXT DEFAULT '',
    game_move_san TEXT DEFAULT '',
    delivery_method TEXT,
    PRIMARY KEY (id, identity_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_dest ON messages(destination);
CREATE INDEX IF NOT EXISTS idx_messages_source ON messages(source);
CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
CREATE INDEX IF NOT EXISTS idx_messages_identity ON messages(identity_id);
CREATE INDEX IF NOT EXISTS idx_messages_identity_ts ON messages(identity_id, timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_messages_unread ON messages(identity_id, direction, state, source);
CREATE INDEX IF NOT EXISTS idx_contacts_dest_identity ON contacts(dest_hash, identity_id);
CREATE INDEX IF NOT EXISTS idx_messages_identity_state ON messages(identity_id, state);
CREATE INDEX IF NOT EXISTS idx_messages_source_identity ON messages(source, identity_id, timestamp ASC);
CREATE INDEX IF NOT EXISTS idx_messages_dest_identity ON messages(destination, identity_id, timestamp ASC);

CREATE TABLE IF NOT EXISTS hidden_conversations (
    dest_hash TEXT NOT NULL,
    identity_id TEXT NOT NULL DEFAULT '',
    hidden_at REAL,
    PRIMARY KEY (dest_hash, identity_id)
);

CREATE TABLE IF NOT EXISTS blocked_contacts (
    dest_hash TEXT NOT NULL,
    identity_id TEXT NOT NULL DEFAULT '',
    display_name TEXT DEFAULT '',
    blocked_at REAL,
    PRIMARY KEY (dest_hash, identity_id)
);

-- Queue of escalations awaiting an announce. When the user blocks + escalates
-- to network blackhole but we have not yet seen the contact's identity, we
-- store the LXMF dest hash here. The announce-handler resolves and escalates
-- on first sighting, then deletes the row.
CREATE TABLE IF NOT EXISTS pending_blackholes (
    dest_hash       TEXT NOT NULL,
    identity_id     TEXT NOT NULL DEFAULT '',
    reason_label    TEXT DEFAULT NULL,
    ttl_seconds     REAL DEFAULT NULL,
    queued_at       REAL NOT NULL,
    PRIMARY KEY (dest_hash, identity_id)
);
CREATE INDEX IF NOT EXISTS idx_pending_blackholes_dest ON pending_blackholes(dest_hash);
CREATE INDEX IF NOT EXISTS idx_pending_blackholes_identity ON pending_blackholes(identity_id);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE IF NOT EXISTS connection_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    name TEXT DEFAULT '',
    last_used REAL NOT NULL,
    times_used INTEGER DEFAULT 1,
    UNIQUE(host, port)
);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content, title, id UNINDEXED, identity_id UNINDEXED,
    content='messages', content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content, title, id, identity_id)
    VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
END;

CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
    VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
END;

DROP TRIGGER IF EXISTS messages_au;

CREATE TRIGGER messages_au AFTER UPDATE OF content, title ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
    VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
    INSERT INTO messages_fts(rowid, content, title, id, identity_id)
    VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
END;

CREATE TABLE IF NOT EXISTS reactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    sender TEXT NOT NULL,
    emoji TEXT NOT NULL,
    timestamp REAL NOT NULL,
    identity_id TEXT DEFAULT '',
    UNIQUE(message_id, sender, emoji, identity_id)
);

CREATE INDEX IF NOT EXISTS idx_reactions_msg ON reactions(message_id);

CREATE TABLE IF NOT EXISTS games (
    game_id TEXT NOT NULL,
    game TEXT NOT NULL,
    contact_hash TEXT NOT NULL,
    identity_id TEXT DEFAULT '',
    challenger TEXT NOT NULL,
    state TEXT DEFAULT '',
    status TEXT DEFAULT 'pending',
    winner TEXT DEFAULT '',
    turn TEXT DEFAULT '',
    first_turn TEXT DEFAULT '',
    move_count INTEGER DEFAULT 0,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (game_id, identity_id)
);

CREATE INDEX IF NOT EXISTS idx_games_contact ON games(contact_hash, identity_id);
CREATE INDEX IF NOT EXISTS idx_games_status ON games(status);

CREATE TABLE IF NOT EXISTS app_sessions (
    session_id    TEXT NOT NULL,
    identity_id   TEXT NOT NULL DEFAULT '',
    app_id        TEXT NOT NULL,
    app_version   INTEGER NOT NULL DEFAULT 1,
    contact_hash  TEXT NOT NULL,
    initiator     TEXT NOT NULL DEFAULT '',
    status        TEXT NOT NULL DEFAULT 'pending',
    metadata      TEXT NOT NULL DEFAULT '{}',
    unread        INTEGER NOT NULL DEFAULT 0,
    created_at    REAL NOT NULL DEFAULT 0,
    updated_at    REAL NOT NULL DEFAULT 0,
    last_action_at REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, identity_id)
);

CREATE INDEX IF NOT EXISTS idx_app_sessions_contact ON app_sessions(contact_hash, identity_id);
CREATE INDEX IF NOT EXISTS idx_app_sessions_status ON app_sessions(status);
CREATE INDEX IF NOT EXISTS idx_app_sessions_app ON app_sessions(app_id);

CREATE TABLE IF NOT EXISTS app_actions (
    session_id    TEXT NOT NULL,
    identity_id   TEXT NOT NULL DEFAULT '',
    action_num    INTEGER NOT NULL,
    command       TEXT NOT NULL,
    payload_json  TEXT NOT NULL DEFAULT '{}',
    sender        TEXT NOT NULL,
    timestamp     REAL NOT NULL DEFAULT 0,
    -- Packed LRGP envelope, populated for outbound actions so the manual
    -- "Resend last move" path can re-transmit without re-dispatching.
    envelope_mp   BLOB,
    UNIQUE (session_id, identity_id, action_num)
);

-- Sidecar to the on-disk known_identities binary file; avoids per-announce
-- full-file rewrites. Display-name precedence: `contacts.display_name` over
-- `identity_activity.display_name`.
CREATE TABLE IF NOT EXISTS identity_activity (
    dest_hash      TEXT PRIMARY KEY,
    identity_hash  TEXT NOT NULL DEFAULT '',
    last_seen      REAL NOT NULL,
    first_seen     REAL NOT NULL,
    announce_count INTEGER NOT NULL DEFAULT 1,
    display_name   TEXT NOT NULL DEFAULT '',
    status         TEXT NOT NULL DEFAULT '',
    last_interface TEXT NOT NULL DEFAULT '',
    services       TEXT NOT NULL DEFAULT '',
    lxmf_compression_support TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_identity_activity_last_seen ON identity_activity(last_seen);

-- Channels persists user intent and connection conveniences, never observed
-- room membership or traffic. desired_* is the durable scheduler input;
-- actual Link and JOIN state remains runtime-owned.
CREATE TABLE IF NOT EXISTS channel_hubs (
    identity_id       TEXT NOT NULL,
    destination_hash  TEXT NOT NULL,
    label             TEXT NOT NULL DEFAULT '',
    nickname          TEXT NOT NULL DEFAULT '',
    added_at           REAL NOT NULL,
    last_connected    REAL NOT NULL DEFAULT 0,
    desired_connected INTEGER NOT NULL DEFAULT 0
        CHECK (desired_connected IN (0, 1)),
    PRIMARY KEY (identity_id, destination_hash),
    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS channel_rooms (
    identity_id          TEXT NOT NULL,
    hub_destination_hash TEXT NOT NULL,
    room_name            TEXT NOT NULL,
    added_at              REAL NOT NULL,
    last_joined           REAL NOT NULL DEFAULT 0,
    desired_joined        INTEGER NOT NULL DEFAULT 0
        CHECK (desired_joined IN (0, 1)),
    join_key_required     INTEGER NOT NULL DEFAULT 0
        CHECK (join_key_required IN (0, 1)),
    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
    FOREIGN KEY (identity_id, hub_destination_hash)
        REFERENCES channel_hubs(identity_id, destination_hash) ON DELETE CASCADE
);

-- Recoverable client join keys are encrypted to the owning Reticulum identity
-- before reaching SQLite. Keep ciphertext separate from ordinary room metadata
-- so it can never leak through the bookmark API or its Debug/Serialize shapes.
CREATE TABLE IF NOT EXISTS channel_room_secrets (
    identity_id          TEXT NOT NULL,
    hub_destination_hash TEXT NOT NULL,
    room_name            TEXT NOT NULL,
    seal_scheme          TEXT NOT NULL,
    seal_version         INTEGER NOT NULL CHECK (seal_version > 0),
    ciphertext           BLOB NOT NULL CHECK (length(ciphertext) > 0),
    updated_at           REAL NOT NULL,
    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
    FOREIGN KEY (identity_id, hub_destination_hash, room_name)
        REFERENCES channel_rooms(identity_id, hub_destination_hash, room_name)
        ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_channel_hubs_identity_recent
    ON channel_hubs(identity_id, last_connected DESC);
-- The first scheduler deliberately budgets one live hub. Enforce that in the
-- durable layer so concurrent callers or a crash cannot create two winners.
CREATE UNIQUE INDEX IF NOT EXISTS idx_channel_hubs_identity_desired
    ON channel_hubs(identity_id) WHERE desired_connected = 1;
CREATE INDEX IF NOT EXISTS idx_channel_rooms_identity_hub
    ON channel_rooms(identity_id, hub_destination_hash, room_name);

-- Hub registry: the rooms this node hosts and the operator policy on them.
-- Durable policy only; relayed traffic never lands here. Join keys persist as
-- a verifiable digest, never as a recoverable key.
CREATE TABLE IF NOT EXISTS channel_hub_rooms (
    identity_id      TEXT NOT NULL,
    room_name        TEXT NOT NULL,
    topic            TEXT NOT NULL DEFAULT '',
    key_salt         TEXT NOT NULL DEFAULT '',
    key_mac          TEXT NOT NULL DEFAULT '',
    key_pepper_id    TEXT NOT NULL DEFAULT '',
    moderated        INTEGER NOT NULL DEFAULT 0,
    invite_only      INTEGER NOT NULL DEFAULT 0,
    topic_ops_only   INTEGER NOT NULL DEFAULT 0,
    no_outside_msgs  INTEGER NOT NULL DEFAULT 0,
    private          INTEGER NOT NULL DEFAULT 0,
    created_at       REAL NOT NULL,
    last_used        REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (identity_id, room_name),
    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
);

-- Per-room grants. kind is op|voice|ban|invite; expires_at is 0 for permanent
-- grants and an absolute unix time for invites.
CREATE TABLE IF NOT EXISTS channel_hub_grants (
    identity_id  TEXT NOT NULL,
    room_name    TEXT NOT NULL,
    kind         TEXT NOT NULL,
    subject      TEXT NOT NULL,
    granted_at   REAL NOT NULL,
    expires_at   REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (identity_id, room_name, kind, subject),
    FOREIGN KEY (identity_id, room_name)
        REFERENCES channel_hub_rooms(identity_id, room_name) ON DELETE CASCADE
);

-- Hub-level identity bans (/kline).
CREATE TABLE IF NOT EXISTS channel_hub_klines (
    identity_id  TEXT NOT NULL,
    subject      TEXT NOT NULL,
    banned_at    REAL NOT NULL,
    PRIMARY KEY (identity_id, subject),
    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_contacts_identity ON contacts(identity_id);
CREATE INDEX IF NOT EXISTS idx_contacts_identity_name ON contacts(identity_id, display_name);
CREATE INDEX IF NOT EXISTS idx_blocked_identity ON blocked_contacts(identity_id);
CREATE INDEX IF NOT EXISTS idx_identities_active ON identities(is_active) WHERE is_active = 1;
"#;

/// Run one schema-version step inside an explicit transaction so a crash
/// mid-step (especially multi-statement table rebuilds) rolls back atomically
/// instead of leaving a half-migrated schema with the version un-bumped.
fn migration_step(
    conn: &Connection,
    to_version: i64,
    apply: impl FnOnce(&Connection) -> Result<(), rusqlite::Error>,
) -> Result<(), rusqlite::Error> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match apply(conn) {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            if conn.execute_batch("ROLLBACK").is_err() {
                tracing::error!(
                    to_version,
                    reason = "rollback_failed",
                    "migration rollback failed"
                );
            }
            tracing::error!(
                to_version,
                reason = "migration_failed",
                "migration step failed; rolled back"
            );
            Err(e)
        }
    }
}

fn run_migrations(conn: &Connection, from_version: i64) -> Result<(), rusqlite::Error> {
    if from_version < 2 {
        migration_step(conn, 2, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS connection_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                host TEXT NOT NULL,
                port INTEGER NOT NULL,
                name TEXT DEFAULT '',
                last_used REAL NOT NULL,
                times_used INTEGER DEFAULT 1,
                UNIQUE(host, port)
            );
            UPDATE schema_version SET version = 2;",
            )?;
            tracing::info!("Migrated to schema version 2 (connection_history)");
            Ok(())
        })?;
    }

    if from_version < 3 {
        migration_step(conn, 3, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS identities (
                hash TEXT PRIMARY KEY,
                lxmf_hash TEXT,
                nickname TEXT DEFAULT '',
                display_name TEXT DEFAULT '',
                created_at REAL NOT NULL,
                last_used REAL,
                is_active INTEGER DEFAULT 0,
                propagation_node TEXT DEFAULT '',
                propagation_enabled INTEGER DEFAULT 0
            );",
            )?;

            let has_identity_id = {
                let mut stmt = conn.prepare("PRAGMA table_info(contacts)")?;
                let cols: Vec<String> = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(|r| r.ok())
                    .collect();
                cols.iter().any(|c| c == "identity_id")
            };

            if !has_identity_id {
                conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS contacts_new (
                    dest_hash TEXT NOT NULL,
                    identity_id TEXT DEFAULT '',
                    display_name TEXT,
                    identity_pubkey TEXT,
                    first_seen REAL,
                    last_seen REAL,
                    trust TEXT DEFAULT 'pending',
                    notes TEXT DEFAULT '',
                    UNIQUE(dest_hash, identity_id)
                );
                INSERT OR IGNORE INTO contacts_new
                    (dest_hash, identity_id, display_name, identity_pubkey, first_seen, last_seen, trust, notes)
                SELECT dest_hash, '', display_name, identity_pubkey, first_seen, last_seen, trust, notes
                FROM contacts;
                DROP TABLE contacts;
                ALTER TABLE contacts_new RENAME TO contacts;"
            )?;
            }

            let has_msg_identity = {
                let mut stmt = conn.prepare("PRAGMA table_info(messages)")?;
                let cols: Vec<String> = stmt
                    .query_map([], |row| row.get::<_, String>(1))?
                    .filter_map(|r| r.ok())
                    .collect();
                cols.iter().any(|c| c == "identity_id")
            };
            if !has_msg_identity {
                conn.execute_batch("ALTER TABLE messages ADD COLUMN identity_id TEXT DEFAULT ''")?;
            }

            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_messages_identity ON messages(identity_id);
             UPDATE schema_version SET version = 3;",
            )?;
            tracing::info!("Migrated to schema version 3 (identities)");
            Ok(())
        })?;
    }

    if from_version < 4 {
        migration_step(conn, 4, |conn| {
            let msg_cols = get_column_names(conn, "messages")?;
            for col in &[
                "attachment_name",
                "attachment_stored_name",
                "image_name",
                "image_stored_name",
            ] {
                if !msg_cols.iter().any(|c| c == col) {
                    conn.execute_batch(&format!(
                        "ALTER TABLE messages ADD COLUMN {col} TEXT DEFAULT ''"
                    ))?;
                }
            }
            conn.execute_batch("UPDATE schema_version SET version = 4;")?;
            tracing::info!("Migrated to schema version 4 (attachment columns)");
            Ok(())
        })?;
    }

    if from_version < 5 {
        migration_step(conn, 5, |conn| {
            conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content, title, id UNINDEXED, identity_id UNINDEXED,
                content='messages', content_rowid='rowid'
            );
            CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, title, id, identity_id)
                VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
                VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
            END;
            CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE OF content, title ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
                VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
                INSERT INTO messages_fts(rowid, content, title, id, identity_id)
                VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
            END;
            INSERT INTO messages_fts(messages_fts) VALUES('rebuild');
            UPDATE schema_version SET version = 5;"
        )?;
            tracing::info!("Migrated to schema version 5 (FTS5)");
            Ok(())
        })?;
    }

    if from_version < 6 {
        migration_step(conn, 6, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS reactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                sender TEXT NOT NULL,
                emoji TEXT NOT NULL,
                timestamp REAL NOT NULL,
                identity_id TEXT DEFAULT '',
                UNIQUE(message_id, sender, emoji, identity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_reactions_msg ON reactions(message_id);",
            )?;
            let msg_cols = get_column_names(conn, "messages")?;
            if !msg_cols.iter().any(|c| c == "reply_to_id") {
                conn.execute_batch("ALTER TABLE messages ADD COLUMN reply_to_id TEXT DEFAULT ''")?;
            }
            if !msg_cols.iter().any(|c| c == "reply_to_preview") {
                conn.execute_batch(
                    "ALTER TABLE messages ADD COLUMN reply_to_preview TEXT DEFAULT ''",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 6;")?;
            tracing::info!("Migrated to schema version 6 (reactions, reply-to)");
            Ok(())
        })?;
    }

    if from_version < 7 {
        migration_step(conn, 7, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS games (
                game_id TEXT PRIMARY KEY,
                game TEXT NOT NULL,
                contact_hash TEXT NOT NULL,
                identity_id TEXT DEFAULT '',
                challenger TEXT NOT NULL,
                state TEXT DEFAULT '',
                status TEXT DEFAULT 'pending',
                winner TEXT DEFAULT '',
                turn TEXT DEFAULT '',
                first_turn TEXT DEFAULT 'challenger',
                move_count INTEGER DEFAULT 0,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_games_contact ON games(contact_hash, identity_id);
            CREATE INDEX IF NOT EXISTS idx_games_status ON games(status);",
            )?;
            let msg_cols = get_column_names(conn, "messages")?;
            if !msg_cols.iter().any(|c| c == "game_id") {
                conn.execute_batch("ALTER TABLE messages ADD COLUMN game_id TEXT DEFAULT ''")?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 7;")?;
            tracing::info!("Migrated to schema version 7 (games)");
            Ok(())
        })?;
    }

    if from_version < 8 {
        migration_step(conn, 8, |conn| {
            let game_cols = get_column_names(conn, "games")?;
            if !game_cols.iter().any(|c| c == "first_turn") {
                conn.execute_batch(
                    "ALTER TABLE games ADD COLUMN first_turn TEXT DEFAULT 'challenger'",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 8;")?;
            tracing::info!("Migrated to schema version 8 (first_turn)");
            Ok(())
        })?;
    }

    if from_version < 9 {
        migration_step(conn, 9, |conn| {
            let mut stmt = conn.prepare(
            "SELECT game_id, identity_id, challenger, contact_hash, turn, first_turn, winner FROM games"
        )?;
            let rows: Vec<(String, String, String, String, String, String, String)> = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2).unwrap_or_default(),
                        row.get::<_, String>(3).unwrap_or_default(),
                        row.get::<_, String>(4).unwrap_or_default(),
                        row.get::<_, String>(5).unwrap_or_default(),
                        row.get::<_, String>(6).unwrap_or_default(),
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();

            for (gid, iid, ch, co, turn, first_turn, winner) in rows {
                let new_turn = match turn.as_str() {
                    "challenger" => &ch,
                    "opponent" => &co,
                    _ => &turn,
                };
                let new_first = match first_turn.as_str() {
                    "challenger" => &ch,
                    "opponent" => &co,
                    _ => &first_turn,
                };
                let new_winner = match winner.as_str() {
                    "challenger" => &ch,
                    "opponent" => &co,
                    _ => &winner,
                };
                conn.execute(
                "UPDATE games SET turn = ?1, first_turn = ?2, winner = ?3 WHERE game_id = ?4 AND identity_id = ?5",
                params![new_turn, new_first, new_winner, gid, iid],
            )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 9;")?;
            tracing::info!("Migrated to schema version 9 (role→hash)");
            Ok(())
        })?;
    }

    if from_version < 10 {
        migration_step(conn, 10, |conn| {
            conn.execute_batch(
                "DROP TABLE IF EXISTS games;
            CREATE TABLE IF NOT EXISTS games (
                game_id TEXT NOT NULL,
                game TEXT NOT NULL,
                contact_hash TEXT NOT NULL,
                identity_id TEXT DEFAULT '',
                challenger TEXT NOT NULL,
                state TEXT DEFAULT '',
                status TEXT DEFAULT 'pending',
                winner TEXT DEFAULT '',
                turn TEXT DEFAULT '',
                first_turn TEXT DEFAULT '',
                move_count INTEGER DEFAULT 0,
                created_at REAL NOT NULL,
                updated_at REAL NOT NULL,
                PRIMARY KEY (game_id, identity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_games_contact ON games(contact_hash, identity_id);
            CREATE INDEX IF NOT EXISTS idx_games_status ON games(status);
            UPDATE schema_version SET version = 10;",
            )?;
            tracing::info!("Migrated to schema version 10 (games composite PK)");
            Ok(())
        })?;
    }

    if from_version < 11 {
        migration_step(conn, 11, |conn| {
            let msg_cols = get_column_names(conn, "messages")?;
            if !msg_cols.iter().any(|c| c == "game_action") {
                conn.execute_batch("ALTER TABLE messages ADD COLUMN game_action TEXT DEFAULT ''")?;
            }
            if !msg_cols.iter().any(|c| c == "game_move_san") {
                conn.execute_batch(
                    "ALTER TABLE messages ADD COLUMN game_move_san TEXT DEFAULT ''",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 11;")?;
            tracing::info!("Migrated to schema version 11 (game_action columns)");
            Ok(())
        })?;
    }

    if from_version < 12 {
        migration_step(conn, 12, |conn| {
            conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS app_sessions (
                session_id    TEXT NOT NULL,
                identity_id   TEXT NOT NULL DEFAULT '',
                app_id        TEXT NOT NULL,
                app_version   INTEGER NOT NULL DEFAULT 1,
                contact_hash  TEXT NOT NULL,
                initiator     TEXT NOT NULL DEFAULT '',
                status        TEXT NOT NULL DEFAULT 'pending',
                metadata      TEXT NOT NULL DEFAULT '{}',
                unread        INTEGER NOT NULL DEFAULT 0,
                created_at    REAL NOT NULL DEFAULT 0,
                updated_at    REAL NOT NULL DEFAULT 0,
                last_action_at REAL NOT NULL DEFAULT 0,
                PRIMARY KEY (session_id, identity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_app_sessions_contact ON app_sessions(contact_hash, identity_id);
            CREATE INDEX IF NOT EXISTS idx_app_sessions_status ON app_sessions(status);
            CREATE INDEX IF NOT EXISTS idx_app_sessions_app ON app_sessions(app_id);
            CREATE TABLE IF NOT EXISTS app_actions (
                session_id    TEXT NOT NULL,
                identity_id   TEXT NOT NULL DEFAULT '',
                action_num    INTEGER NOT NULL,
                command       TEXT NOT NULL,
                payload_json  TEXT NOT NULL DEFAULT '{}',
                sender        TEXT NOT NULL,
                timestamp     REAL NOT NULL DEFAULT 0,
                UNIQUE (session_id, identity_id, action_num)
            );
            UPDATE schema_version SET version = 12;"
        )?;
            tracing::info!("Migrated to schema version 12 (LRGP tables)");
            Ok(())
        })?;
    }

    if from_version < 13 {
        migration_step(conn, 13, |conn| {
            conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_contacts_dest_identity ON contacts(dest_hash, identity_id);
            CREATE INDEX IF NOT EXISTS idx_messages_identity_state ON messages(identity_id, state);
            UPDATE schema_version SET version = 13;"
        )?;
            tracing::info!("Migrated to schema version 13 (additional indexes)");
            Ok(())
        })?;
    }

    if from_version < 14 {
        migration_step(conn, 14, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS blocked_contacts (
                dest_hash TEXT NOT NULL,
                identity_id TEXT NOT NULL DEFAULT '',
                display_name TEXT DEFAULT '',
                blocked_at REAL,
                PRIMARY KEY (dest_hash, identity_id)
            );
            UPDATE schema_version SET version = 14;",
            )?;
            tracing::info!("Migrated to schema version 14 (blocked_contacts)");
            Ok(())
        })?;
    }

    if from_version < 15 {
        migration_step(conn, 15, |conn| {
            conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_messages_source_identity ON messages(source, identity_id, timestamp ASC);
             CREATE INDEX IF NOT EXISTS idx_messages_dest_identity ON messages(destination, identity_id, timestamp ASC);
             UPDATE schema_version SET version = 15;"
        )?;
            tracing::info!("Migrated to schema version 15 (conversation query indexes)");
            Ok(())
        })?;
    }

    if from_version < 16 {
        migration_step(conn, 16, |conn| {
            // Backfill last_seen/first_seen from messages table.
            conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS identity_activity (
                 dest_hash      TEXT PRIMARY KEY,
                 last_seen      REAL NOT NULL,
                 first_seen     REAL NOT NULL,
                 announce_count INTEGER NOT NULL DEFAULT 1
             );
             CREATE INDEX IF NOT EXISTS idx_identity_activity_last_seen ON identity_activity(last_seen);

             CREATE INDEX IF NOT EXISTS idx_contacts_identity ON contacts(identity_id);
             CREATE INDEX IF NOT EXISTS idx_contacts_identity_name ON contacts(identity_id, display_name);
             CREATE INDEX IF NOT EXISTS idx_blocked_identity ON blocked_contacts(identity_id);
             CREATE INDEX IF NOT EXISTS idx_identities_active ON identities(is_active) WHERE is_active = 1;

             INSERT INTO identity_activity(dest_hash, last_seen, first_seen, announce_count)
             SELECT source, MAX(timestamp), MIN(timestamp), 0
             FROM messages
             WHERE source != ''
             GROUP BY source
             ON CONFLICT(dest_hash) DO UPDATE SET
                 last_seen  = MAX(excluded.last_seen,  last_seen),
                 first_seen = MIN(excluded.first_seen, first_seen);

             INSERT INTO identity_activity(dest_hash, last_seen, first_seen, announce_count)
             SELECT destination, MAX(timestamp), MIN(timestamp), 0
             FROM messages
             WHERE destination != ''
             GROUP BY destination
             ON CONFLICT(dest_hash) DO UPDATE SET
                 last_seen  = MAX(excluded.last_seen,  last_seen),
                 first_seen = MIN(excluded.first_seen, first_seen);

             UPDATE schema_version SET version = 16;"
        )?;
            tracing::info!("Migrated to schema version 16 (identity_activity + scaling indexes)");
            Ok(())
        })?;
    }

    if from_version < 17 {
        migration_step(conn, 17, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS lrgp_pending_sends (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id           TEXT NOT NULL,
                identity_id          TEXT NOT NULL,
                contact_hash         TEXT NOT NULL,
                app_id               TEXT NOT NULL,
                command              TEXT NOT NULL,
                envelope_mp          BLOB NOT NULL,
                envelope_hash        TEXT NOT NULL,
                fallback_text        TEXT NOT NULL,
                session_snapshot_json TEXT,
                first_attempt_at     REAL NOT NULL,
                last_attempt_at      REAL NOT NULL,
                attempt_count        INTEGER NOT NULL DEFAULT 0,
                last_transport_tried TEXT,
                msg_id               TEXT,
                UNIQUE (session_id, identity_id, command, envelope_hash)
            );
            CREATE INDEX IF NOT EXISTS idx_lrgp_pending_session
                ON lrgp_pending_sends(session_id, identity_id);
            UPDATE schema_version SET version = 17;",
            )?;
            tracing::info!("Migrated to schema version 17 (lrgp_pending_sends)");
            Ok(())
        })?;
    }

    if from_version < 18 {
        migration_step(conn, 18, |conn| {
            // Self-heal: empty session_id rows orphan the frontend `_allSessions` map.
            let sessions_removed =
                conn.execute("DELETE FROM app_sessions WHERE session_id = ''", [])?;
            let actions_removed =
                conn.execute("DELETE FROM app_actions WHERE session_id = ''", [])?;
            conn.execute_batch("UPDATE schema_version SET version = 18;")?;
            tracing::info!(
                "Migrated to schema version 18 (pruned {sessions_removed} empty-SID sessions, \
             {actions_removed} empty-SID actions)"
            );
            Ok(())
        })?;
    }

    if from_version < 19 {
        migration_step(conn, 19, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS identity_interface_activity (
                dest_hash      TEXT NOT NULL,
                interface_name TEXT NOT NULL,
                last_seen      REAL NOT NULL,
                first_seen     REAL NOT NULL,
                PRIMARY KEY (dest_hash, interface_name)
            );
            CREATE INDEX IF NOT EXISTS idx_iia_interface
                ON identity_interface_activity(interface_name);
            UPDATE schema_version SET version = 19;",
            )?;
            tracing::info!(
                "Migrated to schema version 19 (identity_interface_activity for per-interface peer tracking)"
            );
            Ok(())
        })?;
    }

    if from_version < 20 {
        migration_step(conn, 20, |conn| {
            // Unify peers on identity_activity; drop identity_interface_activity.
            let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
            if !cols.iter().any(|c| c == "display_name") {
                conn.execute_batch(
                    "ALTER TABLE identity_activity
                    ADD COLUMN display_name TEXT NOT NULL DEFAULT '';",
                )?;
            }
            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_iia_interface;
             DROP TABLE IF EXISTS identity_interface_activity;

             UPDATE identity_activity
                SET display_name = (
                    SELECT display_name FROM contacts
                     WHERE contacts.dest_hash = identity_activity.dest_hash
                     LIMIT 1
                )
              WHERE display_name = ''
                AND EXISTS (
                    SELECT 1 FROM contacts
                     WHERE contacts.dest_hash = identity_activity.dest_hash
                       AND COALESCE(contacts.display_name, '') != ''
                );

             UPDATE schema_version SET version = 20;",
            )?;
            tracing::info!(
                "Migrated to schema version 20 (display_name on identity_activity, dropped identity_interface_activity)"
            );
            Ok(())
        })?;
    }

    if from_version < 21 {
        migration_step(conn, 21, |conn| {
            // Add `last_interface`; required by v22's DROP COLUMN below.
            let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
            if !cols.iter().any(|c| c == "last_interface") {
                conn.execute_batch(
                    "ALTER TABLE identity_activity
                    ADD COLUMN last_interface TEXT NOT NULL DEFAULT '';",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 21;")?;
            tracing::info!("Migrated to schema version 21 (last_interface on identity_activity)");
            Ok(())
        })?;
    }

    if from_version < 22 {
        migration_step(conn, 22, |conn| {
            let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
            if cols.iter().any(|c| c == "last_interface") {
                conn.execute_batch("ALTER TABLE identity_activity DROP COLUMN last_interface;")?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 22;")?;
            tracing::info!("Migrated to schema version 22 (dropped last_interface)");
            Ok(())
        })?;
    }

    if from_version < 23 {
        migration_step(conn, 23, |conn| {
            // Re-add `last_interface`; stamped atomically with `last_seen` per announce.
            let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
            if !cols.iter().any(|c| c == "last_interface") {
                conn.execute_batch(
                    "ALTER TABLE identity_activity
                    ADD COLUMN last_interface TEXT NOT NULL DEFAULT '';",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 23;")?;
            tracing::info!(
                "Migrated to schema version 23 (last_interface restored, atomic with announce)"
            );
            Ok(())
        })?;
    }

    if from_version < 24 {
        migration_step(conn, 24, |conn| {
            // Add propagation Off/Auto/Manual mode + favor_static.
            // Pre-existing `propagation_node` and `propagation_enabled` preserved;
            // `enable_propagation` becomes a shim mapping to mode.
            let cols = get_column_names(conn, "identities").unwrap_or_default();
            if !cols.iter().any(|c| c == "propagation_mode") {
                conn.execute_batch(
                    "ALTER TABLE identities
                    ADD COLUMN propagation_mode TEXT NOT NULL DEFAULT 'auto';",
                )?;
            }
            if !cols.iter().any(|c| c == "propagation_auto_favor_static") {
                conn.execute_batch(
                    "ALTER TABLE identities
                    ADD COLUMN propagation_auto_favor_static INTEGER NOT NULL DEFAULT 1;",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 24;")?;
            tracing::info!("Migrated to schema version 24 (propagation_mode + auto_favor_static)");
            Ok(())
        })?;
    }

    if from_version < 25 {
        migration_step(conn, 25, |conn| {
            // Persist the chosen LXMF delivery method per outbound message so the
            // UI can render proof-aware state icons (muted check for opportunistic,
            // accent check for direct, envelope for propagated).
            let cols = get_column_names(conn, "messages").unwrap_or_default();
            if !cols.iter().any(|c| c == "delivery_method") {
                conn.execute_batch("ALTER TABLE messages ADD COLUMN delivery_method TEXT;")?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 25;")?;
            tracing::info!("Migrated to schema version 25 (messages.delivery_method)");
            Ok(())
        })?;
    }

    if from_version < 26 {
        migration_step(conn, 26, |conn| {
            // LRGP application-layer retry queue removed — Direct's
            // MAX_DELIVERY_ATTEMPTS=5 is the actual transport-layer reliability,
            // and the queue's nonce-replay window (30 min) outran LRGP's per-
            // session dedup TTL (10 min), risking duplicate move application.
            conn.execute_batch("DROP TABLE IF EXISTS lrgp_pending_sends;")?;
            conn.execute_batch("UPDATE schema_version SET version = 26;")?;
            tracing::info!("Migrated to schema version 26 (drop lrgp_pending_sends)");
            Ok(())
        })?;
    }

    if from_version < 27 {
        migration_step(conn, 27, |conn| {
            // Persist the packed LRGP envelope per action so the manual "Resend
            // last move" path can re-transmit the exact same envelope without
            // re-dispatching through the LRGP router (which would reject the
            // resend as `not_your_turn` because local state already advanced).
            let cols = get_column_names(conn, "app_actions").unwrap_or_default();
            if !cols.iter().any(|c| c == "envelope_mp") {
                conn.execute_batch("ALTER TABLE app_actions ADD COLUMN envelope_mp BLOB;")?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 27;")?;
            tracing::info!("Migrated to schema version 27 (app_actions.envelope_mp)");
            Ok(())
        })?;
    }

    if from_version < 28 {
        migration_step(conn, 28, |conn| {
            conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_blackholes (
                dest_hash       TEXT NOT NULL,
                identity_id     TEXT NOT NULL DEFAULT '',
                reason_label    TEXT DEFAULT NULL,
                ttl_seconds     REAL DEFAULT NULL,
                queued_at       REAL NOT NULL,
                PRIMARY KEY (dest_hash, identity_id)
            );
            CREATE INDEX IF NOT EXISTS idx_pending_blackholes_dest ON pending_blackholes(dest_hash);
            CREATE INDEX IF NOT EXISTS idx_pending_blackholes_identity ON pending_blackholes(identity_id);
            UPDATE schema_version SET version = 28;",
        )?;
            tracing::info!("Migrated to schema version 28 (pending_blackholes)");
            Ok(())
        })?;
    }

    if from_version < 29 {
        migration_step(conn, 29, |conn| {
            if table_exists(conn, "identity_activity")? {
                let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
                if !cols.iter().any(|c| c == "identity_hash") {
                    conn.execute_batch(
                        "ALTER TABLE identity_activity
                        ADD COLUMN identity_hash TEXT NOT NULL DEFAULT '';",
                    )?;
                }
                if !cols.iter().any(|c| c == "services") {
                    conn.execute_batch(
                        "ALTER TABLE identity_activity
                        ADD COLUMN services TEXT NOT NULL DEFAULT '';",
                    )?;
                }
                conn.execute_batch(
                    "UPDATE identity_activity
                    SET services = 'lxmf.delivery'
                  WHERE services = ''
                    AND (
                        dest_hash IN (SELECT source FROM messages WHERE source != '')
                        OR dest_hash IN (SELECT destination FROM messages WHERE destination != '')
                        OR dest_hash IN (SELECT dest_hash FROM contacts)
                    );",
                )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 29;")?;
            tracing::info!("Migrated to schema version 29 (peer service aspects)");
            Ok(())
        })?;
    }

    if from_version < 30 {
        migration_step(conn, 30, |conn| {
            if table_exists(conn, "messages")? {
                let msg_cols = get_column_names(conn, "messages").unwrap_or_default();
                for (col, ddl) in [
                    ("rtt_ms", "REAL"),
                    ("hops", "INTEGER"),
                    ("path", "TEXT"),
                    ("identity_id", "TEXT DEFAULT ''"),
                    ("attachment_name", "TEXT DEFAULT ''"),
                    ("attachment_stored_name", "TEXT DEFAULT ''"),
                    ("image_name", "TEXT DEFAULT ''"),
                    ("image_stored_name", "TEXT DEFAULT ''"),
                    ("reply_to_id", "TEXT DEFAULT ''"),
                    ("reply_to_preview", "TEXT DEFAULT ''"),
                    ("game_id", "TEXT DEFAULT ''"),
                    ("game_action", "TEXT DEFAULT ''"),
                    ("game_move_san", "TEXT DEFAULT ''"),
                    ("delivery_method", "TEXT"),
                ] {
                    if !msg_cols.iter().any(|c| c == col) {
                        conn.execute_batch(&format!(
                            "ALTER TABLE messages ADD COLUMN {col} {ddl}"
                        ))?;
                    }
                }

                conn.execute_batch(
                "DROP TRIGGER IF EXISTS messages_ai;
                 DROP TRIGGER IF EXISTS messages_ad;
                 DROP TRIGGER IF EXISTS messages_au;
                 DROP TABLE IF EXISTS messages_fts;

                 ALTER TABLE messages RENAME TO messages_old;

                 CREATE TABLE messages (
                    id TEXT NOT NULL,
                    source TEXT NOT NULL,
                    destination TEXT NOT NULL,
                    content TEXT DEFAULT '',
                    title TEXT DEFAULT '',
                    timestamp REAL NOT NULL,
                    state TEXT DEFAULT 'unknown',
                    direction TEXT DEFAULT 'outbound',
                    rtt_ms REAL,
                    hops INTEGER,
                    path TEXT,
                    identity_id TEXT NOT NULL DEFAULT '',
                    attachment_name TEXT DEFAULT '',
                    attachment_stored_name TEXT DEFAULT '',
                    image_name TEXT DEFAULT '',
                    image_stored_name TEXT DEFAULT '',
                    reply_to_id TEXT DEFAULT '',
                    reply_to_preview TEXT DEFAULT '',
                    game_id TEXT DEFAULT '',
                    game_action TEXT DEFAULT '',
                    game_move_san TEXT DEFAULT '',
                    delivery_method TEXT,
                    PRIMARY KEY (id, identity_id)
                 );

                 INSERT OR IGNORE INTO messages (
                    id, source, destination, content, title, timestamp, state, direction,
                    rtt_ms, hops, path, identity_id,
                    attachment_name, attachment_stored_name, image_name, image_stored_name,
                    reply_to_id, reply_to_preview, game_id, game_action, game_move_san,
                    delivery_method
                 )
                 SELECT
                    id,
                    COALESCE(source, ''),
                    COALESCE(destination, ''),
                    COALESCE(content, ''),
                    COALESCE(title, ''),
                    COALESCE(timestamp, 0),
                    COALESCE(state, 'unknown'),
                    COALESCE(direction, 'outbound'),
                    rtt_ms,
                    hops,
                    path,
                    COALESCE(identity_id, ''),
                    COALESCE(attachment_name, ''),
                    COALESCE(attachment_stored_name, ''),
                    COALESCE(image_name, ''),
                    COALESCE(image_stored_name, ''),
                    COALESCE(reply_to_id, ''),
                    COALESCE(reply_to_preview, ''),
                    COALESCE(game_id, ''),
                    COALESCE(game_action, ''),
                    COALESCE(game_move_san, ''),
                    delivery_method
                 FROM messages_old;

                 DROP TABLE messages_old;

                 CREATE INDEX IF NOT EXISTS idx_messages_dest ON messages(destination);
                 CREATE INDEX IF NOT EXISTS idx_messages_source ON messages(source);
                 CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
                 CREATE INDEX IF NOT EXISTS idx_messages_identity ON messages(identity_id);
                 CREATE INDEX IF NOT EXISTS idx_messages_identity_ts ON messages(identity_id, timestamp DESC);
                 CREATE INDEX IF NOT EXISTS idx_messages_unread ON messages(identity_id, direction, state, source);
                 CREATE INDEX IF NOT EXISTS idx_messages_identity_state ON messages(identity_id, state);
                 CREATE INDEX IF NOT EXISTS idx_messages_source_identity ON messages(source, identity_id, timestamp ASC);
                 CREATE INDEX IF NOT EXISTS idx_messages_dest_identity ON messages(destination, identity_id, timestamp ASC);

                 CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                    content, title, id UNINDEXED, identity_id UNINDEXED,
                    content='messages', content_rowid='rowid'
                 );
                 CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
                    INSERT INTO messages_fts(rowid, content, title, id, identity_id)
                    VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
                 END;
                 CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
                    INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
                    VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
                 END;
                 CREATE TRIGGER messages_au AFTER UPDATE OF content, title ON messages BEGIN
                    INSERT INTO messages_fts(messages_fts, rowid, content, title, id, identity_id)
                    VALUES ('delete', old.rowid, old.content, old.title, old.id, old.identity_id);
                    INSERT INTO messages_fts(rowid, content, title, id, identity_id)
                    VALUES (new.rowid, new.content, new.title, new.id, new.identity_id);
                 END;
                 INSERT INTO messages_fts(messages_fts) VALUES('rebuild');",
            )?;
            }
            conn.execute_batch("UPDATE schema_version SET version = 30;")?;
            tracing::info!("Migrated to schema version 30 (messages scoped by identity)");
            Ok(())
        })?;
    }

    if from_version < 31 {
        migration_step(conn, 31, |conn| {
            if table_exists(conn, "identities")? {
                let cols = get_column_names(conn, "identities").unwrap_or_default();
                if !cols.iter().any(|c| c == "status") {
                    conn.execute_batch(
                        "ALTER TABLE identities
                        ADD COLUMN status TEXT NOT NULL DEFAULT '';",
                    )?;
                }
            }
            if table_exists(conn, "identity_activity")? {
                let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
                if !cols.iter().any(|c| c == "status") {
                    conn.execute_batch(
                        "ALTER TABLE identity_activity
                        ADD COLUMN status TEXT NOT NULL DEFAULT '';",
                    )?;
                }
            }
            conn.execute_batch("UPDATE schema_version SET version = 31;")?;
            tracing::info!("Migrated to schema version 31 (announce status metadata)");
            Ok(())
        })?;
    }

    if from_version < 32 {
        migration_step(conn, 32, |conn| {
            // Repair databases that were marked v31 before both status columns were
            // actually present. Without identities.status, identity reads fail and
            // first-run setup incorrectly treats a populated profile as empty.
            if table_exists(conn, "identities")? {
                let cols = get_column_names(conn, "identities").unwrap_or_default();
                if !cols.iter().any(|c| c == "status") {
                    conn.execute_batch(
                        "ALTER TABLE identities
                        ADD COLUMN status TEXT NOT NULL DEFAULT '';",
                    )?;
                }
            }
            if table_exists(conn, "identity_activity")? {
                let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
                if !cols.iter().any(|c| c == "status") {
                    conn.execute_batch(
                        "ALTER TABLE identity_activity
                        ADD COLUMN status TEXT NOT NULL DEFAULT '';",
                    )?;
                }
            }
            conn.execute_batch("UPDATE schema_version SET version = 32;")?;
            tracing::info!("Migrated to schema version 32 (repair identity status columns)");
            Ok(())
        })?;
    }

    if from_version < 33 {
        migration_step(conn, 33, |conn| {
            if table_exists(conn, "identity_activity")? {
                let cols = get_column_names(conn, "identity_activity").unwrap_or_default();
                if !cols.iter().any(|c| c == "lxmf_compression_support") {
                    conn.execute_batch(
                        "ALTER TABLE identity_activity
                        ADD COLUMN lxmf_compression_support TEXT NOT NULL DEFAULT '';",
                    )?;
                }
            }
            conn.execute_batch("UPDATE schema_version SET version = 33;")?;
            tracing::info!("Migrated to schema version 33 (LXMF peer compression capability)");
            Ok(())
        })?;
    }

    if from_version < 34 {
        migration_step(conn, 34, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS channel_hubs (
                    identity_id       TEXT NOT NULL,
                    destination_hash  TEXT NOT NULL,
                    label             TEXT NOT NULL DEFAULT '',
                    nickname          TEXT NOT NULL DEFAULT '',
                    added_at           REAL NOT NULL,
                    last_connected    REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, destination_hash),
                    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS channel_rooms (
                    identity_id          TEXT NOT NULL,
                    hub_destination_hash TEXT NOT NULL,
                    room_name            TEXT NOT NULL,
                    added_at              REAL NOT NULL,
                    last_joined           REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
                    FOREIGN KEY (identity_id, hub_destination_hash)
                        REFERENCES channel_hubs(identity_id, destination_hash) ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS idx_channel_hubs_identity_recent
                    ON channel_hubs(identity_id, last_connected DESC);
                CREATE INDEX IF NOT EXISTS idx_channel_rooms_identity_hub
                    ON channel_rooms(identity_id, hub_destination_hash, room_name);
                UPDATE schema_version SET version = 34;",
            )?;
            tracing::info!("Migrated to schema version 34 (Channels bookmarks)");
            Ok(())
        })?;
    }

    if from_version < 35 {
        migration_step(conn, 35, |conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS channel_hub_rooms (
                    identity_id      TEXT NOT NULL,
                    room_name        TEXT NOT NULL,
                    topic            TEXT NOT NULL DEFAULT '',
                    key_salt         TEXT NOT NULL DEFAULT '',
                    key_mac          TEXT NOT NULL DEFAULT '',
                    key_pepper_id    TEXT NOT NULL DEFAULT '',
                    moderated        INTEGER NOT NULL DEFAULT 0,
                    invite_only      INTEGER NOT NULL DEFAULT 0,
                    topic_ops_only   INTEGER NOT NULL DEFAULT 0,
                    no_outside_msgs  INTEGER NOT NULL DEFAULT 0,
                    private          INTEGER NOT NULL DEFAULT 0,
                    created_at       REAL NOT NULL,
                    last_used        REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, room_name),
                    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS channel_hub_grants (
                    identity_id  TEXT NOT NULL,
                    room_name    TEXT NOT NULL,
                    kind         TEXT NOT NULL,
                    subject      TEXT NOT NULL,
                    granted_at   REAL NOT NULL,
                    expires_at   REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, room_name, kind, subject),
                    FOREIGN KEY (identity_id, room_name)
                        REFERENCES channel_hub_rooms(identity_id, room_name) ON DELETE CASCADE
                );
                CREATE TABLE IF NOT EXISTS channel_hub_klines (
                    identity_id  TEXT NOT NULL,
                    subject      TEXT NOT NULL,
                    banned_at    REAL NOT NULL,
                    PRIMARY KEY (identity_id, subject),
                    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
                );
                UPDATE schema_version SET version = 35;",
            )?;
            tracing::info!("Migrated to schema version 35 (RRC hub registry)");
            Ok(())
        })?;
    }

    if from_version < 36 {
        migration_step(conn, 36, |conn| {
            if table_exists(conn, "channel_hubs")? {
                let columns = get_column_names(conn, "channel_hubs").unwrap_or_default();
                if !columns.iter().any(|column| column == "desired_connected") {
                    conn.execute_batch(
                        "ALTER TABLE channel_hubs
                         ADD COLUMN desired_connected INTEGER NOT NULL DEFAULT 0
                         CHECK (desired_connected IN (0, 1));",
                    )?;
                }
                conn.execute_batch(
                    "CREATE UNIQUE INDEX IF NOT EXISTS idx_channel_hubs_identity_desired
                     ON channel_hubs(identity_id) WHERE desired_connected = 1;",
                )?;
            }
            if table_exists(conn, "channel_rooms")? {
                let columns = get_column_names(conn, "channel_rooms").unwrap_or_default();
                if !columns.iter().any(|column| column == "desired_joined") {
                    conn.execute_batch(
                        "ALTER TABLE channel_rooms
                         ADD COLUMN desired_joined INTEGER NOT NULL DEFAULT 0
                         CHECK (desired_joined IN (0, 1));",
                    )?;
                }
            }
            conn.execute_batch("UPDATE schema_version SET version = 36;")?;
            tracing::info!("Migrated to schema version 36 (Channels desired state)");
            Ok(())
        })?;
    }

    if from_version < 37 {
        migration_step(conn, 37, |conn| {
            if table_exists(conn, "channel_rooms")? {
                let columns = get_column_names(conn, "channel_rooms").unwrap_or_default();
                if !columns.iter().any(|column| column == "join_key_required") {
                    conn.execute_batch(
                        "ALTER TABLE channel_rooms
                         ADD COLUMN join_key_required INTEGER NOT NULL DEFAULT 0
                         CHECK (join_key_required IN (0, 1));",
                    )?;
                }
            }
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS channel_room_secrets (
                    identity_id          TEXT NOT NULL,
                    hub_destination_hash TEXT NOT NULL,
                    room_name            TEXT NOT NULL,
                    seal_scheme          TEXT NOT NULL,
                    seal_version         INTEGER NOT NULL CHECK (seal_version > 0),
                    ciphertext           BLOB NOT NULL CHECK (length(ciphertext) > 0),
                    updated_at           REAL NOT NULL,
                    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
                    FOREIGN KEY (identity_id, hub_destination_hash, room_name)
                        REFERENCES channel_rooms(
                            identity_id, hub_destination_hash, room_name
                        ) ON DELETE CASCADE
                 );
                 UPDATE schema_version SET version = 37;",
            )?;
            tracing::info!("Migrated to schema version 37 (sealed Channels join keys)");
            Ok(())
        })?;
    }

    Ok(())
}

fn get_column_names(conn: &Connection, table: &str) -> Result<Vec<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .collect();
    Ok(cols)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        params![table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
}

pub fn get_active_identity(pool: &DbPool) -> Option<serde_json::Value> {
    let conn = pool.get().ok()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {IDENTITY_SELECT_COLUMNS} FROM identities WHERE is_active = 1 LIMIT 1"
        ))
        .ok()?;
    stmt.query_row([], row_to_identity).ok()
}

pub fn get_all_identities(pool: &DbPool) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(&format!(
        "SELECT {IDENTITY_SELECT_COLUMNS} FROM identities ORDER BY created_at ASC"
    )) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map([], row_to_identity)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn get_identity(pool: &DbPool, hash_hex: &str) -> Option<serde_json::Value> {
    let conn = pool.get().ok()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {IDENTITY_SELECT_COLUMNS} FROM identities WHERE hash = ?1 LIMIT 1"
        ))
        .ok()?;
    stmt.query_row(params![hash_hex], row_to_identity).ok()
}

/// Process-wide identity-table generation. Bumped by every db-layer write
/// that can change which identity is active (or its lxmf hash) so runtime
/// caches invalidate without each caller remembering to — see
/// `ratspeak_runtime::helpers::active_identity_id`.
static IDENTITY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn identity_generation() -> u64 {
    IDENTITY_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// For identity-table writes that bypass the helpers in this module
/// (factory reset's raw table wipe).
pub fn note_identity_tables_changed() {
    IDENTITY_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
}

pub fn save_identity(
    pool: &DbPool,
    hash_hex: &str,
    lxmf_hash: &str,
    nickname: &str,
    display_name: &str,
) {
    note_identity_tables_changed();
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let now = now_ts();
    // ON CONFLICT closes the race between two "not exists" → INSERT callers.
    conn.execute(
        "INSERT INTO identities
             (hash, lxmf_hash, nickname, display_name, created_at, last_used,
              is_active, propagation_node, propagation_enabled)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0, '', 0)
         ON CONFLICT(hash) DO UPDATE SET
             lxmf_hash    = excluded.lxmf_hash,
             nickname     = excluded.nickname,
             display_name = excluded.display_name,
             last_used    = excluded.last_used",
        params![hash_hex, lxmf_hash, nickname, display_name, now],
    )
    .ok();
}

pub fn set_identity_propagation_node(
    pool: &DbPool,
    hash_hex: &str,
    propagation_node: &str,
) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("pool: {e}"))?;
    conn.execute(
        "UPDATE identities SET propagation_node = ?1 WHERE hash = ?2",
        params![propagation_node, hash_hex],
    )
    .map_err(|e| format!("propagation_node: {e}"))?;
    Ok(())
}

pub fn set_active_identity(pool: &DbPool, hash_hex: &str) -> Result<(), String> {
    note_identity_tables_changed();
    let mut conn = pool.get().map_err(|e| format!("pool: {e}"))?;
    let now = now_ts();
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    tx.execute("UPDATE identities SET is_active = 0", [])
        .map_err(|e| format!("deactivate: {e}"))?;
    let updated = tx
        .execute(
            "UPDATE identities SET is_active = 1, last_used = ?1 WHERE hash = ?2",
            params![now, hash_hex],
        )
        .map_err(|e| format!("activate: {e}"))?;
    if updated != 1 {
        return Err("identity not found".into());
    }
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    Ok(())
}

pub fn update_identity(
    pool: &DbPool,
    hash_hex: &str,
    nickname: Option<&str>,
    display_name: Option<&str>,
) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("pool: {e}"))?;
    if let Some(nn) = nickname {
        conn.execute(
            "UPDATE identities SET nickname = ?1 WHERE hash = ?2",
            params![nn, hash_hex],
        )
        .map_err(|e| format!("nickname: {e}"))?;
    }
    if let Some(dn) = display_name {
        conn.execute(
            "UPDATE identities SET display_name = ?1 WHERE hash = ?2",
            params![dn, hash_hex],
        )
        .map_err(|e| format!("display_name: {e}"))?;
    }
    Ok(())
}

pub fn update_identity_status(pool: &DbPool, hash_hex: &str, status: &str) -> Result<(), String> {
    let conn = pool.get().map_err(|e| format!("pool: {e}"))?;
    conn.execute(
        "UPDATE identities SET status = ?1 WHERE hash = ?2",
        params![status, hash_hex],
    )
    .map_err(|e| format!("status: {e}"))?;
    Ok(())
}

/// Every user-data table cleared by factory reset (`api_reset_database`).
/// Inventory-checked in tests: a new user-data table must be added here (or
/// explicitly exempted in the test) before it can ship.
pub const RESET_TABLES: &[&str] = &[
    "messages",
    "contacts",
    "identities",
    "settings",
    "connection_history",
    "reactions",
    "games",
    "app_sessions",
    "app_actions",
    "hidden_conversations",
    "blocked_contacts",
    "identity_activity",
    "pending_blackholes",
    "channel_room_secrets",
    "channel_rooms",
    "channel_hubs",
    "channel_hub_grants",
    "channel_hub_rooms",
    "channel_hub_klines",
];

/// Per-identity cascade for `delete_identity`. Static DELETEs (no format!()
/// interpolation), children before parents. Inventory-checked in tests
/// against every table carrying an `identity_id` column.
const IDENTITY_CASCADE: &[(&str, &str)] = &[
    (
        "app_actions",
        "DELETE FROM app_actions WHERE identity_id = ?1",
    ),
    (
        "app_sessions",
        "DELETE FROM app_sessions WHERE identity_id = ?1",
    ),
    ("games", "DELETE FROM games WHERE identity_id = ?1"),
    ("reactions", "DELETE FROM reactions WHERE identity_id = ?1"),
    (
        "hidden_conversations",
        "DELETE FROM hidden_conversations WHERE identity_id = ?1",
    ),
    (
        "blocked_contacts",
        "DELETE FROM blocked_contacts WHERE identity_id = ?1",
    ),
    (
        "pending_blackholes",
        "DELETE FROM pending_blackholes WHERE identity_id = ?1",
    ),
    (
        "channel_room_secrets",
        "DELETE FROM channel_room_secrets WHERE identity_id = ?1",
    ),
    (
        "channel_rooms",
        "DELETE FROM channel_rooms WHERE identity_id = ?1",
    ),
    (
        "channel_hubs",
        "DELETE FROM channel_hubs WHERE identity_id = ?1",
    ),
    (
        "channel_hub_grants",
        "DELETE FROM channel_hub_grants WHERE identity_id = ?1",
    ),
    (
        "channel_hub_rooms",
        "DELETE FROM channel_hub_rooms WHERE identity_id = ?1",
    ),
    (
        "channel_hub_klines",
        "DELETE FROM channel_hub_klines WHERE identity_id = ?1",
    ),
    ("contacts", "DELETE FROM contacts WHERE identity_id = ?1"),
    ("messages", "DELETE FROM messages WHERE identity_id = ?1"),
];

pub fn delete_identity(pool: &DbPool, hash_hex: &str, cascade: bool) -> Result<(), String> {
    note_identity_tables_changed();
    let mut conn = pool.get().map_err(|e| format!("pool: {e}"))?;
    let tx = conn.transaction().map_err(|e| format!("begin: {e}"))?;
    if cascade {
        for (label, sql) in IDENTITY_CASCADE {
            tx.execute(sql, params![hash_hex])
                .map_err(|e| format!("delete {label}: {e}"))?;
        }
    }
    tx.execute("DELETE FROM identities WHERE hash = ?1", params![hash_hex])
        .map_err(|e| format!("delete identity: {e}"))?;
    tx.commit().map_err(|e| format!("commit: {e}"))?;
    Ok(())
}

pub fn save_contact(
    pool: &DbPool,
    dest_hash: &str,
    display_name: Option<&str>,
    trust: &str,
    identity_id: &str,
) {
    save_contact_with_identity_pubkey(pool, dest_hash, display_name, None, trust, identity_id);
}

pub fn save_contact_with_identity_pubkey(
    pool: &DbPool,
    dest_hash: &str,
    display_name: Option<&str>,
    identity_pubkey: Option<&str>,
    trust: &str,
    identity_id: &str,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let now = now_ts();
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            params![dest_hash, identity_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if exists {
        if let Some(dn) = display_name {
            conn.execute(
                "UPDATE contacts
                 SET display_name = ?1,
                     identity_pubkey = COALESCE(?2, identity_pubkey),
                     trust = ?3,
                     last_seen = ?4
                 WHERE dest_hash = ?5 AND identity_id = ?6",
                params![dn, identity_pubkey, trust, now, dest_hash, identity_id],
            )
            .ok();
        } else {
            conn.execute(
                "UPDATE contacts
                 SET identity_pubkey = COALESCE(?1, identity_pubkey),
                     trust = ?2,
                     last_seen = ?3
                 WHERE dest_hash = ?4 AND identity_id = ?5",
                params![identity_pubkey, trust, now, dest_hash, identity_id],
            )
            .ok();
        }
    } else {
        let dn = display_name.unwrap_or("");
        conn.execute(
            "INSERT INTO contacts (dest_hash, identity_id, display_name, identity_pubkey, first_seen, last_seen, trust, notes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '')",
            params![dest_hash, identity_id, dn, identity_pubkey, now, now, trust],
        ).ok();
    }
}

pub fn delete_contact(pool: &DbPool, dest_hash: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "DELETE FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2",
        params![dest_hash, identity_id],
    )
    .ok();
}

pub fn get_all_contacts(pool: &DbPool, identity_id: &str) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT
            c.dest_hash,
            c.identity_id,
            c.display_name,
            c.identity_pubkey,
            c.first_seen,
            c.last_seen,
            c.trust,
            c.notes,
            COALESCE(ia.services, '') AS services
         FROM contacts c
         LEFT JOIN identity_activity ia ON ia.dest_hash = c.dest_hash
         WHERE c.identity_id = ?1
         ORDER BY c.display_name",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    stmt.query_map(params![identity_id], row_to_contact)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn get_all_contacts_conn(conn: &Connection, identity_id: &str) -> Vec<serde_json::Value> {
    let mut stmt = match conn.prepare(
        "SELECT
            c.dest_hash,
            c.identity_id,
            c.display_name,
            c.identity_pubkey,
            c.first_seen,
            c.last_seen,
            c.trust,
            c.notes,
            COALESCE(ia.services, '') AS services
         FROM contacts c
         LEFT JOIN identity_activity ia ON ia.dest_hash = c.dest_hash
         WHERE c.identity_id = ?1
         ORDER BY c.display_name",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    stmt.query_map(params![identity_id], row_to_contact)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Updates display_name only when empty; preserves user-chosen names.
pub fn update_contact_name_from_announce(
    pool: &DbPool,
    dest_hash: &str,
    name: &str,
    identity_id: &str,
) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let rows = conn
        .execute(
            "UPDATE contacts SET display_name = ?1, last_seen = ?4
         WHERE dest_hash = ?2 AND identity_id = ?3
         AND (display_name IS NULL OR display_name = '')",
            params![
                name,
                dest_hash,
                identity_id,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
            ],
        )
        .unwrap_or(0);
    rows > 0
}

pub fn get_contact(pool: &DbPool, dest_hash: &str, identity_id: &str) -> Option<serde_json::Value> {
    let conn = pool.get().ok()?;
    let mut stmt = conn
        .prepare("SELECT * FROM contacts WHERE dest_hash = ?1 AND identity_id = ?2")
        .ok()?;
    stmt.query_row(params![dest_hash, identity_id], row_to_contact)
        .ok()
}

pub fn block_contact(pool: &DbPool, dest_hash: &str, display_name: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!(
                reason = "pool_unavailable",
                "block_contact: pool.get() failed"
            );
            return;
        }
    };
    if conn.execute(
        "INSERT OR REPLACE INTO blocked_contacts (dest_hash, identity_id, display_name, blocked_at) VALUES (?1, ?2, ?3, ?4)",
        params![dest_hash, identity_id, display_name, now_ts()],
    ).is_err() {
        tracing::warn!(reason = "insert_failed", "block_contact: INSERT failed");
    }
}

pub fn unblock_contact(pool: &DbPool, dest_hash: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!(
                reason = "pool_unavailable",
                "unblock_contact: pool.get() failed"
            );
            return;
        }
    };
    if conn
        .execute(
            "DELETE FROM blocked_contacts WHERE dest_hash = ?1 AND identity_id = ?2",
            params![dest_hash, identity_id],
        )
        .is_err()
    {
        tracing::warn!(reason = "delete_failed", "unblock_contact: DELETE failed");
    }
}

pub fn get_blocked_contacts(pool: &DbPool, identity_id: &str) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT dest_hash, display_name, blocked_at FROM blocked_contacts WHERE identity_id = ?1 ORDER BY blocked_at DESC"
    ) { Ok(s) => s, Err(_) => return vec![] };

    stmt.query_map(params![identity_id], |row| {
        Ok(serde_json::json!({
            "hash": row.get::<_, String>(0)?,
            "display_name": row.get::<_, String>(1).unwrap_or_default(),
            "blocked_at": row.get::<_, f64>(2).unwrap_or(0.0),
        }))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn is_blocked(pool: &DbPool, dest_hash: &str, identity_id: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT COUNT(*) FROM blocked_contacts WHERE dest_hash = ?1 AND identity_id = ?2",
        params![dest_hash, identity_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

pub fn get_blocked_set(pool: &DbPool, identity_id: &str) -> std::collections::HashSet<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Default::default(),
    };
    let mut stmt =
        match conn.prepare("SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?1") {
            Ok(s) => s,
            Err(_) => return Default::default(),
        };

    stmt.query_map(params![identity_id], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn identity_hash_for_dest(pool: &DbPool, dest_hash: &str) -> Option<String> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT identity_hash FROM identity_activity WHERE dest_hash = ?1",
        params![dest_hash],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

pub fn identity_hashes_for_dests(
    pool: &DbPool,
    dest_hashes: &[String],
) -> std::collections::HashMap<String, String> {
    if dest_hashes.is_empty() {
        return Default::default();
    }

    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Default::default(),
    };
    let mut stmt =
        match conn.prepare("SELECT identity_hash FROM identity_activity WHERE dest_hash = ?1") {
            Ok(s) => s,
            Err(_) => return Default::default(),
        };

    let mut out = std::collections::HashMap::new();
    for dest_hash in dest_hashes {
        let identity_hash = stmt
            .query_row(params![dest_hash], |row| row.get::<_, String>(0))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(identity_hash) = identity_hash {
            out.insert(dest_hash.clone(), identity_hash);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct PendingBlackholeRow {
    pub dest_hash: String,
    pub identity_id: String,
    pub reason_label: Option<String>,
    pub ttl_seconds: Option<f64>,
    pub queued_at: f64,
}

/// Insert a pending blackhole row. Returns true on success.
/// Uses INSERT OR REPLACE so re-blocking the same dest+identity refreshes the
/// queued_at timestamp without duplicating the row.
pub fn enqueue_pending_blackhole(
    pool: &DbPool,
    dest_hash: &str,
    identity_id: &str,
    reason_label: Option<&str>,
    ttl_seconds: Option<f64>,
) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!(
                reason = "pool_unavailable",
                "enqueue_pending_blackhole: pool.get() failed"
            );
            return false;
        }
    };
    match conn.execute(
        "INSERT OR REPLACE INTO pending_blackholes
            (dest_hash, identity_id, reason_label, ttl_seconds, queued_at)
            VALUES (?1, ?2, ?3, ?4, ?5)",
        params![dest_hash, identity_id, reason_label, ttl_seconds, now_ts()],
    ) {
        Ok(_) => true,
        Err(_) => {
            tracing::warn!(
                reason = "insert_failed",
                "enqueue_pending_blackhole: INSERT failed"
            );
            false
        }
    }
}

/// All pending rows for a given dest_hash across local identities. Used by
/// the announce-handler sweep, which sees the dest hash on the wire and may
/// have queued escalations under multiple receivers.
pub fn list_pending_blackholes_by_dest(pool: &DbPool, dest_hash: &str) -> Vec<PendingBlackholeRow> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT dest_hash, identity_id, reason_label, ttl_seconds, queued_at
            FROM pending_blackholes WHERE dest_hash = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(params![dest_hash], |row| {
        Ok(PendingBlackholeRow {
            dest_hash: row.get::<_, String>(0)?,
            identity_id: row.get::<_, String>(1)?,
            reason_label: row.get::<_, Option<String>>(2)?,
            ttl_seconds: row.get::<_, Option<f64>>(3)?,
            queued_at: row.get::<_, f64>(4)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// All pending rows for a given local identity. Used by `api_blocked_contacts`
/// to decorate the blocked list with `is_blackhole_pending`.
pub fn list_pending_blackholes_for_identity(
    pool: &DbPool,
    identity_id: &str,
) -> Vec<PendingBlackholeRow> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT dest_hash, identity_id, reason_label, ttl_seconds, queued_at
            FROM pending_blackholes WHERE identity_id = ?1 ORDER BY queued_at DESC",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(params![identity_id], |row| {
        Ok(PendingBlackholeRow {
            dest_hash: row.get::<_, String>(0)?,
            identity_id: row.get::<_, String>(1)?,
            reason_label: row.get::<_, Option<String>>(2)?,
            ttl_seconds: row.get::<_, Option<f64>>(3)?,
            queued_at: row.get::<_, f64>(4)?,
        })
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Delete a pending row. Returns true if a row was removed.
pub fn clear_pending_blackhole(pool: &DbPool, dest_hash: &str, identity_id: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.execute(
        "DELETE FROM pending_blackholes WHERE dest_hash = ?1 AND identity_id = ?2",
        params![dest_hash, identity_id],
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

pub fn get_message_delivery_method(
    pool: &DbPool,
    msg_id: &str,
    identity_id: &str,
) -> Option<String> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT delivery_method FROM messages \
         WHERE id = ?1 AND identity_id = ?2 AND direction = 'outbound' LIMIT 1",
        params![msg_id, identity_id],
        |row| row.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

pub fn update_message_delivery_method(
    pool: &DbPool,
    msg_id: &str,
    identity_id: &str,
    delivery_method: &str,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "UPDATE messages SET delivery_method = ?1 \
         WHERE id = ?2 AND identity_id = ?3 AND direction = 'outbound'",
        params![delivery_method, msg_id, identity_id],
    )
    .ok();
}

pub fn message_exists(pool: &DbPool, msg_id: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE id = ?1",
        params![msg_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

pub fn message_exists_for_identity(pool: &DbPool, msg_id: &str, identity_id: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE id = ?1 AND identity_id = ?2",
        params![msg_id, identity_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

// Mirrors the `messages` table insert/update columns. Keeping the call explicit
// makes schema writes easy to trace at each persistence site.
#[allow(clippy::too_many_arguments)]
pub fn save_message(
    pool: &DbPool,
    msg_id: &str,
    source: &str,
    destination: &str,
    content: &str,
    title: &str,
    timestamp: f64,
    state: &str,
    direction: &str,
    identity_id: &str,
    attachment_name: &str,
    attachment_stored_name: &str,
    image_name: &str,
    image_stored_name: &str,
    reply_to_id: &str,
    reply_to_preview: &str,
    delivery_method: Option<&str>,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE id = ?1 AND identity_id = ?2",
            params![msg_id, identity_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if exists {
        conn.execute(
            "UPDATE messages SET state = ?1 WHERE id = ?2 AND identity_id = ?3",
            params![state, msg_id, identity_id],
        )
        .ok();
    } else {
        conn.execute(
            "INSERT INTO messages (id, source, destination, content, title, timestamp, state, direction, identity_id, attachment_name, attachment_stored_name, image_name, image_stored_name, reply_to_id, reply_to_preview, delivery_method) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![msg_id, source, destination, content, title, timestamp, state, direction, identity_id, attachment_name, attachment_stored_name, image_name, image_stored_name, reply_to_id, reply_to_preview, delivery_method],
        ).ok();
    }
}

/// One-way lattice: terminal states (delivered/propagated/failed/cancelled/rejected)
/// cannot be regressed by later updates. `propagated` is terminal at the LXMF
/// layer because the propagation path only confirms node-deposit, not end-to-end
/// recipient delivery — there is no later signal that upgrades it to `delivered`.
pub fn update_message_state(
    pool: &DbPool,
    msg_id: &str,
    identity_id: &str,
    state: &str,
    rtt_ms: Option<f64>,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    if let Some(rtt) = rtt_ms {
        conn.execute(
            "UPDATE messages SET state = ?1, rtt_ms = ?2 \
             WHERE id = ?3 AND identity_id = ?4 AND direction = 'outbound' AND state NOT IN ('delivered', 'propagated', 'failed', 'cancelled', 'rejected')",
            params![state, rtt, msg_id, identity_id],
        )
        .ok();
    } else {
        conn.execute(
            "UPDATE messages SET state = ?1 \
             WHERE id = ?2 AND identity_id = ?3 AND direction = 'outbound' AND state NOT IN ('delivered', 'propagated', 'failed', 'cancelled', 'rejected')",
            params![state, msg_id, identity_id],
        )
        .ok();
    }
}

pub fn cancel_outbound_message_state(pool: &DbPool, msg_id: &str, identity_id: &str) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.execute(
        "UPDATE messages SET state = 'cancelled' \
         WHERE id = ?1 AND identity_id = ?2 AND direction = 'outbound' AND state NOT IN ('delivered', 'propagated', 'failed', 'cancelled', 'rejected')",
        params![msg_id, identity_id],
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

pub fn get_conversation(
    pool: &DbPool,
    dest_hash: &str,
    identity_id: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    // UNION ALL preserves index use (OR would defeat it). Pull the newest
    // rows first, then restore chronological order for rendering.
    let mut stmt = match conn.prepare(
        "SELECT * FROM (
            SELECT * FROM (
                SELECT *, rowid AS _rw FROM messages WHERE source = ?1 AND identity_id = ?2
                UNION ALL
                SELECT *, rowid AS _rw FROM messages WHERE destination = ?1 AND identity_id = ?2 AND source != ?1
            ) ORDER BY timestamp DESC, _rw DESC LIMIT ?3
        ) ORDER BY timestamp ASC, _rw ASC"
    ) { Ok(s) => s, Err(_) => return vec![] };

    let rows: Vec<serde_json::Value> = stmt
        .query_map(params![dest_hash, identity_id, limit], row_to_message)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();

    let msg_ids: Vec<String> = rows
        .iter()
        .filter_map(|m: &serde_json::Value| {
            m.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())
        })
        .collect();
    let reactions = get_reactions_batch(&conn, &msg_ids, identity_id);

    rows.into_iter()
        .map(|mut m: serde_json::Value| {
            let mid = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(rxns) = reactions.get(&mid) {
                if let Some(o) = m.as_object_mut() {
                    o.insert("reactions".into(), serde_json::json!(rxns));
                }
            } else if let Some(o) = m.as_object_mut() {
                o.insert("reactions".into(), serde_json::json!([]));
            }
            m
        })
        .collect()
}

/// Return a display timestamp that appends to the current local conversation.
///
/// LXMF payload timestamps are sender-authored protocol data. Chat ordering uses
/// local observation time so delayed/offline deliveries cannot insert ahead of
/// messages the user has already seen or sent.
pub fn next_conversation_observed_timestamp(
    pool: &DbPool,
    dest_hash: &str,
    identity_id: &str,
    observed_at: f64,
) -> f64 {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return observed_at,
    };
    let latest: Option<f64> = conn
        .query_row(
            "SELECT MAX(timestamp) FROM (
                SELECT timestamp FROM messages WHERE source = ?1 AND identity_id = ?2
                UNION ALL
                SELECT timestamp FROM messages WHERE destination = ?1 AND identity_id = ?2 AND source != ?1
            )",
            params![dest_hash, identity_id],
            |row| row.get::<_, Option<f64>>(0),
        )
        .ok()
        .flatten();

    match latest {
        Some(ts) if ts.is_finite() && observed_at <= ts => ts + 0.001,
        _ => observed_at,
    }
}

pub fn search_messages(
    pool: &DbPool,
    query: &str,
    identity_id: &str,
    limit: i64,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!(
                reason = "pool_unavailable",
                "search_messages: pool.get() failed"
            );
            return vec![];
        }
    };
    // Phrase-search escape; tolerates user-typed FTS5 specials.
    let safe_query = format!("\"{}\"", query.replace('"', "\"\""));

    let result = conn.prepare(
        "SELECT m.* FROM messages m JOIN messages_fts f ON m.rowid = f.rowid WHERE messages_fts MATCH ?1 AND f.identity_id = ?2 ORDER BY m.timestamp DESC LIMIT ?3"
    ).and_then(|mut stmt| {
        stmt.query_map(params![safe_query, identity_id, limit], row_to_message)
            .map(|rows| rows.filter_map(|r| r.ok()).collect::<Vec<_>>())
    });

    match result {
        Ok(rows) => rows,
        Err(_) => {
            // LIKE fallback on FTS errors.
            let pattern = format!("%{query}%");
            conn.prepare(
                "SELECT * FROM messages WHERE content LIKE ?1 AND identity_id = ?2 ORDER BY timestamp DESC LIMIT ?3"
            ).and_then(|mut stmt| {
                stmt.query_map(params![pattern, identity_id, limit], row_to_message)
                    .map(|rows| rows.filter_map(|r| r.ok()).collect())
            }).unwrap_or_default()
        }
    }
}

pub fn mark_read(pool: &DbPool, dest_hash: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "UPDATE messages SET state = 'read' WHERE source = ?1 AND direction = 'inbound' AND state != 'read' AND identity_id = ?2",
        params![dest_hash, identity_id],
    ).ok();
}

pub fn get_all_unread_counts(
    pool: &DbPool,
    identity_id: &str,
) -> std::collections::HashMap<String, i64> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Default::default(),
    };
    let mut stmt = match conn.prepare(
        "SELECT source, COUNT(*) as cnt FROM messages WHERE direction = 'inbound' AND state != 'read' AND identity_id = ?1 GROUP BY source"
    ) { Ok(s) => s, Err(_) => return Default::default() };

    stmt.query_map(params![identity_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Used by the Android foreground-service to render per-sender notifications.
pub fn get_unread_breakdown(
    pool: &DbPool,
    identity_id: &str,
) -> Vec<(String, Option<String>, i64, String, f64)> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let sql = "
        SELECT cnt.source,
               c.display_name,
               cnt.unread,
               latest.content,
               latest.ts
        FROM (
            SELECT source, COUNT(*) AS unread
            FROM messages
            WHERE direction = 'inbound' AND state != 'read' AND identity_id = ?1
            GROUP BY source
        ) cnt
        JOIN (
            SELECT source,
                   content,
                   timestamp AS ts,
                   ROW_NUMBER() OVER (PARTITION BY source ORDER BY timestamp DESC) AS rn
            FROM messages
            WHERE direction = 'inbound' AND state != 'read' AND identity_id = ?1
        ) latest ON latest.source = cnt.source AND latest.rn = 1
        LEFT JOIN contacts c ON c.dest_hash = cnt.source AND c.identity_id = ?1
        ORDER BY latest.ts DESC
    ";
    let mut stmt = match conn.prepare(sql) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                reason = "prepare_failed",
                "get_unread_breakdown: prepare failed"
            );
            return Vec::new();
        }
    };
    stmt.query_map(params![identity_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1).ok().flatten(),
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3).unwrap_or_default(),
            row.get::<_, f64>(4).unwrap_or(0.0),
        ))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn get_all_unread_counts_conn(
    conn: &Connection,
    identity_id: &str,
) -> std::collections::HashMap<String, i64> {
    let mut stmt = match conn.prepare(
        "SELECT source, COUNT(*) as cnt FROM messages WHERE direction = 'inbound' AND state != 'read' AND identity_id = ?1 GROUP BY source"
    ) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(reason = "prepare_failed", "get_all_unread_counts_conn: prepare failed");
            return Default::default();
        }
    };

    stmt.query_map(params![identity_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_else(|_| {
        tracing::warn!(
            reason = "query_failed",
            "get_all_unread_counts_conn: query_map failed"
        );
        Default::default()
    })
}

pub fn cleanup_stale_outbound(pool: &DbPool, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let result = conn.execute(
        "UPDATE messages SET state = 'failed' WHERE state IN ('sending', 'routing', 'propagating', 'sent') AND direction = 'outbound' AND identity_id = ?1",
        params![identity_id],
    );
    if let Ok(count) = result
        && count > 0
    {
        tracing::info!("Cleaned up {count} stale outbound message(s)");
    }
}

pub fn hide_conversation(pool: &DbPool, dest_hash: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "INSERT OR REPLACE INTO hidden_conversations (dest_hash, identity_id, hidden_at) VALUES (?1, ?2, ?3)",
        params![dest_hash, identity_id, now_ts()],
    ).ok();
}

pub fn unhide_conversation(pool: &DbPool, dest_hash: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "DELETE FROM hidden_conversations WHERE dest_hash = ?1 AND identity_id = ?2",
        params![dest_hash, identity_id],
    )
    .ok();
}

pub fn get_hidden_conversations(
    pool: &DbPool,
    identity_id: &str,
) -> std::collections::HashSet<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Default::default(),
    };
    let mut stmt =
        match conn.prepare("SELECT dest_hash FROM hidden_conversations WHERE identity_id = ?1") {
            Ok(s) => s,
            Err(_) => return Default::default(),
        };

    stmt.query_map(params![identity_id], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn delete_conversation(pool: &DbPool, dest_hash: &str, identity_id: &str) -> Vec<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut file_refs = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT attachment_stored_name, image_stored_name FROM messages WHERE (source = ?1 OR destination = ?1) AND identity_id = ?2"
    )
        && let Ok(rows) = stmt.query_map(params![dest_hash, identity_id], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
            ))
        })
    {
        for r in rows.flatten() {
            if !r.0.is_empty() { file_refs.push(r.0); }
            if !r.1.is_empty() { file_refs.push(r.1); }
        }
    }

    conn.execute(
        "DELETE FROM messages WHERE (source = ?1 OR destination = ?1) AND identity_id = ?2",
        params![dest_hash, identity_id],
    )
    .ok();
    conn.execute(
        "DELETE FROM hidden_conversations WHERE dest_hash = ?1 AND identity_id = ?2",
        params![dest_hash, identity_id],
    )
    .ok();

    file_refs
}

pub fn get_setting(pool: &DbPool, key: &str) -> Option<String> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Read a related set of settings from one SQLite snapshot. Missing keys are
/// omitted; database failures are surfaced instead of being confused with an
/// unset preference.
pub fn get_settings(
    pool: &DbPool,
    keys: &[&str],
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let mut values = std::collections::HashMap::with_capacity(keys.len());
    for key in keys {
        let value = transaction
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(value) = value {
            values.insert((*key).to_string(), value);
        }
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(values)
}

pub fn set_setting(pool: &DbPool, key: &str, value: &str) {
    let _ = try_set_setting(pool, key, value);
}

pub fn try_set_setting(pool: &DbPool, key: &str, value: &str) -> Result<(), String> {
    let conn = pool.get().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Persist a coherent group of settings in one transaction. This is the
/// boundary for controls whose fields are edited and applied as one unit.
pub fn try_set_settings(pool: &DbPool, values: &[(String, String)]) -> Result<(), String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    for (key, value) in values {
        transaction
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(|error| error.to_string())?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    #[test]
    fn related_settings_round_trip_as_one_snapshot() {
        let pool = test_pool();
        try_set_settings(
            &pool,
            &[
                ("hub_name".to_string(), "Mountain relay".to_string()),
                ("hub_enabled".to_string(), "1".to_string()),
            ],
        )
        .unwrap();

        let values = get_settings(&pool, &["hub_name", "hub_enabled", "missing"]).unwrap();
        assert_eq!(
            values.get("hub_name").map(String::as_str),
            Some("Mountain relay")
        );
        assert_eq!(values.get("hub_enabled").map(String::as_str), Some("1"));
        assert!(!values.contains_key("missing"));
    }

    #[test]
    fn a_failed_settings_batch_rolls_back_every_field() {
        let pool = test_pool();
        pool.get()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER reject_test_setting
                 BEFORE INSERT ON settings
                 WHEN NEW.key = 'reject'
                 BEGIN SELECT RAISE(ABORT, 'rejected'); END;",
            )
            .unwrap();

        let result = try_set_settings(
            &pool,
            &[
                ("first".to_string(), "saved-too-early".to_string()),
                ("reject".to_string(), "no".to_string()),
            ],
        );
        assert!(result.is_err());
        assert_eq!(get_setting(&pool, "first"), None);
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SavedChannelHub {
    pub destination_hash: String,
    pub label: String,
    pub nickname: String,
    pub added_at: f64,
    pub last_connected: f64,
    /// Durable scheduler intent, distinct from an observed live Link.
    pub desired_connected: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SavedChannelRoom {
    pub hub_destination_hash: String,
    pub room_name: String,
    pub added_at: f64,
    pub last_joined: f64,
    /// Durable scheduler intent, distinct from hub-confirmed membership.
    pub desired_joined: bool,
    /// Non-secret recovery hint. A desired protected room without ciphertext
    /// must wait for user input instead of retrying a keyless JOIN forever.
    pub join_key_required: bool,
}

#[derive(Clone, PartialEq)]
pub struct StoredChannelRoomSecret {
    pub hub_destination_hash: String,
    pub room_name: String,
    pub seal_scheme: String,
    pub seal_version: u32,
    pub ciphertext: Vec<u8>,
    pub updated_at: f64,
}

impl std::fmt::Debug for StoredChannelRoomSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredChannelRoomSecret")
            .field("hub_destination_hash", &self.hub_destination_hash)
            .field("room_name", &self.room_name)
            .field("seal_scheme", &self.seal_scheme)
            .field("seal_version", &self.seal_version)
            .field("ciphertext", &"<redacted>")
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

pub fn list_saved_channel_hubs(
    pool: &DbPool,
    identity_id: &str,
) -> Result<Vec<SavedChannelHub>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT destination_hash, label, nickname, added_at, last_connected,
                    desired_connected
             FROM channel_hubs
             WHERE identity_id = ?1
             ORDER BY last_connected DESC, label COLLATE NOCASE, destination_hash",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![identity_id], |row| {
            Ok(SavedChannelHub {
                destination_hash: row.get(0)?,
                label: row.get(1)?,
                nickname: row.get(2)?,
                added_at: row.get(3)?,
                last_connected: row.get(4)?,
                desired_connected: row.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub fn save_channel_hub(
    pool: &DbPool,
    identity_id: &str,
    destination_hash: &str,
    label: &str,
    nickname: &str,
    connected: bool,
) -> Result<(), String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let now = now_ts();
    conn.execute(
        "INSERT INTO channel_hubs
            (identity_id, destination_hash, label, nickname, added_at, last_connected)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(identity_id, destination_hash) DO UPDATE SET
            label = excluded.label,
            nickname = excluded.nickname,
            last_connected = CASE
                WHEN excluded.last_connected > 0 THEN excluded.last_connected
                ELSE channel_hubs.last_connected
            END",
        params![
            identity_id,
            destination_hash,
            label,
            nickname,
            now,
            if connected { now } else { 0.0 }
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Persist the one-hub scheduler target without conflating it with an
/// observed connection. Selecting a hub clears the previous winner in the
/// same transaction; the partial unique index is the final concurrency guard.
pub fn set_channel_hub_desired(
    pool: &DbPool,
    identity_id: &str,
    destination_hash: &str,
    nickname: &str,
    desired: bool,
) -> Result<(), String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    if desired {
        tx.execute(
            "UPDATE channel_hubs SET desired_connected = 0
             WHERE identity_id = ?1 AND desired_connected != 0",
            params![identity_id],
        )
        .map_err(|error| error.to_string())?;
    }
    let now = now_ts();
    tx.execute(
        "INSERT INTO channel_hubs
            (identity_id, destination_hash, label, nickname, added_at,
             last_connected, desired_connected)
         VALUES (?1, ?2, '', ?3, ?4, 0, ?5)
         ON CONFLICT(identity_id, destination_hash) DO UPDATE SET
            nickname = excluded.nickname,
            desired_connected = excluded.desired_connected",
        params![identity_id, destination_hash, nickname, now, desired as i64],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())
}

/// Rename an identity and retire the superseded name from its saved hub
/// bookmarks in one transaction.
///
/// Bookmarks record whatever nickname the session connected as, so a rename
/// would otherwise keep offering — and broadcasting — the previous name. Only
/// bookmarks still holding the exact previous name are rewritten; a deliberate
/// per-hub alias differs from it and is left alone.
///
/// The two writes must commit together: if the sweep were to fail after the
/// rename committed, a retry would read the already-updated name as the
/// "previous" one, skip the sweep on the equality guard, and strand the old
/// name in the bookmark permanently.
pub struct IdentityRenameOutcome {
    /// The name this identity carried before the rename ("" if unset).
    pub previous_name: String,
    pub retired_bookmarks: usize,
}

pub fn rename_identity_and_retire_alias(
    pool: &DbPool,
    identity_id: &str,
    new_name: &str,
) -> Result<IdentityRenameOutcome, String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let previous_name: String = transaction
        .query_row(
            "SELECT COALESCE(display_name, '') FROM identities WHERE hash = ?1",
            params![identity_id],
            |row| row.get(0),
        )
        .unwrap_or_default();
    transaction
        .execute(
            "UPDATE identities SET display_name = ?1 WHERE hash = ?2",
            params![new_name, identity_id],
        )
        .map_err(|error| format!("display_name: {error}"))?;
    let retired = if previous_name.is_empty() || previous_name == new_name {
        0
    } else {
        transaction
            .execute(
                "UPDATE channel_hubs SET nickname = ?1 WHERE identity_id = ?2 AND nickname = ?3",
                params![new_name, identity_id, previous_name],
            )
            .map_err(|error| format!("hub_nickname: {error}"))?
    };
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(IdentityRenameOutcome {
        previous_name,
        retired_bookmarks: retired,
    })
}

/// One hosted room's durable policy. Grants ride along so a restore is a
/// single query pair rather than one query per room.
#[derive(Clone, PartialEq)]
pub struct HubRoomRow {
    pub room_name: String,
    pub topic: String,
    pub key_salt: String,
    pub key_mac: String,
    pub key_pepper_id: String,
    pub moderated: bool,
    pub invite_only: bool,
    pub topic_ops_only: bool,
    pub no_outside_msgs: bool,
    pub private: bool,
    pub last_used: f64,
    /// `(kind, subject hex, expires_at)`; kind is `op|voice|ban|invite`.
    pub grants: Vec<(String, String, f64)>,
}

/// Hand-written so a room key digest can never reach a log or a panic message.
impl std::fmt::Debug for HubRoomRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubRoomRow")
            .field("room_name", &self.room_name)
            .field("keyed", &!self.key_mac.is_empty())
            .field("grants", &self.grants.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum HubRoomOp {
    Upsert(Box<HubRoomRow>),
    Touched { room_name: String, last_used: f64 },
    Removed { room_name: String },
    ReplaceKlines(Vec<String>),
    GcInvites { before: f64 },
}

pub fn list_hub_rooms(pool: &DbPool, identity_id: &str) -> Result<Vec<HubRoomRow>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let mut rooms: Vec<HubRoomRow> = conn
        .prepare(
            "SELECT room_name, topic, key_salt, key_mac, key_pepper_id, moderated,
                    invite_only, topic_ops_only, no_outside_msgs, private, last_used
             FROM channel_hub_rooms WHERE identity_id = ?1 ORDER BY room_name",
        )
        .and_then(|mut stmt| {
            stmt.query_map(params![identity_id], |row| {
                Ok(HubRoomRow {
                    room_name: row.get(0)?,
                    topic: row.get(1)?,
                    key_salt: row.get(2)?,
                    key_mac: row.get(3)?,
                    key_pepper_id: row.get(4)?,
                    moderated: row.get::<_, i64>(5)? != 0,
                    invite_only: row.get::<_, i64>(6)? != 0,
                    topic_ops_only: row.get::<_, i64>(7)? != 0,
                    no_outside_msgs: row.get::<_, i64>(8)? != 0,
                    private: row.get::<_, i64>(9)? != 0,
                    last_used: row.get(10)?,
                    grants: Vec::new(),
                })
            })
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|error| error.to_string())?;

    let mut grants: std::collections::HashMap<String, Vec<(String, String, f64)>> =
        std::collections::HashMap::new();
    conn.prepare(
        "SELECT room_name, kind, subject, expires_at
         FROM channel_hub_grants WHERE identity_id = ?1",
    )
    .and_then(|mut stmt| {
        stmt.query_map(params![identity_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, f64>(3)?,
            ))
        })
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
    })
    .map_err(|error| error.to_string())?
    .into_iter()
    .for_each(|(room, kind, subject, expires)| {
        grants
            .entry(room)
            .or_default()
            .push((kind, subject, expires));
    });

    for room in &mut rooms {
        if let Some(found) = grants.remove(&room.room_name) {
            room.grants = found;
        }
    }
    Ok(rooms)
}

pub fn list_hub_klines(pool: &DbPool, identity_id: &str) -> Result<Vec<String>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    conn.prepare("SELECT subject FROM channel_hub_klines WHERE identity_id = ?1")
        .and_then(|mut stmt| {
            stmt.query_map(params![identity_id], |row| row.get::<_, String>(0))
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        })
        .map_err(|error| error.to_string())
}

/// Apply a batch of registry writes in one transaction, in order. Ordering is
/// load-bearing: two writes to the same room must not reorder, so the caller
/// hands the whole batch over rather than spawning a task per op.
pub fn apply_hub_ops(pool: &DbPool, identity_id: &str, ops: &[HubRoomOp]) -> Result<(), String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let tx = conn.transaction().map_err(|error| error.to_string())?;
    let now = now_ts();
    for op in ops {
        match op {
            HubRoomOp::Upsert(room) => {
                tx.execute(
                    "INSERT INTO channel_hub_rooms
                        (identity_id, room_name, topic, key_salt, key_mac, key_pepper_id,
                         moderated, invite_only, topic_ops_only, no_outside_msgs, private,
                         created_at, last_used)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                     ON CONFLICT(identity_id, room_name) DO UPDATE SET
                        topic = excluded.topic,
                        key_salt = excluded.key_salt,
                        key_mac = excluded.key_mac,
                        key_pepper_id = excluded.key_pepper_id,
                        moderated = excluded.moderated,
                        invite_only = excluded.invite_only,
                        topic_ops_only = excluded.topic_ops_only,
                        no_outside_msgs = excluded.no_outside_msgs,
                        private = excluded.private,
                        last_used = excluded.last_used",
                    params![
                        identity_id,
                        room.room_name,
                        room.topic,
                        room.key_salt,
                        room.key_mac,
                        room.key_pepper_id,
                        room.moderated as i64,
                        room.invite_only as i64,
                        room.topic_ops_only as i64,
                        room.no_outside_msgs as i64,
                        room.private as i64,
                        now,
                        room.last_used
                    ],
                )
                .map_err(|error| error.to_string())?;
                // Grants are authoritative per room: replace wholesale so a
                // revoked op or expired invite cannot survive as a stale row.
                tx.execute(
                    "DELETE FROM channel_hub_grants WHERE identity_id = ?1 AND room_name = ?2",
                    params![identity_id, room.room_name],
                )
                .map_err(|error| error.to_string())?;
                for (kind, subject, expires_at) in &room.grants {
                    tx.execute(
                        "INSERT OR REPLACE INTO channel_hub_grants
                            (identity_id, room_name, kind, subject, granted_at, expires_at)
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![identity_id, room.room_name, kind, subject, now, expires_at],
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            HubRoomOp::Touched {
                room_name,
                last_used,
            } => {
                tx.execute(
                    "UPDATE channel_hub_rooms SET last_used = ?1
                     WHERE identity_id = ?2 AND room_name = ?3",
                    params![last_used, identity_id, room_name],
                )
                .map_err(|error| error.to_string())?;
            }
            HubRoomOp::Removed { room_name } => {
                tx.execute(
                    "DELETE FROM channel_hub_rooms WHERE identity_id = ?1 AND room_name = ?2",
                    params![identity_id, room_name],
                )
                .map_err(|error| error.to_string())?;
            }
            HubRoomOp::ReplaceKlines(subjects) => {
                tx.execute(
                    "DELETE FROM channel_hub_klines WHERE identity_id = ?1",
                    params![identity_id],
                )
                .map_err(|error| error.to_string())?;
                for subject in subjects {
                    tx.execute(
                        "INSERT OR REPLACE INTO channel_hub_klines
                            (identity_id, subject, banned_at) VALUES (?1,?2,?3)",
                        params![identity_id, subject, now],
                    )
                    .map_err(|error| error.to_string())?;
                }
            }
            HubRoomOp::GcInvites { before } => {
                tx.execute(
                    "DELETE FROM channel_hub_grants
                     WHERE identity_id = ?1 AND kind = 'invite' AND expires_at <= ?2",
                    params![identity_id, before],
                )
                .map_err(|error| error.to_string())?;
            }
        }
    }
    tx.commit().map_err(|error| error.to_string())
}

pub fn remove_channel_hub(
    pool: &DbPool,
    identity_id: &str,
    destination_hash: &str,
) -> Result<bool, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    conn.execute(
        "DELETE FROM channel_hubs WHERE identity_id = ?1 AND destination_hash = ?2",
        params![identity_id, destination_hash],
    )
    .map(|changed| changed > 0)
    .map_err(|error| error.to_string())
}

pub fn list_saved_channel_rooms(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
) -> Result<Vec<SavedChannelRoom>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT hub_destination_hash, room_name, added_at, last_joined,
                    desired_joined, join_key_required
             FROM channel_rooms
             WHERE identity_id = ?1 AND hub_destination_hash = ?2
             ORDER BY last_joined DESC, room_name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![identity_id, hub_destination_hash], |row| {
            Ok(SavedChannelRoom {
                hub_destination_hash: row.get(0)?,
                room_name: row.get(1)?,
                added_at: row.get(2)?,
                last_joined: row.get(3)?,
                desired_joined: row.get::<_, i64>(4)? != 0,
                join_key_required: row.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

/// Load all remembered rooms for one identity in one query. The service-state
/// snapshot is hub-keyed, so doing one query per saved hub would make startup
/// cost grow quadratically with a user's community list.
pub fn list_saved_channel_rooms_for_identity(
    pool: &DbPool,
    identity_id: &str,
) -> Result<Vec<SavedChannelRoom>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT hub_destination_hash, room_name, added_at, last_joined,
                    desired_joined, join_key_required
             FROM channel_rooms
             WHERE identity_id = ?1
             ORDER BY hub_destination_hash, last_joined DESC,
                      room_name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![identity_id], |row| {
            Ok(SavedChannelRoom {
                hub_destination_hash: row.get(0)?,
                room_name: row.get(1)?,
                added_at: row.get(2)?,
                last_joined: row.get(3)?,
                desired_joined: row.get::<_, i64>(4)? != 0,
                join_key_required: row.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

pub fn save_channel_room(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
    joined: bool,
) -> Result<(), String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let now = now_ts();
    conn.execute(
        "INSERT INTO channel_rooms
            (identity_id, hub_destination_hash, room_name, added_at, last_joined)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(identity_id, hub_destination_hash, room_name) DO UPDATE SET
            last_joined = CASE
                WHEN excluded.last_joined > 0 THEN excluded.last_joined
                ELSE channel_rooms.last_joined
            END",
        params![
            identity_id,
            hub_destination_hash,
            room_name,
            now,
            if joined { now } else { 0.0 }
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Persist desired room membership independently from the last JOIN observed.
/// A failed or disconnected session can therefore retain honest user intent
/// without claiming that the hub currently considers the identity a member.
pub fn set_channel_room_desired(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
    desired: bool,
) -> Result<(), String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let now = now_ts();
    conn.execute(
        "INSERT INTO channel_rooms
            (identity_id, hub_destination_hash, room_name, added_at,
             last_joined, desired_joined)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(identity_id, hub_destination_hash, room_name) DO UPDATE SET
            desired_joined = excluded.desired_joined",
        params![
            identity_id,
            hub_destination_hash,
            room_name,
            now,
            desired as i64
        ],
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

pub fn list_channel_room_secrets_for_identity(
    pool: &DbPool,
    identity_id: &str,
) -> Result<Vec<StoredChannelRoomSecret>, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let mut statement = conn
        .prepare(
            "SELECT hub_destination_hash, room_name, seal_scheme, seal_version,
                    ciphertext, updated_at
             FROM channel_room_secrets
             WHERE identity_id = ?1
             ORDER BY hub_destination_hash, room_name COLLATE NOCASE",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(params![identity_id], |row| {
            Ok(StoredChannelRoomSecret {
                hub_destination_hash: row.get(0)?,
                room_name: row.get(1)?,
                seal_scheme: row.get(2)?,
                seal_version: row.get(3)?,
                ciphertext: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())
}

/// Atomically store identity-sealed ciphertext and the non-secret requirement
/// hint. Callers must do this only after authenticated JOIN confirmation.
pub fn save_channel_room_secret(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
    seal_scheme: &str,
    seal_version: u32,
    ciphertext: &[u8],
) -> Result<(), String> {
    if seal_scheme.is_empty() || seal_version == 0 || ciphertext.is_empty() {
        return Err("invalid sealed channel room secret".into());
    }
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let now = now_ts();
    let changed = transaction
        .execute(
            "UPDATE channel_rooms SET join_key_required = 1
             WHERE identity_id = ?1 AND hub_destination_hash = ?2 AND room_name = ?3",
            params![identity_id, hub_destination_hash, room_name],
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("channel room does not exist".into());
    }
    transaction
        .execute(
            "INSERT INTO channel_room_secrets
                (identity_id, hub_destination_hash, room_name, seal_scheme,
                 seal_version, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(identity_id, hub_destination_hash, room_name) DO UPDATE SET
                seal_scheme = excluded.seal_scheme,
                seal_version = excluded.seal_version,
                ciphertext = excluded.ciphertext,
                updated_at = excluded.updated_at",
            params![
                identity_id,
                hub_destination_hash,
                room_name,
                seal_scheme,
                i64::from(seal_version),
                ciphertext,
                now
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

/// Persist one-way knowledge that this room requires a join key. This never
/// modifies recoverable ciphertext: a mistyped replacement must not destroy a
/// previously confirmed key.
pub fn mark_channel_room_key_required(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
) -> Result<(), String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    let changed = conn
        .execute(
            "UPDATE channel_rooms SET join_key_required = 1
             WHERE identity_id = ?1 AND hub_destination_hash = ?2 AND room_name = ?3",
            params![identity_id, hub_destination_hash, room_name],
        )
        .map_err(|error| error.to_string())?;
    if changed == 1 {
        Ok(())
    } else {
        Err("channel room does not exist".into())
    }
}

/// Forget recoverable ciphertext while preserving whether reconnect must wait
/// for a replacement key. Rejection/corruption uses `required = true`.
pub fn remove_channel_room_secret(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
    required: bool,
) -> Result<bool, String> {
    let mut conn = pool.get().map_err(|error| error.to_string())?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let removed = transaction
        .execute(
            "DELETE FROM channel_room_secrets
             WHERE identity_id = ?1 AND hub_destination_hash = ?2 AND room_name = ?3",
            params![identity_id, hub_destination_hash, room_name],
        )
        .map_err(|error| error.to_string())?
        > 0;
    transaction
        .execute(
            "UPDATE channel_rooms SET join_key_required = ?1
             WHERE identity_id = ?2 AND hub_destination_hash = ?3 AND room_name = ?4",
            params![
                required as i64,
                identity_id,
                hub_destination_hash,
                room_name
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(removed)
}

pub fn remove_channel_room(
    pool: &DbPool,
    identity_id: &str,
    hub_destination_hash: &str,
    room_name: &str,
) -> Result<bool, String> {
    let conn = pool.get().map_err(|error| error.to_string())?;
    conn.execute(
        "DELETE FROM channel_rooms
         WHERE identity_id = ?1 AND hub_destination_hash = ?2 AND room_name = ?3",
        params![identity_id, hub_destination_hash, room_name],
    )
    .map(|changed| changed > 0)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod channel_bookmark_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let manager = SqliteConnectionManager::memory()
            .with_init(|connection| connection.execute_batch("PRAGMA foreign_keys=ON;"));
        let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    #[test]
    fn hubs_and_rooms_are_identity_scoped_and_hub_delete_cascades() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");

        save_channel_hub(
            &pool,
            "identity-a",
            "00112233445566778899aabbccddeeff",
            "Mountain relay",
            "Field Rat",
            false,
        )
        .unwrap();
        save_channel_room(
            &pool,
            "identity-a",
            "00112233445566778899aabbccddeeff",
            "field team",
            true,
        )
        .unwrap();

        let hubs = list_saved_channel_hubs(&pool, "identity-a").unwrap();
        assert_eq!(hubs.len(), 1);
        assert_eq!(hubs[0].label, "Mountain relay");
        assert!(
            list_saved_channel_hubs(&pool, "identity-b")
                .unwrap()
                .is_empty()
        );
        let rooms =
            list_saved_channel_rooms(&pool, "identity-a", "00112233445566778899aabbccddeeff")
                .unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_name, "field team");
        assert!(rooms[0].last_joined > 0.0);

        assert!(
            remove_channel_hub(&pool, "identity-a", "00112233445566778899aabbccddeeff").unwrap()
        );
        assert!(
            list_saved_channel_rooms(&pool, "identity-a", "00112233445566778899aabbccddeeff")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn desired_channel_state_is_single_hub_scoped_and_independent_of_recency() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");

        set_channel_hub_desired(&pool, "identity-a", "aa", "alpha", true).unwrap();
        set_channel_room_desired(&pool, "identity-a", "aa", "general", true).unwrap();
        set_channel_room_desired(&pool, "identity-a", "aa", "quiet", false).unwrap();

        let hubs = list_saved_channel_hubs(&pool, "identity-a").unwrap();
        assert_eq!(hubs.len(), 1);
        assert!(hubs[0].desired_connected);
        let rooms = list_saved_channel_rooms_for_identity(&pool, "identity-a").unwrap();
        assert_eq!(rooms.len(), 2);
        assert!(
            rooms
                .iter()
                .find(|room| room.room_name == "general")
                .unwrap()
                .desired_joined
        );
        assert!(
            !rooms
                .iter()
                .find(|room| room.room_name == "quiet")
                .unwrap()
                .desired_joined
        );

        // Selecting another hub atomically replaces the one scheduler winner
        // but retains the first hub and its room intent for a later switch.
        set_channel_hub_desired(&pool, "identity-a", "bb", "bravo", true).unwrap();
        let hubs = list_saved_channel_hubs(&pool, "identity-a").unwrap();
        assert_eq!(hubs.iter().filter(|hub| hub.desired_connected).count(), 1);
        assert!(
            hubs.iter()
                .find(|hub| hub.destination_hash == "bb")
                .unwrap()
                .desired_connected
        );
        assert!(
            !hubs
                .iter()
                .find(|hub| hub.destination_hash == "aa")
                .unwrap()
                .desired_connected
        );
        assert!(
            list_saved_channel_rooms(&pool, "identity-a", "aa")
                .unwrap()
                .iter()
                .any(|room| room.room_name == "general" && room.desired_joined)
        );

        // Updating recency and labels is orthogonal to scheduler intent.
        save_channel_hub(&pool, "identity-a", "bb", "Relay B", "bravo", true).unwrap();
        save_channel_room(&pool, "identity-a", "bb", "ops", true).unwrap();
        assert!(
            list_saved_channel_hubs(&pool, "identity-a")
                .unwrap()
                .iter()
                .find(|hub| hub.destination_hash == "bb")
                .unwrap()
                .desired_connected
        );
        assert!(
            !list_saved_channel_rooms(&pool, "identity-a", "bb")
                .unwrap()
                .iter()
                .find(|room| room.room_name == "ops")
                .unwrap()
                .desired_joined
        );

        set_channel_hub_desired(&pool, "identity-b", "cc", "charlie", true).unwrap();
        assert!(
            list_saved_channel_hubs(&pool, "identity-b").unwrap()[0].desired_connected,
            "the one-hub budget is identity-scoped"
        );
    }

    #[test]
    fn sealed_room_secrets_are_identity_scoped_redacted_and_forgettable() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");
        set_channel_hub_desired(&pool, "identity-a", "aa", "alpha", true).unwrap();
        set_channel_room_desired(&pool, "identity-a", "aa", "general", true).unwrap();
        set_channel_hub_desired(&pool, "identity-b", "bb", "bravo", true).unwrap();
        set_channel_room_desired(&pool, "identity-b", "bb", "general", true).unwrap();

        let ciphertext = b"opaque-ciphertext-that-debug-must-hide";
        save_channel_room_secret(
            &pool,
            "identity-a",
            "aa",
            "general",
            "rns_identity",
            1,
            ciphertext,
        )
        .unwrap();

        let secrets = list_channel_room_secrets_for_identity(&pool, "identity-a").unwrap();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].ciphertext, ciphertext);
        assert_eq!(secrets[0].seal_scheme, "rns_identity");
        assert_eq!(secrets[0].seal_version, 1);
        let debug = format!("{:?}", secrets[0]);
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("opaque-ciphertext"));
        assert!(
            list_channel_room_secrets_for_identity(&pool, "identity-b")
                .unwrap()
                .is_empty()
        );
        assert!(list_saved_channel_rooms(&pool, "identity-a", "aa").unwrap()[0].join_key_required);
        mark_channel_room_key_required(&pool, "identity-a", "aa", "general").unwrap();
        assert_eq!(
            list_channel_room_secrets_for_identity(&pool, "identity-a").unwrap()[0].ciphertext,
            ciphertext,
            "learning that a key is required must not erase confirmed ciphertext"
        );

        set_channel_room_desired(&pool, "identity-a", "aa", "invited", true).unwrap();
        mark_channel_room_key_required(&pool, "identity-a", "aa", "invited").unwrap();
        let invited = list_saved_channel_rooms(&pool, "identity-a", "aa")
            .unwrap()
            .into_iter()
            .find(|room| room.room_name == "invited")
            .unwrap();
        assert!(invited.join_key_required);
        assert_eq!(
            list_channel_room_secrets_for_identity(&pool, "identity-a")
                .unwrap()
                .len(),
            1,
            "key-required knowledge does not invent recoverable key material"
        );

        assert!(remove_channel_room_secret(&pool, "identity-a", "aa", "general", true).unwrap());
        assert!(
            list_channel_room_secrets_for_identity(&pool, "identity-a")
                .unwrap()
                .is_empty()
        );
        let room = &list_saved_channel_rooms(&pool, "identity-a", "aa").unwrap()[0];
        assert!(
            room.desired_joined,
            "forgetting a key preserves room desire"
        );
        assert!(
            room.join_key_required,
            "a rejected key must block keyless reconnect"
        );
    }

    #[test]
    fn removing_a_client_room_cascades_its_sealed_secret() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        set_channel_hub_desired(&pool, "identity-a", "aa", "alpha", true).unwrap();
        set_channel_room_desired(&pool, "identity-a", "aa", "general", true).unwrap();
        save_channel_room_secret(
            &pool,
            "identity-a",
            "aa",
            "general",
            "rns_identity",
            1,
            b"ciphertext",
        )
        .unwrap();

        assert!(remove_channel_room(&pool, "identity-a", "aa", "general").unwrap());
        assert!(
            list_channel_room_secrets_for_identity(&pool, "identity-a")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn renaming_retires_the_old_name_but_keeps_deliberate_hub_aliases() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "Old Name");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");

        // Bookmark carrying a copy of the identity name (auto-prefilled).
        save_channel_hub(&pool, "identity-a", "aa", "Relay", "Old Name", true).unwrap();
        // Bookmark with a deliberate per-hub alias.
        save_channel_hub(&pool, "identity-a", "bb", "Alias relay", "Radio Rat", true).unwrap();
        // Another identity that happens to use the same name.
        save_channel_hub(&pool, "identity-b", "cc", "Other", "Old Name", true).unwrap();

        let updated = rename_identity_and_retire_alias(&pool, "identity-a", "New Name").unwrap();
        assert_eq!(
            updated.retired_bookmarks, 1,
            "only the stale copy is rewritten"
        );
        assert_eq!(updated.previous_name, "Old Name");
        assert_eq!(
            get_identity(&pool, "identity-a")
                .and_then(|identity| identity
                    .get("display_name")
                    .and_then(|value| value.as_str())
                    .map(str::to_string))
                .unwrap_or_default(),
            "New Name",
            "the rename commits with the sweep"
        );

        let hubs = list_saved_channel_hubs(&pool, "identity-a").unwrap();
        let stale = hubs
            .iter()
            .find(|hub| hub.destination_hash == "aa")
            .unwrap();
        let alias = hubs
            .iter()
            .find(|hub| hub.destination_hash == "bb")
            .unwrap();
        assert_eq!(
            stale.nickname, "New Name",
            "superseded name must not survive"
        );
        assert_eq!(
            alias.nickname, "Radio Rat",
            "a deliberate per-hub alias must keep working"
        );

        let other = list_saved_channel_hubs(&pool, "identity-b").unwrap();
        assert_eq!(
            other[0].nickname, "Old Name",
            "another identity's bookmarks are untouched"
        );

        // Renaming to the same name is a no-op sweep.
        assert_eq!(
            rename_identity_and_retire_alias(&pool, "identity-a", "New Name")
                .unwrap()
                .retired_bookmarks,
            0
        );
    }

    fn hub_room(name: &str) -> HubRoomRow {
        HubRoomRow {
            room_name: name.to_string(),
            topic: "field ops".into(),
            key_salt: "aabb".into(),
            key_mac: "ccdd".into(),
            key_pepper_id: "eeff".into(),
            moderated: true,
            invite_only: false,
            topic_ops_only: true,
            no_outside_msgs: true,
            private: true,
            last_used: 1234.0,
            grants: vec![
                ("op".into(), "a".repeat(32), 0.0),
                ("ban".into(), "b".repeat(32), 0.0),
                ("invite".into(), "c".repeat(32), 9_000.0),
            ],
        }
    }

    #[test]
    fn hub_registry_round_trips_rooms_grants_and_klines() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");

        apply_hub_ops(
            &pool,
            "identity-a",
            &[
                HubRoomOp::Upsert(Box::new(hub_room("lobby"))),
                HubRoomOp::ReplaceKlines(vec!["d".repeat(32)]),
            ],
        )
        .unwrap();

        let rooms = list_hub_rooms(&pool, "identity-a").unwrap();
        assert_eq!(rooms.len(), 1);
        let room = &rooms[0];
        assert_eq!(room.room_name, "lobby");
        assert_eq!(room.topic, "field ops");
        assert_eq!(room.key_mac, "ccdd");
        // +p must survive a restart; the reference loses it.
        assert!(room.private && room.moderated && room.topic_ops_only && room.no_outside_msgs);
        assert_eq!(room.last_used, 1234.0);
        let mut kinds: Vec<&str> = room.grants.iter().map(|(k, _, _)| k.as_str()).collect();
        kinds.sort();
        assert_eq!(kinds, vec!["ban", "invite", "op"]);
        assert_eq!(
            list_hub_klines(&pool, "identity-a").unwrap(),
            vec!["d".repeat(32)]
        );
    }

    #[test]
    fn a_room_upsert_replaces_its_grants_wholesale() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        apply_hub_ops(
            &pool,
            "identity-a",
            &[HubRoomOp::Upsert(Box::new(hub_room("lobby")))],
        )
        .unwrap();

        // A revoked op must not survive as a stale row.
        let mut room = hub_room("lobby");
        room.grants = vec![("voice".into(), "e".repeat(32), 0.0)];
        apply_hub_ops(&pool, "identity-a", &[HubRoomOp::Upsert(Box::new(room))]).unwrap();

        let rooms = list_hub_rooms(&pool, "identity-a").unwrap();
        assert_eq!(rooms[0].grants.len(), 1);
        assert_eq!(rooms[0].grants[0].0, "voice");
    }

    #[test]
    fn hub_registry_ops_apply_in_batch_order() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        // Touch-then-remove and remove-then-upsert must not reorder.
        apply_hub_ops(
            &pool,
            "identity-a",
            &[
                HubRoomOp::Upsert(Box::new(hub_room("lobby"))),
                HubRoomOp::Touched {
                    room_name: "lobby".into(),
                    last_used: 4321.0,
                },
                HubRoomOp::Removed {
                    room_name: "lobby".into(),
                },
                HubRoomOp::Upsert(Box::new(hub_room("lobby"))),
            ],
        )
        .unwrap();
        let rooms = list_hub_rooms(&pool, "identity-a").unwrap();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].last_used, 1234.0, "the final upsert wins");
    }

    #[test]
    fn removing_a_room_cascades_its_grants() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        apply_hub_ops(
            &pool,
            "identity-a",
            &[HubRoomOp::Upsert(Box::new(hub_room("lobby")))],
        )
        .unwrap();
        apply_hub_ops(
            &pool,
            "identity-a",
            &[HubRoomOp::Removed {
                room_name: "lobby".into(),
            }],
        )
        .unwrap();

        assert!(list_hub_rooms(&pool, "identity-a").unwrap().is_empty());
        let orphans: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM channel_hub_grants", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(orphans, 0, "grants must not outlive their room");
    }

    #[test]
    fn gc_invites_drops_only_expired_invite_grants() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        apply_hub_ops(
            &pool,
            "identity-a",
            &[HubRoomOp::Upsert(Box::new(hub_room("lobby")))],
        )
        .unwrap();

        apply_hub_ops(
            &pool,
            "identity-a",
            &[HubRoomOp::GcInvites { before: 10_000.0 }],
        )
        .unwrap();
        let rooms = list_hub_rooms(&pool, "identity-a").unwrap();
        let kinds: Vec<&str> = rooms[0].grants.iter().map(|(k, _, _)| k.as_str()).collect();
        assert!(!kinds.contains(&"invite"), "the expired invite is gone");
        assert!(
            kinds.contains(&"op") && kinds.contains(&"ban"),
            "permanent grants (expires_at 0) must never be collected"
        );
    }

    #[test]
    fn hub_registry_is_identity_scoped_and_cascades_with_the_identity() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");
        for id in ["identity-a", "identity-b"] {
            apply_hub_ops(
                &pool,
                id,
                &[
                    HubRoomOp::Upsert(Box::new(hub_room("lobby"))),
                    HubRoomOp::ReplaceKlines(vec!["d".repeat(32)]),
                ],
            )
            .unwrap();
        }

        delete_identity(&pool, "identity-a", true).unwrap();
        assert!(list_hub_rooms(&pool, "identity-a").unwrap().is_empty());
        assert!(list_hub_klines(&pool, "identity-a").unwrap().is_empty());
        assert_eq!(list_hub_rooms(&pool, "identity-b").unwrap().len(), 1);
        assert_eq!(list_hub_klines(&pool, "identity-b").unwrap().len(), 1);
    }

    #[test]
    fn a_hub_room_row_never_debug_prints_its_key() {
        let rendered = format!("{:?}", hub_room("lobby"));
        assert!(!rendered.contains("ccdd"), "the key MAC must not be logged");
        assert!(
            !rendered.contains("aabb"),
            "the key salt must not be logged"
        );
        assert!(rendered.contains("keyed: true"));
    }

    #[test]
    fn a_rename_that_fails_leaves_the_old_name_recoverable() {
        // The sweep must not commit ahead of the rename: if it did, a retry
        // would read the new name as the "previous" one, skip the sweep on the
        // equality guard, and strand the superseded name in the bookmark.
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "Old Name");
        save_channel_hub(&pool, "identity-a", "aa", "Relay", "Old Name", true).unwrap();

        // Force the transaction to fail after the identities write by holding a
        // schema-incompatible state: drop the table the sweep targets.
        pool.get()
            .unwrap()
            .execute("DROP TABLE channel_hubs", [])
            .unwrap();
        assert!(rename_identity_and_retire_alias(&pool, "identity-a", "New Name").is_err());

        // The rename rolled back with it, so the retry still sees the old name
        // as "previous" and can still retire it.
        assert_eq!(
            get_identity(&pool, "identity-a")
                .and_then(|identity| identity
                    .get("display_name")
                    .and_then(|value| value.as_str())
                    .map(str::to_string))
                .unwrap_or_default(),
            "Old Name",
            "a failed rename must not leave the new name committed"
        );
    }
}

/// Overridable via `known_identities_prune_days` (0 disables).
pub const DEFAULT_PRUNE_DAYS: u32 = 14;

/// Soft cap on `known_identities`; cap-based prune backstop. ~2MB at 25k.
pub const SOFT_CAP_IDENTITIES: usize = 25_000;

/// Cap eviction never touches entries fresher than this.
pub const CAP_HARD_FLOOR_DAYS: u32 = 90;

/// `None` disables pruning; `Some(n)` evicts entries older than `n` days.
pub fn get_prune_days(pool: &DbPool) -> Option<u32> {
    let raw = get_setting(pool, "known_identities_prune_days")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_PRUNE_DAYS);
    if raw == 0 { None } else { Some(raw) }
}

/// Upsert one announce per row; `last_seen` + `display_name` + `last_interface`
/// stamped atomically. Empty optionals preserve the existing column.
/// Returns rows touched.
pub fn touch_identity_activity(
    pool: &DbPool,
    rows: &[(String, f64, Option<String>, Option<String>)],
) -> usize {
    touch_identity_activity_for_service(pool, rows, None, PEER_SERVICE_LXMF_DELIVERY)
}

fn normalized_peer_services<'a>(services: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for service in services {
        let service = service.trim();
        if !service.is_empty() && !out.iter().any(|s| s == service) {
            out.push(service.to_string());
        }
    }
    out
}

fn normalized_lxmf_compression_support(value: &str) -> Option<&'static str> {
    match value.trim() {
        LXMF_COMPRESSION_SUPPORT_SUPPORTED => Some(LXMF_COMPRESSION_SUPPORT_SUPPORTED),
        LXMF_COMPRESSION_SUPPORT_UNSUPPORTED => Some(LXMF_COMPRESSION_SUPPORT_UNSUPPORTED),
        _ => None,
    }
}

/// Same as `touch_identity_activity`, but records the service aspect that made
/// the destination actionable for Ratspeak.
pub fn touch_identity_activity_for_service(
    pool: &DbPool,
    rows: &[(String, f64, Option<String>, Option<String>)],
    identity_hash: Option<&str>,
    service: &str,
) -> usize {
    touch_identity_activity_for_services(pool, rows, identity_hash, &[service], false)
}

#[derive(Debug, Clone)]
pub struct IdentityActivityUpdate {
    pub dest_hash: String,
    pub timestamp: f64,
    pub display_name: Option<String>,
    pub status: Option<String>,
    pub last_interface: Option<String>,
    pub identity_hash: Option<String>,
    pub services: Vec<String>,
    pub clear_ratspeak_services: bool,
    pub lxmf_compression_support: Option<String>,
}

/// Same as `touch_identity_activity_for_service`, but merges multiple service
/// tokens in one row update so one announce increments `announce_count` once.
/// `clear_ratspeak_services` removes stale `ratspeak.*` tokens before adding
/// the provided services, allowing peers to opt out on a later announce.
pub fn touch_identity_activity_for_services(
    pool: &DbPool,
    rows: &[(String, f64, Option<String>, Option<String>)],
    identity_hash: Option<&str>,
    services: &[&str],
    clear_ratspeak_services: bool,
) -> usize {
    if rows.is_empty() {
        return 0;
    }
    let services = normalized_peer_services(services.iter().copied());
    let updates: Vec<IdentityActivityUpdate> = rows
        .iter()
        .map(|(hash, ts, name, iface)| IdentityActivityUpdate {
            dest_hash: hash.clone(),
            timestamp: *ts,
            display_name: name.clone(),
            status: None,
            last_interface: iface.clone(),
            identity_hash: identity_hash.map(str::to_owned),
            services: services.clone(),
            clear_ratspeak_services,
            lxmf_compression_support: None,
        })
        .collect();
    touch_identity_activity_updates(pool, &updates)
}

/// Upsert peer activity where each row can carry its own identity hash and
/// service set. Used for announce snapshot backfills from busy hubs.
pub fn touch_identity_activity_updates(pool: &DbPool, updates: &[IdentityActivityUpdate]) -> usize {
    if updates.is_empty() {
        return 0;
    }
    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let mut touched = 0usize;
    {
        let mut existing_stmt = match tx.prepare_cached(
            "SELECT COALESCE(services, '') FROM identity_activity WHERE dest_hash = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let mut stmt = match tx.prepare_cached(
            "INSERT INTO identity_activity(dest_hash, identity_hash, last_seen, first_seen, announce_count, display_name, status, last_interface, services, lxmf_compression_support)
             VALUES (?1, ?2, ?3, ?3, 1, COALESCE(?4, ''), COALESCE(?5, ''), COALESCE(?6, ''), ?7, COALESCE(?8, ''))
             ON CONFLICT(dest_hash) DO UPDATE SET
                 last_seen = MAX(excluded.last_seen, last_seen),
                 announce_count = announce_count + 1,
                 identity_hash = CASE
                     WHEN excluded.identity_hash != '' THEN excluded.identity_hash
                     ELSE identity_hash
                 END,
                 display_name = CASE
                     WHEN excluded.display_name != '' THEN excluded.display_name
                     ELSE display_name
                 END,
                 status = CASE
                     WHEN ?5 IS NOT NULL THEN excluded.status
                     ELSE status
                 END,
                 last_interface = CASE
                     WHEN excluded.last_interface != '' THEN excluded.last_interface
                     ELSE last_interface
                 END,
                 services = excluded.services,
                 lxmf_compression_support = CASE
                     WHEN ?8 IS NOT NULL AND ?8 != '' THEN excluded.lxmf_compression_support
                     ELSE lxmf_compression_support
                 END",
        ) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        for update in updates {
            let services = normalized_peer_services(update.services.iter().map(String::as_str));
            if services.is_empty() {
                continue;
            }
            let n = update.display_name.as_deref().filter(|s| !s.is_empty());
            let i = update.last_interface.as_deref().filter(|s| !s.is_empty());
            let identity_hash = update.identity_hash.as_deref().unwrap_or("").trim();
            let existing_raw = existing_stmt
                .query_row(params![update.dest_hash], |row| row.get::<_, String>(0))
                .unwrap_or_default();
            let mut merged = normalized_peer_services(existing_raw.split(','));
            if update.clear_ratspeak_services {
                merged.retain(|service| !service.starts_with("ratspeak."));
            }
            for service in &services {
                if !merged.iter().any(|existing| existing == service) {
                    merged.push(service.clone());
                }
            }
            let merged_services = merged.join(",");
            let lxmf_compression_support = update
                .lxmf_compression_support
                .as_deref()
                .and_then(normalized_lxmf_compression_support);
            let ok = stmt
                .execute(params![
                    update.dest_hash,
                    identity_hash,
                    update.timestamp,
                    n,
                    update.status.as_deref(),
                    i,
                    merged_services,
                    lxmf_compression_support,
                ])
                .is_ok();
            if ok {
                touched += 1;
            }
        }
    }
    tx.commit().ok();
    touched
}

pub fn get_identity_lxmf_compression_support(pool: &DbPool, dest_hash: &str) -> Option<String> {
    let conn = pool.get().ok()?;
    let raw: String = conn
        .query_row(
            "SELECT COALESCE(lxmf_compression_support, '') FROM identity_activity WHERE dest_hash = ?1",
            params![dest_hash],
            |row| row.get(0),
        )
        .ok()?;
    normalized_lxmf_compression_support(&raw).map(str::to_owned)
}

pub fn set_identity_lxmf_compression_support(
    pool: &DbPool,
    dest_hash: &str,
    support: &str,
) -> bool {
    let Some(support) = normalized_lxmf_compression_support(support) else {
        return false;
    };
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.execute(
        "UPDATE identity_activity SET lxmf_compression_support = ?1 WHERE dest_hash = ?2",
        params![support, dest_hash],
    )
    .map(|rows| rows > 0)
    .unwrap_or(false)
}

pub fn touch_identity_last_heard(pool: &DbPool, dest_hash: &str, timestamp: f64) -> bool {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return false,
    };
    conn.execute(
        "INSERT INTO identity_activity(dest_hash, last_seen, first_seen, announce_count, services)
         VALUES (?1, ?2, ?2, 0, ?3)
         ON CONFLICT(dest_hash) DO UPDATE SET
             last_seen = MAX(excluded.last_seen, last_seen),
             services = CASE
                 WHEN services = '' THEN excluded.services
                 WHEN instr(',' || services || ',', ',' || excluded.services || ',') > 0 THEN services
                 ELSE services || ',' || excluded.services
             END",
        params![dest_hash, timestamp, PEER_SERVICE_LXMF_DELIVERY],
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

pub fn get_identity_activity_first_seen(pool: &DbPool, dest_hash: &str) -> Option<f64> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT first_seen FROM identity_activity WHERE dest_hash = ?1",
        params![dest_hash],
        |row| row.get::<_, f64>(0),
    )
    .ok()
}

/// Same JOIN as `get_peers_snapshot`, scoped to an explicit hash list.
pub fn get_peers_by_hashes(pool: &DbPool, hashes: &[String], identity_id: &str) -> Vec<PeerRow> {
    if hashes.is_empty() {
        return vec![];
    }
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    // Chunk to avoid SQLITE_LIMIT_VARIABLE_NUMBER (default 999).
    let mut out = Vec::with_capacity(hashes.len());
    for chunk in hashes.chunks(500) {
        let placeholders: Vec<String> = (0..chunk.len()).map(|i| format!("?{}", i + 2)).collect();
        let service_filter = peer_service_filter_sql("ia.services");
        let sql = format!(
            "SELECT
                ia.dest_hash,
                ia.last_seen,
                ia.first_seen,
                COALESCE(NULLIF(c.display_name, ''), ia.display_name, '') AS display_name,
                COALESCE(ia.status, '') AS profile_status,
                CASE WHEN c.dest_hash IS NOT NULL THEN 1 ELSE 0 END AS is_contact,
                ia.last_interface,
                ia.identity_hash,
                CASE
                    WHEN c.dest_hash IS NOT NULL AND COALESCE(ia.services, '') = '' THEN '{lxmf}'
                    ELSE COALESCE(ia.services, '')
                END AS services
             FROM identity_activity ia
             LEFT JOIN contacts c ON c.dest_hash = ia.dest_hash AND c.identity_id = ?1
             WHERE ia.dest_hash IN ({})
               AND (c.dest_hash IS NOT NULL OR {service_filter})
               AND ia.dest_hash NOT IN (SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?1)",
            placeholders.join(","),
            lxmf = PEER_SERVICE_LXMF_DELIVERY
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!(
                    reason = "prepare_failed",
                    "get_peers_by_hashes: prepare failed"
                );
                continue;
            }
        };
        let mut params_vec: Vec<&dyn rusqlite::ToSql> =
            Vec::with_capacity(chunk.len().saturating_add(1));
        params_vec.push(&identity_id);
        params_vec.extend(chunk.iter().map(|h| h as &dyn rusqlite::ToSql));
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().copied()),
                |row| {
                    Ok(PeerRow {
                        hash: row.get::<_, String>(0)?,
                        identity_hash: row.get::<_, String>(7)?,
                        last_seen: row.get::<_, Option<f64>>(1)?,
                        first_seen: row.get::<_, Option<f64>>(2)?,
                        display_name: row.get::<_, String>(3)?,
                        profile_status: row.get::<_, String>(4)?,
                        is_contact: row.get::<_, i64>(5)? != 0,
                        last_interface: row.get::<_, String>(6)?,
                        services: parse_peer_services(row.get::<_, String>(8)?),
                    })
                },
            )
            .map(|it| it.filter_map(|r| r.ok()).collect::<Vec<_>>())
            .unwrap_or_default();
        out.extend(rows);
    }
    out
}

pub use ratspeak_core::types::PeerRow;

fn parse_peer_services(raw: String) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn peer_service_filter_sql(column: &str) -> String {
    format!(
        "(instr(',' || COALESCE({column}, '') || ',', ',{lxmf},') > 0 \
          OR instr(',' || COALESCE({column}, '') || ',', ',{lxst},') > 0)",
        lxmf = PEER_SERVICE_LXMF_DELIVERY,
        lxst = PEER_SERVICE_LXST_TELEPHONY
    )
}

/// Active peers (within cutoff) UNION every contact. Display-name precedence:
/// `contacts.display_name` over `identity_activity.display_name`.
pub fn get_peers_snapshot(pool: &DbPool, cutoff_unix: f64, identity_id: &str) -> Vec<PeerRow> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let service_filter = peer_service_filter_sql("ia.services");
    let sql = format!(
        "SELECT
            ia.dest_hash,
            ia.last_seen,
            ia.first_seen,
            COALESCE(NULLIF(c.display_name, ''), ia.display_name, '') AS display_name,
            COALESCE(ia.status, '') AS profile_status,
            CASE WHEN c.dest_hash IS NOT NULL THEN 1 ELSE 0 END AS is_contact,
            ia.last_interface,
            ia.identity_hash,
            COALESCE(ia.services, '') AS services
         FROM identity_activity ia
         LEFT JOIN contacts c ON c.dest_hash = ia.dest_hash AND c.identity_id = ?2
         WHERE ia.last_seen >= ?1
           AND c.dest_hash IS NULL
           AND {service_filter}
           AND ia.dest_hash NOT IN (SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?2)
         UNION ALL
         SELECT
            c.dest_hash,
            ia.last_seen,
            ia.first_seen,
            COALESCE(NULLIF(c.display_name, ''), ia.display_name, '') AS display_name,
            COALESCE(ia.status, '') AS profile_status,
            1 AS is_contact,
            COALESCE(ia.last_interface, '') AS last_interface,
            COALESCE(ia.identity_hash, '') AS identity_hash,
            CASE
                WHEN COALESCE(ia.services, '') = '' THEN '{lxmf}'
                ELSE ia.services
            END AS services
         FROM contacts c
         LEFT JOIN identity_activity ia ON ia.dest_hash = c.dest_hash
         WHERE c.identity_id = ?2
           AND c.dest_hash NOT IN (SELECT dest_hash FROM blocked_contacts WHERE identity_id = ?2)",
        lxmf = PEER_SERVICE_LXMF_DELIVERY
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => {
            tracing::warn!(
                reason = "prepare_failed",
                "get_peers_snapshot: prepare failed"
            );
            return vec![];
        }
    };

    stmt.query_map(params![cutoff_unix, identity_id], |row| {
        Ok(PeerRow {
            hash: row.get::<_, String>(0)?,
            identity_hash: row.get::<_, String>(7)?,
            last_seen: row.get::<_, Option<f64>>(1)?,
            first_seen: row.get::<_, Option<f64>>(2)?,
            display_name: row.get::<_, String>(3)?,
            profile_status: row.get::<_, String>(4)?,
            is_contact: row.get::<_, i64>(5)? != 0,
            last_interface: row.get::<_, String>(6)?,
            services: parse_peer_services(row.get::<_, String>(8)?),
        })
    })
    .map(|it| it.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

/// Protected: contacts, blocked, message counterparties, propagation_node,
/// `protected_extra`.
pub fn find_prune_candidates(
    pool: &DbPool,
    cutoff_unix: f64,
    protected_extra: &std::collections::HashSet<String>,
) -> Vec<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT dest_hash FROM identity_activity
         WHERE last_seen < ?1
           AND dest_hash NOT IN (SELECT dest_hash FROM contacts)
           AND dest_hash NOT IN (SELECT dest_hash FROM blocked_contacts)
           AND dest_hash NOT IN (SELECT source      FROM messages WHERE source      != '')
           AND dest_hash NOT IN (SELECT destination FROM messages WHERE destination != '')
           AND dest_hash NOT IN (SELECT propagation_node FROM identities WHERE propagation_node != '')"
    ) { Ok(s) => s, Err(_) => return vec![] };
    let rows: Vec<String> = stmt
        .query_map(params![cutoff_unix], |row| row.get::<_, String>(0))
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    if protected_extra.is_empty() {
        rows
    } else {
        rows.into_iter()
            .filter(|h| !protected_extra.contains(h))
            .collect()
    }
}

/// Oldest non-protected (same rules as `find_prune_candidates`) older than
/// `cutoff_unix`, up to `limit`.
pub fn find_cap_eviction_candidates(
    pool: &DbPool,
    cutoff_unix: f64,
    limit: usize,
    protected_extra: &std::collections::HashSet<String>,
) -> Vec<String> {
    if limit == 0 {
        return vec![];
    }
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    // 4x over-fetch absorbs protected_extra filtering in one round-trip.
    let sql_limit = limit.saturating_mul(4).min(100_000) as i64;
    let mut stmt = match conn.prepare(
        "SELECT dest_hash FROM identity_activity
         WHERE last_seen < ?1
           AND dest_hash NOT IN (SELECT dest_hash FROM contacts)
           AND dest_hash NOT IN (SELECT dest_hash FROM blocked_contacts)
           AND dest_hash NOT IN (SELECT source      FROM messages WHERE source      != '')
           AND dest_hash NOT IN (SELECT destination FROM messages WHERE destination != '')
           AND dest_hash NOT IN (SELECT propagation_node FROM identities WHERE propagation_node != '')
         ORDER BY last_seen ASC
         LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows: Vec<String> = stmt
        .query_map(params![cutoff_unix, sql_limit], |row| {
            row.get::<_, String>(0)
        })
        .map(|it| it.filter_map(|r| r.ok()).collect())
        .unwrap_or_default();
    if protected_extra.is_empty() {
        rows.into_iter().take(limit).collect()
    } else {
        rows.into_iter()
            .filter(|h| !protected_extra.contains(h))
            .take(limit)
            .collect()
    }
}

/// Chunked at 500 to stay under SQLite's default parameter limit.
pub fn delete_identity_activity(pool: &DbPool, hashes: &[String]) -> usize {
    if hashes.is_empty() {
        return 0;
    }
    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let mut deleted = 0usize;
    for chunk in hashes.chunks(500) {
        let placeholders: String = (1..=chunk.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM identity_activity WHERE dest_hash IN ({placeholders})");
        let params: Vec<&dyn rusqlite::types::ToSql> = chunk
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        match tx.execute(&sql, params.as_slice()) {
            Ok(n) => deleted += n,
            Err(_) => {
                // Continue on chunk failure; pruner retries next pass.
                tracing::warn!(
                    chunk_len = chunk.len(),
                    reason = "delete_failed",
                    "delete_identity_activity chunk failed; remaining chunks will still be attempted"
                );
            }
        }
    }
    if tx.commit().is_err() {
        tracing::error!(
            reason = "commit_failed",
            "delete_identity_activity commit failed — deletions discarded"
        );
        return 0;
    }
    deleted
}

/// Clear discovered peer activity while preserving rows needed by user data.
///
/// Contacts, blocked identities, message counterparties, and configured
/// propagation nodes are not merely cache; keeping those rows preserves name
/// resolution and conversation affordances after an announce-cache clear.
pub fn clear_discovered_identity_activity(pool: &DbPool) -> usize {
    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return 0,
    };
    let deleted = tx
        .execute(
            "DELETE FROM identity_activity AS ia
             WHERE NOT EXISTS (
                 SELECT 1 FROM contacts c WHERE c.dest_hash = ia.dest_hash
             )
               AND NOT EXISTS (
                 SELECT 1 FROM blocked_contacts b WHERE b.dest_hash = ia.dest_hash
             )
               AND NOT EXISTS (
                 SELECT 1 FROM messages m WHERE m.source = ia.dest_hash
             )
               AND NOT EXISTS (
                 SELECT 1 FROM messages m WHERE m.destination = ia.dest_hash
             )
               AND NOT EXISTS (
                 SELECT 1 FROM identities i
                  WHERE COALESCE(i.propagation_node, '') = ia.dest_hash
             )",
            [],
        )
        .unwrap_or(0);
    if tx.commit().is_err() {
        return 0;
    }
    deleted
}

/// `ON CONFLICT DO NOTHING`: only stamps unseen peers.
pub fn seed_identity_activity_now(pool: &DbPool, hashes: &[String]) {
    if hashes.is_empty() {
        return;
    }
    let now = now_ts();
    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let tx = match conn.transaction() {
        Ok(t) => t,
        Err(_) => return,
    };
    {
        let mut stmt = match tx.prepare_cached(
            "INSERT INTO identity_activity(dest_hash, last_seen, first_seen, announce_count)
             VALUES (?1, ?2, ?2, 0)
             ON CONFLICT(dest_hash) DO NOTHING",
        ) {
            Ok(s) => s,
            Err(_) => return,
        };
        for hash in hashes {
            let _ = stmt.execute(params![hash, now]);
        }
    }
    tx.commit().ok();
}

pub fn save_reaction(
    pool: &DbPool,
    message_id: &str,
    sender: &str,
    emoji: &str,
    identity_id: &str,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "INSERT OR IGNORE INTO reactions (message_id, sender, emoji, timestamp, identity_id) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![message_id, sender, emoji, now_ts(), identity_id],
    ).ok();
}

pub fn remove_reaction(
    pool: &DbPool,
    message_id: &str,
    sender: &str,
    emoji: &str,
    identity_id: &str,
) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "DELETE FROM reactions WHERE message_id = ?1 AND sender = ?2 AND emoji = ?3 AND identity_id = ?4",
        params![message_id, sender, emoji, identity_id],
    ).ok();
}

fn get_reactions_batch(
    conn: &Connection,
    message_ids: &[String],
    identity_id: &str,
) -> std::collections::HashMap<String, Vec<serde_json::Value>> {
    if message_ids.is_empty() {
        return Default::default();
    }
    let placeholders: String = (0..message_ids.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT message_id, sender, emoji, timestamp FROM reactions WHERE message_id IN ({placeholders}) AND identity_id = ?{} ORDER BY timestamp ASC",
        message_ids.len() + 1,
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Default::default(),
    };

    let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = message_ids
        .iter()
        .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
        .collect();
    params_vec.push(Box::new(identity_id.to_string()));

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        params_vec.iter().map(|p| p.as_ref()).collect();

    let mut result: std::collections::HashMap<String, Vec<serde_json::Value>> = Default::default();
    if let Ok(rows) = stmt.query_map(param_refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, f64>(3)?,
        ))
    }) {
        for r in rows.flatten() {
            result.entry(r.0).or_default().push(serde_json::json!({
                "sender": r.1, "emoji": r.2, "timestamp": r.3,
            }));
        }
    }
    result
}

pub fn get_reactions_for_message(
    pool: &DbPool,
    message_id: &str,
    identity_id: &str,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT sender, emoji, timestamp FROM reactions WHERE message_id = ?1 AND identity_id = ?2 ORDER BY timestamp ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = match stmt.query_map(params![message_id, identity_id], |row| {
        Ok(serde_json::json!({
            "sender": row.get::<_, String>(0)?,
            "emoji": row.get::<_, String>(1)?,
            "timestamp": row.get::<_, f64>(2)?,
        }))
    }) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };
    rows.flatten().collect()
}

pub fn get_connection_history(pool: &DbPool, limit: i64) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt =
        match conn.prepare("SELECT * FROM connection_history ORDER BY last_used DESC LIMIT ?1") {
            Ok(s) => s,
            Err(_) => return vec![],
        };

    stmt.query_map(params![limit], |row| {
        Ok(serde_json::json!({
            "id": row.get::<_, i64>(0)?,
            "host": row.get::<_, String>(1)?,
            "port": row.get::<_, i64>(2)?,
            "name": row.get::<_, String>(3).unwrap_or_default(),
            "last_used": row.get::<_, f64>(4)?,
            "times_used": row.get::<_, i64>(5).unwrap_or(1),
        }))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn delete_connection_history(pool: &DbPool, history_id: i64) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "DELETE FROM connection_history WHERE id = ?1",
        params![history_id],
    )
    .ok();
}

pub fn save_connection_history(pool: &DbPool, host: &str, port: i64, name: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let now = now_ts();
    let existing: Option<(i64, i64)> = conn
        .query_row(
            "SELECT id, times_used FROM connection_history WHERE host = ?1 AND port = ?2",
            params![host, port],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .ok();

    if let Some((id, _)) = existing {
        conn.execute(
            "UPDATE connection_history SET last_used = ?1, times_used = times_used + 1, name = CASE WHEN ?2 != '' THEN ?2 ELSE name END WHERE id = ?3",
            params![now, name, id],
        ).ok();
    } else {
        conn.execute(
            "INSERT INTO connection_history (host, port, name, last_used, times_used) VALUES (?1, ?2, ?3, ?4, 1)",
            params![host, port, name, now],
        ).ok();
    }
}

pub fn clear_all_messages(pool: &DbPool, identity_id: &str) -> Vec<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut file_refs = Vec::new();
    if identity_id.is_empty() {
        if let Ok(mut stmt) =
            conn.prepare("SELECT attachment_stored_name, image_stored_name FROM messages")
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0).unwrap_or_default(),
                    row.get::<_, String>(1).unwrap_or_default(),
                ))
            })
        {
            for r in rows.flatten() {
                if !r.0.is_empty() {
                    file_refs.push(r.0);
                }
                if !r.1.is_empty() {
                    file_refs.push(r.1);
                }
            }
        }
    } else {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT attachment_stored_name, image_stored_name FROM messages WHERE identity_id = ?1",
        ) && let Ok(rows) = stmt.query_map(params![identity_id], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
            ))
        }) {
            for r in rows.flatten() {
                if !r.0.is_empty() {
                    file_refs.push(r.0);
                }
                if !r.1.is_empty() {
                    file_refs.push(r.1);
                }
            }
        }
    }
    if identity_id.is_empty() {
        conn.execute("DELETE FROM messages", []).ok();
    } else {
        conn.execute(
            "DELETE FROM messages WHERE identity_id = ?1",
            params![identity_id],
        )
        .ok();
    }
    file_refs
}

pub fn get_identity_file_refs(pool: &DbPool, identity_id: &str) -> Vec<String> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    if identity_id.is_empty() {
        return vec![];
    }
    let mut file_refs = Vec::new();
    if let Ok(mut stmt) = conn.prepare(
        "SELECT attachment_stored_name, image_stored_name FROM messages WHERE identity_id = ?1",
    ) && let Ok(rows) = stmt.query_map(params![identity_id], |row| {
        Ok((
            row.get::<_, String>(0).unwrap_or_default(),
            row.get::<_, String>(1).unwrap_or_default(),
        ))
    }) {
        for r in rows.flatten() {
            if !r.0.is_empty() {
                file_refs.push(r.0);
            }
            if !r.1.is_empty() {
                file_refs.push(r.1);
            }
        }
    }
    file_refs
}

pub fn clear_all_contacts(pool: &DbPool, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    if identity_id.is_empty() {
        conn.execute("DELETE FROM contacts", []).ok();
    } else {
        conn.execute(
            "DELETE FROM contacts WHERE identity_id = ?1",
            params![identity_id],
        )
        .ok();
    }
}

pub fn clear_all_blocked(pool: &DbPool, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    if identity_id.is_empty() {
        conn.execute("DELETE FROM blocked_contacts", []).ok();
    } else {
        conn.execute(
            "DELETE FROM blocked_contacts WHERE identity_id = ?1",
            params![identity_id],
        )
        .ok();
    }
}

pub fn get_database_stats(pool: &DbPool) -> serde_json::Value {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return serde_json::json!({"messages": 0, "contacts": 0, "connection_history": 0}),
    };
    let msg_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap_or(0);
    let contact_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| row.get(0))
        .unwrap_or(0);
    let history_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM connection_history", [], |row| {
            row.get(0)
        })
        .unwrap_or(0);
    serde_json::json!({
        "messages": msg_count,
        "contacts": contact_count,
        "connection_history": history_count,
    })
}

pub fn backfill_identity_id(pool: &DbPool, identity_hash: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "UPDATE contacts SET identity_id = ?1 WHERE identity_id = ''",
        params![identity_hash],
    )
    .ok();
    conn.execute(
        "UPDATE messages SET identity_id = ?1 WHERE identity_id = ''",
        params![identity_hash],
    )
    .ok();
    tracing::info!("Backfilled identity_id on existing contacts/messages");
}

pub fn save_game_session(pool: &DbPool, session: &lrgp::session::Session) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    let metadata_json = serde_json::to_string(&session.metadata).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT OR REPLACE INTO app_sessions (session_id, identity_id, app_id, app_version, contact_hash, initiator, status, metadata, unread, created_at, updated_at, last_action_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            session.session_id, session.identity_id, session.app_id, session.app_version,
            session.contact_hash, session.initiator, session.status, metadata_json,
            session.unread, session.created_at, session.updated_at, session.last_action_at,
        ],
    ).ok();
}

pub fn get_game_session(
    pool: &DbPool,
    session_id: &str,
    identity_id: &str,
) -> Option<serde_json::Value> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT * FROM app_sessions WHERE session_id = ?1 AND identity_id = ?2",
        params![session_id, identity_id],
        row_to_app_session,
    )
    .ok()
}

pub fn list_game_sessions(
    pool: &DbPool,
    identity_id: &str,
    contact_hash: Option<&str>,
    status: Option<&str>,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut sql = "SELECT * FROM app_sessions WHERE identity_id = ?1".to_string();
    let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(identity_id.to_string())];

    if let Some(ch) = contact_hash {
        sql.push_str(&format!(" AND contact_hash = ?{}", param_values.len() + 1));
        param_values.push(Box::new(ch.to_string()));
    }
    if let Some(st) = status {
        sql.push_str(&format!(" AND status = ?{}", param_values.len() + 1));
        param_values.push(Box::new(st.to_string()));
    }
    sql.push_str(" ORDER BY last_action_at DESC");

    let param_refs: Vec<&dyn rusqlite::types::ToSql> =
        param_values.iter().map(|p| p.as_ref()).collect();
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    stmt.query_map(param_refs.as_slice(), row_to_app_session)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

pub fn save_game_action(pool: &DbPool, action: &lrgp::store::Action, envelope_mp: Option<&[u8]>) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "INSERT OR REPLACE INTO app_actions (session_id, identity_id, action_num, command, payload_json, sender, timestamp, envelope_mp) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            action.session_id, action.identity_id, action.action_num,
            action.command, action.payload_json, action.sender, action.timestamp,
            envelope_mp,
        ],
    ).ok();
}

/// Returns the packed LRGP envelope for the active identity's most recent
/// outbound action in this session. Used by the manual "Resend last move"
/// path so we re-transmit the same envelope rather than re-dispatching.
pub fn get_last_outbound_envelope_for_session(
    pool: &DbPool,
    session_id: &str,
    identity_id: &str,
) -> Option<Vec<u8>> {
    let conn = pool.get().ok()?;
    conn.query_row(
        "SELECT envelope_mp FROM app_actions
         WHERE session_id = ?1 AND identity_id = ?2 AND sender = ?2 AND envelope_mp IS NOT NULL
         ORDER BY action_num DESC LIMIT 1",
        params![session_id, identity_id],
        |row| row.get::<_, Option<Vec<u8>>>(0),
    )
    .ok()
    .flatten()
}

pub fn get_game_actions(
    pool: &DbPool,
    session_id: &str,
    identity_id: &str,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut stmt = match conn.prepare(
        "SELECT * FROM app_actions WHERE session_id = ?1 AND identity_id = ?2 ORDER BY action_num ASC"
    ) { Ok(s) => s, Err(_) => return vec![] };

    stmt.query_map(params![session_id, identity_id], |row| {
        let payload_str: String = row.get::<_, String>(4).unwrap_or_else(|_| "{}".into());
        let payload: serde_json::Value =
            serde_json::from_str(&payload_str).unwrap_or(serde_json::json!({}));
        Ok(serde_json::json!({
            "session_id": row.get::<_, String>(0)?,
            "identity_id": row.get::<_, String>(1).unwrap_or_default(),
            "action_num": row.get::<_, i64>(2)?,
            "command": row.get::<_, String>(3)?,
            "payload": payload,
            "sender": row.get::<_, String>(5)?,
            "timestamp": row.get::<_, f64>(6)?,
        }))
    })
    .map(|rows| rows.filter_map(|r| r.ok()).collect())
    .unwrap_or_default()
}

pub fn get_game_action_count(pool: &DbPool, session_id: &str, identity_id: &str) -> i64 {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    conn.query_row(
        "SELECT COUNT(*) FROM app_actions WHERE session_id = ?1 AND identity_id = ?2",
        params![session_id, identity_id],
        |row| row.get(0),
    )
    .unwrap_or(0)
}

pub fn mark_game_read(pool: &DbPool, session_id: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "UPDATE app_sessions SET unread = 0 WHERE session_id = ?1 AND identity_id = ?2",
        params![session_id, identity_id],
    )
    .ok();
}

pub fn delete_game_session(pool: &DbPool, session_id: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "DELETE FROM app_actions WHERE session_id = ?1 AND identity_id = ?2",
        params![session_id, identity_id],
    )
    .ok();
    conn.execute(
        "DELETE FROM app_sessions WHERE session_id = ?1 AND identity_id = ?2",
        params![session_id, identity_id],
    )
    .ok();
}

pub fn get_failed_messages_for_contact(
    pool: &DbPool,
    dest_hash: &str,
    identity_id: &str,
) -> Vec<serde_json::Value> {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        - 3600.0;
    let mut stmt = match conn.prepare(
        "SELECT * FROM messages WHERE destination = ?1 AND identity_id = ?2 AND state = 'failed' AND direction = 'outbound' AND timestamp > ?3 ORDER BY timestamp ASC"
    ) { Ok(s) => s, Err(_) => return vec![] };

    stmt.query_map(params![dest_hash, identity_id, cutoff], row_to_message)
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
}

/// Bypasses the terminal-state guard for an intentional retry.
pub fn mark_message_resent(pool: &DbPool, msg_id: &str, identity_id: &str) {
    let conn = match pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };
    conn.execute(
        "UPDATE messages SET state = 'resent' \
         WHERE id = ?1 AND identity_id = ?2 AND direction = 'outbound' AND state = 'failed'",
        params![msg_id, identity_id],
    )
    .ok();
}

fn row_to_identity(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "hash": row.get::<_, String>(0)?,
        "lxmf_hash": row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        "nickname": row.get::<_, String>(2).unwrap_or_default(),
        "display_name": row.get::<_, String>(3).unwrap_or_default(),
        "status": row.get::<_, String>(4).unwrap_or_default(),
        "created_at": row.get::<_, f64>(5)?,
        "last_used": row.get::<_, Option<f64>>(6)?,
        "is_active": row.get::<_, i64>(7).unwrap_or(0),
        "propagation_node": row.get::<_, String>(8).unwrap_or_default(),
        "propagation_enabled": row.get::<_, i64>(9).unwrap_or(0),
        "propagation_mode": row.get::<_, String>(10).unwrap_or_else(|_| "auto".to_string()),
        "propagation_auto_favor_static": row.get::<_, i64>(11).unwrap_or(1),
    }))
}

fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "dest_hash": row.get::<_, String>(0)?,
        "identity_id": row.get::<_, String>(1).unwrap_or_default(),
        "display_name": row.get::<_, Option<String>>(2)?,
        "identity_pubkey": row.get::<_, Option<String>>(3)?,
        "first_seen": row.get::<_, Option<f64>>(4)?,
        "last_seen": row.get::<_, Option<f64>>(5)?,
        "trust": row.get::<_, String>(6).unwrap_or("pending".into()),
        "notes": row.get::<_, String>(7).unwrap_or_default(),
        "services": parse_peer_services(row.get::<_, String>(8).unwrap_or_default()),
    }))
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    let attachment_name = row.get::<_, String>(12).unwrap_or_default();
    let attachment_stored_name = row.get::<_, String>(13).unwrap_or_default();
    let image_name = row.get::<_, String>(14).unwrap_or_default();
    let image_stored_name = row.get::<_, String>(15).unwrap_or_default();

    // Reshape flat columns to nested `msg.image` / `msg.attachments`.
    let image_json = (!image_stored_name.is_empty()).then(|| {
        serde_json::json!({
            "stored_name": image_stored_name,
            "filename": image_name,
        })
    });
    let attachments_json = (!attachment_stored_name.is_empty()).then(|| {
        serde_json::json!([{
            "filename": attachment_name,
            "stored_name": attachment_stored_name,
        }])
    });

    Ok(serde_json::json!({
        "id": row.get::<_, String>(0)?,
        "source": row.get::<_, String>(1)?,
        "destination": row.get::<_, String>(2)?,
        "content": row.get::<_, String>(3).unwrap_or_default(),
        "title": row.get::<_, String>(4).unwrap_or_default(),
        "timestamp": row.get::<_, f64>(5)?,
        "state": row.get::<_, String>(6).unwrap_or("unknown".into()),
        "direction": row.get::<_, String>(7).unwrap_or("outbound".into()),
        "rtt_ms": row.get::<_, Option<f64>>(8)?,
        "hops": row.get::<_, Option<i64>>(9)?,
        "path": row.get::<_, Option<String>>(10)?,
        "identity_id": row.get::<_, String>(11).unwrap_or_default(),
        "image": image_json,
        "attachments": attachments_json,
        "reply_to_id": row.get::<_, String>(16).unwrap_or_default(),
        "reply_to_preview": row.get::<_, String>(17).unwrap_or_default(),
        "game_id": row.get::<_, String>(18).unwrap_or_default(),
        "game_action": row.get::<_, String>(19).unwrap_or_default(),
        "game_move_san": row.get::<_, String>(20).unwrap_or_default(),
        "delivery_method": row.get::<_, Option<String>>(21)?,
    }))
}

fn row_to_app_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    let metadata_str: String = row.get::<_, String>(7).unwrap_or_else(|_| "{}".into());
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).unwrap_or(serde_json::json!({}));
    let session_id = row.get::<_, String>(0)?;
    let identity_id = row.get::<_, String>(1).unwrap_or_default();
    let initiator = row.get::<_, String>(5).unwrap_or_default();

    let mut obj = serde_json::json!({
        "game_id": session_id.clone(),
        "session_id": session_id,
        "identity_id": identity_id.clone(),
        "my_lxmf_hash": identity_id,
        "app_id": row.get::<_, String>(2)?,
        "app_version": row.get::<_, i64>(3).unwrap_or(1),
        "contact_hash": row.get::<_, String>(4)?,
        "initiator": initiator.clone(),
        "challenger": initiator,
        "status": row.get::<_, String>(6).unwrap_or("pending".into()),
        "metadata": metadata.clone(),
        "unread": row.get::<_, i64>(8).unwrap_or(0),
        "created_at": row.get::<_, f64>(9).unwrap_or(0.0),
        "updated_at": row.get::<_, f64>(10).unwrap_or(0.0),
        "last_action_at": row.get::<_, f64>(11).unwrap_or(0.0),
    });

    // Lift known metadata keys to top-level for the frontend.
    if let serde_json::Value::Object(meta) = &metadata
        && let Some(obj_map) = obj.as_object_mut()
    {
        if let Some(board) = meta.get("board") {
            obj_map.insert("state".to_string(), board.clone());
        }
        for key in &[
            "turn",
            "first_turn",
            "my_marker",
            "winner",
            "terminal",
            "draw_offered",
            "move_count",
            "cancelled_by_initiator",
            "delivery_state",
            "fen",
            "legal_moves",
            "last_move",
            "in_check",
            "my_color",
            "terminal_reason",
            "draw_offer_reason",
        ] {
            if let Some(val) = meta.get(*key) {
                obj_map.insert(key.to_string(), val.clone());
            }
        }
    }

    Ok(obj)
}

#[cfg(test)]
mod unread_breakdown_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    // Test fixture mirrors the subset of message columns under assertion.
    #[allow(clippy::too_many_arguments)]
    fn insert_msg(
        pool: &DbPool,
        id: &str,
        source: &str,
        dest: &str,
        content: &str,
        ts: f64,
        state: &str,
        direction: &str,
        identity_id: &str,
    ) {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO messages (id, source, destination, content, title, timestamp, state, direction, identity_id)
             VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8)",
            params![id, source, dest, content, ts, state, direction, identity_id],
        )
        .unwrap();
    }

    fn insert_contact(pool: &DbPool, dest_hash: &str, display_name: &str, identity_id: &str) {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO contacts (dest_hash, identity_id, display_name, first_seen, last_seen)
             VALUES (?1, ?2, ?3, 0, 0)",
            params![dest_hash, identity_id, display_name],
        )
        .unwrap();
    }

    #[test]
    fn breakdown_empty_when_no_unread() {
        let pool = test_pool();
        let rows = get_unread_breakdown(&pool, "me");
        assert!(rows.is_empty());
    }

    #[test]
    fn breakdown_groups_by_sender_and_orders_by_timestamp_desc() {
        let pool = test_pool();
        insert_msg(
            &pool,
            "a1",
            "alice",
            "me",
            "hi1",
            100.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool,
            "a2",
            "alice",
            "me",
            "hi2",
            200.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool,
            "b1",
            "bob",
            "me",
            "hello",
            150.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool, "o1", "me", "bob", "reply", 160.0, "sent", "outbound", "me",
        );
        insert_msg(
            &pool, "a0", "alice", "me", "read_me", 50.0, "read", "inbound", "me",
        );
        insert_contact(&pool, "alice", "Alice Display", "me");

        let rows = get_unread_breakdown(&pool, "me");
        assert_eq!(rows.len(), 2, "expected two unread senders, got {rows:?}");

        assert_eq!(rows[0].0, "alice");
        assert_eq!(rows[0].1, Some("Alice Display".to_string()));
        assert_eq!(rows[0].2, 2, "alice should have 2 unread");
        assert_eq!(rows[0].3, "hi2", "preview should be newest unread content");
        assert!((rows[0].4 - 200.0).abs() < f64::EPSILON);

        assert_eq!(rows[1].0, "bob");
        assert_eq!(rows[1].1, None, "bob has no contact row");
        assert_eq!(rows[1].2, 1);
        assert_eq!(rows[1].3, "hello");
    }

    #[test]
    fn breakdown_isolates_by_identity_id() {
        let pool = test_pool();
        insert_msg(
            &pool,
            "x1",
            "alice",
            "meA",
            "for A",
            100.0,
            "delivered",
            "inbound",
            "meA",
        );
        insert_msg(
            &pool,
            "x2",
            "alice",
            "meB",
            "for B",
            100.0,
            "delivered",
            "inbound",
            "meB",
        );

        let a = get_unread_breakdown(&pool, "meA");
        let b = get_unread_breakdown(&pool, "meB");
        let c = get_unread_breakdown(&pool, "meC");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].3, "for A");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].3, "for B");
        assert!(c.is_empty());
    }

    #[test]
    fn breakdown_excludes_outbound_and_read() {
        let pool = test_pool();
        insert_msg(
            &pool,
            "1",
            "alice",
            "me",
            "unread",
            100.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool,
            "2",
            "alice",
            "me",
            "already_read",
            90.0,
            "read",
            "inbound",
            "me",
        );
        insert_msg(
            &pool, "3", "me", "alice", "outbound", 110.0, "sent", "outbound", "me",
        );

        let rows = get_unread_breakdown(&pool, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "alice");
        assert_eq!(rows[0].2, 1, "only the single unread inbound should count");
        assert_eq!(rows[0].3, "unread");
    }

    #[test]
    fn total_matches_sum_of_breakdown() {
        let pool = test_pool();
        insert_msg(
            &pool,
            "1",
            "alice",
            "me",
            "m",
            10.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool,
            "2",
            "alice",
            "me",
            "m",
            11.0,
            "delivered",
            "inbound",
            "me",
        );
        insert_msg(
            &pool,
            "3",
            "bob",
            "me",
            "m",
            20.0,
            "delivered",
            "inbound",
            "me",
        );

        let rows = get_unread_breakdown(&pool, "me");
        let legacy_total: i64 = get_all_unread_counts(&pool, "me").values().sum();
        let breakdown_total: i64 = rows.iter().map(|(_, _, c, _, _)| *c).sum();
        assert_eq!(legacy_total, breakdown_total);
        assert_eq!(breakdown_total, 3);
    }

    #[test]
    fn clear_all_messages_returns_attachment_file_refs_for_identity() {
        let pool = test_pool();
        save_message(
            &pool,
            "with-file",
            "me",
            "peer",
            "content",
            "",
            10.0,
            "sent",
            "outbound",
            "me",
            "note.txt",
            "123_note.txt",
            "",
            "456_image.png",
            "",
            "",
            Some("direct"),
        );
        save_message(
            &pool,
            "other-identity",
            "me",
            "peer",
            "content",
            "",
            10.0,
            "sent",
            "outbound",
            "other",
            "other.txt",
            "789_other.txt",
            "",
            "",
            "",
            "",
            Some("direct"),
        );

        let refs = clear_all_messages(&pool, "me");

        assert_eq!(
            refs,
            vec!["123_note.txt".to_string(), "456_image.png".to_string()]
        );
        assert_eq!(get_conversation(&pool, "peer", "me", 10).len(), 0);
        assert_eq!(get_conversation(&pool, "peer", "other", 10).len(), 1);
    }

    #[test]
    fn same_lxmf_message_id_can_exist_for_different_local_identities() {
        let pool = test_pool();
        let shared_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        save_message(
            &pool,
            shared_id,
            "identity-a-lxmf",
            "peer-b",
            "outbound copy",
            "",
            10.0,
            "propagated",
            "outbound",
            "identity-a",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("propagated"),
        );
        save_message(
            &pool,
            shared_id,
            "peer-a",
            "identity-b-lxmf",
            "inbound copy",
            "",
            20.0,
            "received",
            "inbound",
            "identity-b",
            "",
            "",
            "",
            "",
            "",
            "",
            None,
        );

        assert!(message_exists_for_identity(&pool, shared_id, "identity-a"));
        assert!(message_exists_for_identity(&pool, shared_id, "identity-b"));

        let conn = pool.get().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE id = ?1",
                params![shared_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
        drop(conn);

        let identity_a = get_conversation(&pool, "peer-b", "identity-a", 10);
        let identity_b = get_conversation(&pool, "peer-a", "identity-b", 10);
        assert_eq!(identity_a.len(), 1);
        assert_eq!(identity_b.len(), 1);
        assert_eq!(
            identity_b[0].get("content").and_then(|v| v.as_str()),
            Some("inbound copy")
        );
    }

    #[test]
    fn update_message_delivery_method_changes_existing_row() {
        let pool = test_pool();
        save_message(
            &pool,
            "msg",
            "me",
            "peer",
            "content",
            "",
            10.0,
            "sending",
            "outbound",
            "me",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("direct"),
        );

        update_message_delivery_method(&pool, "msg", "me", "propagated");

        assert_eq!(
            get_message_delivery_method(&pool, "msg", "me").as_deref(),
            Some("propagated")
        );
    }

    #[test]
    fn outbound_state_updates_do_not_touch_inbound_duplicate_ids() {
        let pool = test_pool();
        let shared_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        save_message(
            &pool,
            shared_id,
            "me-a",
            "peer",
            "outbound",
            "",
            10.0,
            "sending",
            "outbound",
            "identity-a",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("direct"),
        );
        save_message(
            &pool,
            shared_id,
            "peer",
            "me-b",
            "inbound",
            "",
            20.0,
            "received",
            "inbound",
            "identity-b",
            "",
            "",
            "",
            "",
            "",
            "",
            None,
        );

        update_message_state(&pool, shared_id, "identity-a", "delivered", Some(12.0));

        let conn = pool.get().unwrap();
        let inbound_state: String = conn
            .query_row(
                "SELECT state FROM messages WHERE id = ?1 AND identity_id = 'identity-b'",
                params![shared_id],
                |row| row.get(0),
            )
            .unwrap();
        let outbound_state: String = conn
            .query_row(
                "SELECT state FROM messages WHERE id = ?1 AND identity_id = 'identity-a'",
                params![shared_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(inbound_state, "received");
        assert_eq!(outbound_state, "delivered");
    }

    /// T1-14: state updates are identity-scoped — a delivery proof, method
    /// change, or cancel handled for identity A must not flip identity B's
    /// row with the same message hash.
    #[test]
    fn message_state_updates_are_identity_scoped() {
        let pool = test_pool();
        let shared_id = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        for identity in ["identity-a", "identity-b"] {
            save_message(
                &pool,
                shared_id,
                "me",
                "peer",
                "content",
                "",
                10.0,
                "sent",
                "outbound",
                identity,
                "",
                "",
                "",
                "",
                "",
                "",
                Some("direct"),
            );
        }

        update_message_state(&pool, shared_id, "identity-a", "delivered", Some(8.0));
        update_message_delivery_method(&pool, shared_id, "identity-a", "propagated");

        // Fresh connection per query: the test pool has max_size 1, so a held
        // checkout would starve the update calls below.
        let state_of = |identity: &str| -> String {
            pool.get()
                .unwrap()
                .query_row(
                    "SELECT state FROM messages WHERE id = ?1 AND identity_id = ?2",
                    params![shared_id, identity],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(state_of("identity-a"), "delivered");
        assert_eq!(state_of("identity-b"), "sent", "B's row must not flip");
        assert_eq!(
            get_message_delivery_method(&pool, shared_id, "identity-a").as_deref(),
            Some("propagated")
        );
        assert_eq!(
            get_message_delivery_method(&pool, shared_id, "identity-b").as_deref(),
            Some("direct")
        );
        let method_b: Option<String> = pool
            .get()
            .unwrap()
            .query_row(
                "SELECT delivery_method FROM messages WHERE id = ?1 AND identity_id = 'identity-b'",
                params![shared_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(method_b.as_deref(), Some("direct"));

        assert!(
            !cancel_outbound_message_state(&pool, shared_id, "identity-a"),
            "A's row is terminal now"
        );
        assert!(cancel_outbound_message_state(
            &pool,
            shared_id,
            "identity-b"
        ));
        assert_eq!(state_of("identity-a"), "delivered");
        assert_eq!(state_of("identity-b"), "cancelled");
    }

    #[test]
    fn mark_message_resent_is_identity_scoped() {
        let pool = test_pool();
        let shared_id = "resent-duplicate-id";
        for identity in ["identity-a", "identity-b"] {
            save_message(
                &pool,
                shared_id,
                "me",
                "peer",
                "failed outbound",
                "",
                10.0,
                "failed",
                "outbound",
                identity,
                "",
                "",
                "",
                "",
                "",
                "",
                Some("direct"),
            );
        }

        mark_message_resent(&pool, shared_id, "identity-a");

        let state_of = |identity: &str| -> String {
            pool.get()
                .unwrap()
                .query_row(
                    "SELECT state FROM messages WHERE id = ?1 AND identity_id = ?2",
                    params![shared_id, identity],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(state_of("identity-a"), "resent");
        assert_eq!(state_of("identity-b"), "failed");
    }

    #[test]
    fn cancel_outbound_message_state_only_cancels_non_terminal_outbound_rows() {
        let pool = test_pool();
        save_message(
            &pool,
            "cancel-me",
            "me",
            "peer",
            "pending",
            "",
            10.0,
            "sent",
            "outbound",
            "identity-a",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("direct"),
        );
        save_message(
            &pool,
            "already-done",
            "me",
            "peer",
            "done",
            "",
            11.0,
            "delivered",
            "outbound",
            "identity-a",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("direct"),
        );
        save_message(
            &pool,
            "inbound-row",
            "peer",
            "me",
            "incoming",
            "",
            12.0,
            "received",
            "inbound",
            "identity-a",
            "",
            "",
            "",
            "",
            "",
            "",
            None,
        );

        assert!(cancel_outbound_message_state(
            &pool,
            "cancel-me",
            "identity-a"
        ));
        assert!(!cancel_outbound_message_state(
            &pool,
            "already-done",
            "identity-a"
        ));
        assert!(!cancel_outbound_message_state(
            &pool,
            "inbound-row",
            "identity-a"
        ));

        let conn = pool.get().unwrap();
        let cancel_state: String = conn
            .query_row(
                "SELECT state FROM messages WHERE id = 'cancel-me'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let done_state: String = conn
            .query_row(
                "SELECT state FROM messages WHERE id = 'already-done'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let inbound_state: String = conn
            .query_row(
                "SELECT state FROM messages WHERE id = 'inbound-row'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cancel_state, "cancelled");
        assert_eq!(done_state, "delivered");
        assert_eq!(inbound_state, "received");
    }

    #[test]
    fn observed_conversation_timestamp_appends_after_latest_message() {
        let pool = test_pool();
        save_message(
            &pool,
            "sent-first",
            "me",
            "echo",
            "ping",
            "",
            100.0,
            "sent",
            "outbound",
            "me",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("opportunistic"),
        );

        let observed = next_conversation_observed_timestamp(&pool, "echo", "me", 99.0);
        assert!(observed > 100.0);

        save_message(
            &pool,
            "reply-second",
            "echo",
            "me",
            "ping",
            "",
            observed,
            "received",
            "inbound",
            "me",
            "",
            "",
            "",
            "",
            "",
            "",
            None,
        );

        let messages = get_conversation(&pool, "echo", "me", 10);
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
            .collect();
        assert_eq!(ids, vec!["sent-first", "reply-second"]);
    }

    #[test]
    fn get_conversation_returns_latest_limited_messages_in_chronological_order() {
        let pool = test_pool();
        for i in 0..105 {
            let id = format!("msg-{i:03}");
            let content = format!("message {i}");
            save_message(
                &pool,
                &id,
                "me",
                "echo",
                &content,
                "",
                i as f64,
                "sent",
                "outbound",
                "me",
                "",
                "",
                "",
                "",
                "",
                "",
                Some("direct"),
            );
        }

        let messages = get_conversation(&pool, "echo", "me", 10);
        let ids: Vec<String> = messages
            .iter()
            .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
            .map(str::to_string)
            .collect();
        let expected: Vec<String> = (95..105).map(|i| format!("msg-{i:03}")).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn observed_conversation_timestamp_keeps_newer_observation() {
        let pool = test_pool();
        save_message(
            &pool,
            "old",
            "me",
            "peer",
            "old",
            "",
            100.0,
            "sent",
            "outbound",
            "me",
            "",
            "",
            "",
            "",
            "",
            "",
            Some("direct"),
        );

        let observed = next_conversation_observed_timestamp(&pool, "peer", "me", 101.0);
        assert!((observed - 101.0).abs() < f64::EPSILON);
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn empty_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        r2d2::Pool::builder().max_size(1).build(mgr).unwrap()
    }

    fn read_schema_version(pool: &DbPool) -> i64 {
        let conn = pool.get().unwrap();
        conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .unwrap()
    }

    #[test]
    fn test_fresh_db_initializes_at_current_schema_version() {
        let pool = empty_pool();
        init_schema(&pool).unwrap();
        assert_eq!(read_schema_version(&pool), SCHEMA_VERSION);

        let conn = pool.get().unwrap();
        for table in [
            "schema_version",
            "identities",
            "contacts",
            "messages",
            "connection_history",
            "messages_fts",
            "channel_hubs",
            "channel_rooms",
            "channel_room_secrets",
            "channel_hub_rooms",
            "channel_hub_grants",
            "channel_hub_klines",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists > 0, "expected table `{table}` after init_schema");
        }

        for index in [
            "idx_contacts_dest_identity",
            "idx_messages_identity_state",
            "idx_messages_source_identity",
            "idx_messages_dest_identity",
            "idx_channel_hubs_identity_recent",
            "idx_channel_rooms_identity_hub",
        ] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists > 0, "expected index `{index}` after init_schema");
        }

        let activity_cols = get_column_names(&conn, "identity_activity").unwrap();
        assert!(
            activity_cols
                .iter()
                .any(|c| c == "lxmf_compression_support"),
            "fresh schema should include LXMF compression capability metadata"
        );
    }

    #[test]
    fn test_init_schema_idempotent() {
        let pool = empty_pool();
        init_schema(&pool).unwrap();
        init_schema(&pool).unwrap();
        assert_eq!(read_schema_version(&pool), SCHEMA_VERSION);

        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO identities (hash, created_at) VALUES ('abc', 0.0)",
                [],
            )
            .unwrap();
        }
        init_schema(&pool).unwrap();
        let count: i64 = pool
            .get()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM identities", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "data survives repeat init_schema calls");
    }

    /// T1-8: every user-data table must be wiped by factory reset, and every
    /// identity-scoped table covered by the delete_identity cascade — new
    /// tables cannot silently drift out of either list.
    #[test]
    fn test_reset_and_cascade_cover_all_user_data_tables() {
        let pool = empty_pool();
        init_schema(&pool).unwrap();
        let conn = pool.get().unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                 AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        // schema_version survives reset by design; FTS shadow tables follow
        // `messages` via triggers plus the explicit rebuild after the wipe.
        for table in &tables {
            if table == "schema_version" || table.starts_with("messages_fts") {
                continue;
            }
            assert!(
                RESET_TABLES.contains(&table.as_str()),
                "table `{table}` is not wiped by factory reset — add it to RESET_TABLES or exempt it here"
            );
        }

        // identities itself is keyed by hash and deleted separately.
        for table in &tables {
            if table == "identities" || table.starts_with("messages_fts") {
                continue;
            }
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            if cols.iter().any(|c| c == "identity_id") {
                assert!(
                    IDENTITY_CASCADE.iter().any(|(label, _)| label == table),
                    "table `{table}` has identity_id but is missing from the delete_identity cascade"
                );
            }
        }
    }

    /// T1-8: blackhole requests queued for an identity do not survive its
    /// deletion.
    #[test]
    fn test_delete_identity_cascades_pending_blackholes() {
        let pool = empty_pool();
        init_schema(&pool).unwrap();
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO identities (hash, created_at) VALUES ('idA', 0.0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO pending_blackholes (dest_hash, identity_id, queued_at)
                 VALUES ('peer1', 'idA', 1.0), ('peer2', 'idB', 1.0)",
                [],
            )
            .unwrap();
        }
        delete_identity(&pool, "idA", true).unwrap();
        let conn = pool.get().unwrap();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_blackholes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(remaining, 1, "only the other identity's row survives");
        let who: String = conn
            .query_row("SELECT identity_id FROM pending_blackholes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(who, "idB");
    }

    /// T1-4: a crash between statements of one migration step must roll the
    /// whole step back (schema + version bump) and re-run cleanly.
    #[test]
    fn test_migration_step_interrupt_rolls_back_and_rerun_succeeds() {
        let pool = empty_pool();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (1);
             CREATE TABLE t (x INTEGER);
             INSERT INTO t VALUES (1);",
        )
        .unwrap();

        // Step applies one statement, then dies before finishing the batch.
        let result = migration_step(&conn, 2, |conn| {
            conn.execute_batch(
                "ALTER TABLE t RENAME TO t_old;
                 UPDATE schema_version SET version = 2;",
            )?;
            Err(rusqlite::Error::QueryReturnedNoRows)
        });
        assert!(result.is_err());

        // Both the rename and the version bump rolled back.
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 1, "version bump must roll back with the step");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1, "schema change must roll back with the step");

        // Re-running the same step (without the injected interrupt) succeeds.
        migration_step(&conn, 2, |conn| {
            conn.execute_batch(
                "ALTER TABLE t RENAME TO t_old;
                 UPDATE schema_version SET version = 2;",
            )
        })
        .unwrap();
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 2);
    }

    #[test]
    fn test_migration_from_v2_to_current_preserves_data() {
        let pool = empty_pool();

        // Minimal v2 schema: contacts/messages without identity_id, no FTS.
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (2);

                CREATE TABLE contacts (
                    dest_hash TEXT PRIMARY KEY,
                    display_name TEXT,
                    identity_pubkey TEXT,
                    first_seen REAL,
                    last_seen REAL,
                    trust TEXT DEFAULT 'pending',
                    notes TEXT DEFAULT ''
                );
                INSERT INTO contacts (dest_hash, display_name, first_seen, last_seen)
                VALUES ('deadbeef', 'Old Friend', 100.0, 200.0);

                CREATE TABLE messages (
                    id TEXT PRIMARY KEY,
                    source TEXT,
                    destination TEXT,
                    content TEXT,
                    title TEXT,
                    timestamp REAL,
                    state TEXT,
                    direction TEXT
                );
                INSERT INTO messages (id, source, destination, content, title, timestamp, state, direction)
                VALUES ('msg1', 'src', 'dst', 'hello from v2', '', 300.0, 'delivered', 'outbound');

                CREATE TABLE connection_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    host TEXT NOT NULL,
                    port INTEGER NOT NULL,
                    name TEXT DEFAULT '',
                    last_used REAL NOT NULL,
                    times_used INTEGER DEFAULT 1,
                    UNIQUE(host, port)
                );
                INSERT INTO connection_history (host, port, name, last_used)
                VALUES ('testhub', 4242, 'v2-hub', 400.0);
                "#,
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        assert_eq!(read_schema_version(&pool), SCHEMA_VERSION);

        let conn = pool.get().unwrap();

        let (dest, display, identity_id): (String, String, String) = conn
            .query_row(
                "SELECT dest_hash, display_name, identity_id FROM contacts WHERE dest_hash = 'deadbeef'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(dest, "deadbeef");
        assert_eq!(display, "Old Friend");
        assert_eq!(
            identity_id, "",
            "identity_id defaults to '' for legacy rows"
        );

        let (msg_id, msg_content, msg_identity, attachment_name): (String, String, String, String) =
            conn.query_row(
                "SELECT id, content, identity_id, attachment_name FROM messages WHERE id = 'msg1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(msg_id, "msg1");
        assert_eq!(msg_content, "hello from v2");
        assert_eq!(msg_identity, "");
        assert_eq!(attachment_name, "", "v4 attachment_name defaults to empty");

        let host: String = conn
            .query_row(
                "SELECT host FROM connection_history WHERE port = 4242",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(host, "testhub");

        for table in ["identities", "messages_fts"] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists > 0, "migration must create `{table}`");
        }
    }

    #[test]
    fn test_re_init_after_migration_is_noop() {
        let pool = empty_pool();
        init_schema(&pool).unwrap();
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO identities (hash, created_at) VALUES ('keep-me', 0.0)",
                [],
            )
            .unwrap();
        }
        init_schema(&pool).unwrap();

        let kept: i64 = pool
            .get()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM identities WHERE hash = 'keep-me'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn migration_from_v33_adds_channel_bookmark_tables() {
        let pool = empty_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (33);",
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        for table in ["channel_hubs", "channel_rooms"] {
            assert!(table_exists(&conn, table).unwrap());
        }
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migration_from_v34_adds_channel_hub_registry_tables() {
        let pool = empty_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (34);",
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        for table in [
            "channel_hub_rooms",
            "channel_hub_grants",
            "channel_hub_klines",
        ] {
            assert!(table_exists(&conn, table).unwrap());
        }
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migration_from_v35_adds_channel_desire_without_reclassifying_recents() {
        let pool = empty_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (35);
                 CREATE TABLE identities (
                    hash TEXT PRIMARY KEY,
                    created_at REAL NOT NULL,
                    is_active INTEGER DEFAULT 0
                 );
                 INSERT INTO identities (hash, created_at) VALUES ('identity-a', 0);
                 CREATE TABLE channel_hubs (
                    identity_id TEXT NOT NULL,
                    destination_hash TEXT NOT NULL,
                    label TEXT NOT NULL DEFAULT '',
                    nickname TEXT NOT NULL DEFAULT '',
                    added_at REAL NOT NULL,
                    last_connected REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, destination_hash),
                    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
                 );
                 CREATE TABLE channel_rooms (
                    identity_id TEXT NOT NULL,
                    hub_destination_hash TEXT NOT NULL,
                    room_name TEXT NOT NULL,
                    added_at REAL NOT NULL,
                    last_joined REAL NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
                    FOREIGN KEY (identity_id, hub_destination_hash)
                        REFERENCES channel_hubs(identity_id, destination_hash) ON DELETE CASCADE
                 );
                 INSERT INTO channel_hubs
                    (identity_id, destination_hash, label, nickname, added_at, last_connected)
                    VALUES ('identity-a', 'aa', 'Relay', 'rat', 1, 2);
                 INSERT INTO channel_rooms
                    (identity_id, hub_destination_hash, room_name, added_at, last_joined)
                    VALUES ('identity-a', 'aa', 'general', 1, 2);",
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let hubs = list_saved_channel_hubs(&pool, "identity-a").unwrap();
        let rooms = list_saved_channel_rooms(&pool, "identity-a", "aa").unwrap();
        assert_eq!(hubs.len(), 1);
        assert_eq!(rooms.len(), 1);
        assert!(
            !hubs[0].desired_connected && !rooms[0].desired_joined,
            "past recency is not proof of current user intent"
        );
        assert_eq!(read_schema_version(&pool), SCHEMA_VERSION);
    }

    #[test]
    fn migration_from_v36_adds_identity_sealed_room_key_storage() {
        let pool = empty_pool();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "PRAGMA foreign_keys=ON;
                 CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (36);
                 CREATE TABLE identities (
                    hash TEXT PRIMARY KEY,
                    created_at REAL NOT NULL,
                    is_active INTEGER DEFAULT 0
                 );
                 INSERT INTO identities (hash, created_at) VALUES ('identity-a', 0);
                 CREATE TABLE channel_hubs (
                    identity_id TEXT NOT NULL,
                    destination_hash TEXT NOT NULL,
                    label TEXT NOT NULL DEFAULT '',
                    nickname TEXT NOT NULL DEFAULT '',
                    added_at REAL NOT NULL,
                    last_connected REAL NOT NULL DEFAULT 0,
                    desired_connected INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, destination_hash),
                    FOREIGN KEY (identity_id) REFERENCES identities(hash) ON DELETE CASCADE
                 );
                 CREATE TABLE channel_rooms (
                    identity_id TEXT NOT NULL,
                    hub_destination_hash TEXT NOT NULL,
                    room_name TEXT NOT NULL,
                    added_at REAL NOT NULL,
                    last_joined REAL NOT NULL DEFAULT 0,
                    desired_joined INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, hub_destination_hash, room_name),
                    FOREIGN KEY (identity_id, hub_destination_hash)
                        REFERENCES channel_hubs(identity_id, destination_hash) ON DELETE CASCADE
                 );
                 INSERT INTO channel_hubs
                    (identity_id, destination_hash, added_at, desired_connected)
                    VALUES ('identity-a', 'aa', 1, 1);
                 INSERT INTO channel_rooms
                    (identity_id, hub_destination_hash, room_name, added_at,
                     desired_joined)
                    VALUES ('identity-a', 'aa', 'general', 1, 1);",
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        assert!(table_exists(&conn, "channel_room_secrets").unwrap());
        assert!(
            get_column_names(&conn, "channel_rooms")
                .unwrap()
                .iter()
                .any(|column| column == "join_key_required")
        );
        drop(conn);
        let room = &list_saved_channel_rooms(&pool, "identity-a", "aa").unwrap()[0];
        assert!(room.desired_joined);
        assert!(
            !room.join_key_required,
            "migration must not infer key policy from past membership"
        );
        assert_eq!(read_schema_version(&pool), SCHEMA_VERSION);
    }

    #[test]
    fn migrated_and_fresh_sealed_room_key_schemas_match() {
        let migrated = empty_pool();
        {
            let conn = migrated.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (36);
                 CREATE TABLE channel_rooms (
                    identity_id TEXT NOT NULL,
                    hub_destination_hash TEXT NOT NULL,
                    room_name TEXT NOT NULL,
                    added_at REAL NOT NULL,
                    last_joined REAL NOT NULL DEFAULT 0,
                    desired_joined INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (identity_id, hub_destination_hash, room_name)
                 );",
            )
            .unwrap();
        }
        init_schema(&migrated).unwrap();
        let fresh = empty_pool();
        init_schema(&fresh).unwrap();

        for table in ["channel_rooms", "channel_room_secrets"] {
            let columns = |pool: &DbPool| {
                let conn = pool.get().unwrap();
                get_column_names(&conn, table).unwrap()
            };
            assert_eq!(
                columns(&migrated),
                columns(&fresh),
                "migrated and fresh `{table}` columns diverged"
            );
        }
    }

    /// The migrated schema and the fresh schema must agree; the DDL is
    /// duplicated between them by house convention, so drift is easy.
    #[test]
    fn migrated_and_fresh_hub_registry_schemas_match() {
        let migrated = empty_pool();
        {
            let conn = migrated.get().unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version (version) VALUES (34);",
            )
            .unwrap();
        }
        init_schema(&migrated).unwrap();
        let fresh = empty_pool();
        init_schema(&fresh).unwrap();

        for table in [
            "channel_hub_rooms",
            "channel_hub_grants",
            "channel_hub_klines",
        ] {
            let columns = |pool: &DbPool| -> Vec<String> {
                let conn = pool.get().unwrap();
                get_column_names(&conn, table).unwrap()
            };
            assert_eq!(
                columns(&migrated),
                columns(&fresh),
                "{table} drifted between the migration and the fresh schema"
            );
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    #[test]
    fn set_active_identity_rejects_missing_without_clearing_current() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        set_active_identity(&pool, "identity-a").unwrap();

        let err = set_active_identity(&pool, "missing").unwrap_err();
        assert!(err.contains("identity not found"));

        let active = get_active_identity(&pool).unwrap();
        assert_eq!(
            active.get("hash").and_then(|v| v.as_str()),
            Some("identity-a")
        );
    }

    #[test]
    fn get_identity_returns_requested_row_only() {
        let pool = test_pool();
        save_identity(&pool, "identity-a", "lxmf-a", "A", "A");
        save_identity(&pool, "identity-b", "lxmf-b", "B", "B");

        let found = get_identity(&pool, "identity-b").unwrap();
        assert_eq!(
            found.get("hash").and_then(|v| v.as_str()),
            Some("identity-b")
        );
        assert!(get_identity(&pool, "missing").is_none());
    }
}

#[cfg(test)]
mod peers_snapshot_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    fn touch(pool: &DbPool, hash: &str, ts: f64) {
        touch_identity_activity(pool, &[(hash.to_string(), ts, None, None)]);
    }

    fn set_announce_name(pool: &DbPool, hash: &str, name: &str) {
        let conn = pool.get().unwrap();
        conn.execute(
            "UPDATE identity_activity SET display_name = ?1 WHERE dest_hash = ?2",
            params![name, hash],
        )
        .unwrap();
    }

    fn services_for(pool: &DbPool, hash: &str) -> String {
        let conn = pool.get().unwrap();
        conn.query_row(
            "SELECT services FROM identity_activity WHERE dest_hash = ?1",
            params![hash],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    fn announce_count_for(pool: &DbPool, hash: &str) -> i64 {
        let conn = pool.get().unwrap();
        conn.query_row(
            "SELECT announce_count FROM identity_activity WHERE dest_hash = ?1",
            params![hash],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    }

    fn identity_hash_for(pool: &DbPool, hash: &str) -> String {
        let conn = pool.get().unwrap();
        conn.query_row(
            "SELECT identity_hash FROM identity_activity WHERE dest_hash = ?1",
            params![hash],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    fn lxmf_compression_support_for(pool: &DbPool, hash: &str) -> String {
        let conn = pool.get().unwrap();
        conn.query_row(
            "SELECT lxmf_compression_support FROM identity_activity WHERE dest_hash = ?1",
            params![hash],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
    }

    #[test]
    fn touch_identity_activity_merges_multiple_services_once_and_clears_ratspeak() {
        let pool = test_pool();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let rows = vec![(hash.to_string(), 100.0, None, None)];

        touch_identity_activity_for_services(
            &pool,
            &rows,
            None,
            &[
                PEER_SERVICE_LXMF_DELIVERY,
                PEER_SERVICE_RATSPEAK_CLIENT,
                PEER_SERVICE_RATSPEAK_GAMES,
            ],
            true,
        );
        assert_eq!(announce_count_for(&pool, hash), 1);
        assert_eq!(
            services_for(&pool, hash),
            "lxmf.delivery,ratspeak.client,ratspeak.games"
        );

        touch_identity_activity_for_services(
            &pool,
            &rows,
            None,
            &[PEER_SERVICE_LXMF_DELIVERY],
            true,
        );
        assert_eq!(announce_count_for(&pool, hash), 2);
        assert_eq!(services_for(&pool, hash), "lxmf.delivery");
    }

    #[test]
    fn touch_identity_activity_updates_keeps_per_row_identity_and_services() {
        let pool = test_pool();
        touch_identity_activity_updates(
            &pool,
            &[
                IdentityActivityUpdate {
                    dest_hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    timestamp: 100.0,
                    display_name: Some("Alice".into()),
                    status: Some("Around".into()),
                    last_interface: None,
                    identity_hash: Some("11111111111111111111111111111111".into()),
                    services: vec![PEER_SERVICE_LXMF_DELIVERY.into()],
                    clear_ratspeak_services: true,
                    lxmf_compression_support: None,
                },
                IdentityActivityUpdate {
                    dest_hash: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    timestamp: 200.0,
                    display_name: None,
                    status: None,
                    last_interface: None,
                    identity_hash: Some("22222222222222222222222222222222".into()),
                    services: vec![PEER_SERVICE_LXST_TELEPHONY.into()],
                    clear_ratspeak_services: false,
                    lxmf_compression_support: None,
                },
            ],
        );

        assert_eq!(
            identity_hash_for(&pool, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "11111111111111111111111111111111"
        );
        assert_eq!(
            identity_hash_for(&pool, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            "22222222222222222222222222222222"
        );
        assert_eq!(
            services_for(&pool, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
            PEER_SERVICE_LXST_TELEPHONY
        );
    }

    #[test]
    fn touch_identity_activity_updates_merges_lxmf_compression_support() {
        let pool = test_pool();
        let hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

        touch_identity_activity_updates(
            &pool,
            &[IdentityActivityUpdate {
                dest_hash: hash.into(),
                timestamp: 100.0,
                display_name: Some("Alice".into()),
                status: None,
                last_interface: None,
                identity_hash: None,
                services: vec![PEER_SERVICE_LXMF_DELIVERY.into()],
                clear_ratspeak_services: false,
                lxmf_compression_support: Some(LXMF_COMPRESSION_SUPPORT_UNSUPPORTED.into()),
            }],
        );
        assert_eq!(
            get_identity_lxmf_compression_support(&pool, hash).as_deref(),
            Some(LXMF_COMPRESSION_SUPPORT_UNSUPPORTED)
        );

        touch_identity_activity_updates(
            &pool,
            &[IdentityActivityUpdate {
                dest_hash: hash.into(),
                timestamp: 101.0,
                display_name: None,
                status: None,
                last_interface: None,
                identity_hash: None,
                services: vec![PEER_SERVICE_LXMF_DELIVERY.into()],
                clear_ratspeak_services: false,
                lxmf_compression_support: None,
            }],
        );
        assert_eq!(
            lxmf_compression_support_for(&pool, hash),
            LXMF_COMPRESSION_SUPPORT_UNSUPPORTED
        );

        assert!(set_identity_lxmf_compression_support(
            &pool,
            hash,
            LXMF_COMPRESSION_SUPPORT_SUPPORTED
        ));
        assert_eq!(
            get_identity_lxmf_compression_support(&pool, hash).as_deref(),
            Some(LXMF_COMPRESSION_SUPPORT_SUPPORTED)
        );
        assert!(!set_identity_lxmf_compression_support(
            &pool, hash, "unknown"
        ));
        assert_eq!(
            lxmf_compression_support_for(&pool, hash),
            LXMF_COMPRESSION_SUPPORT_SUPPORTED
        );
    }

    fn add_contact(pool: &DbPool, hash: &str, display_name: &str) {
        add_contact_for(pool, "me", hash, display_name);
    }

    fn add_contact_for(pool: &DbPool, identity_id: &str, hash: &str, display_name: &str) {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO contacts (dest_hash, identity_id, display_name, first_seen, last_seen)
             VALUES (?1, ?2, ?3, 0, 0)",
            params![hash, identity_id, display_name],
        )
        .unwrap();
    }

    fn block(pool: &DbPool, hash: &str) {
        let conn = pool.get().unwrap();
        conn.execute(
            "INSERT INTO blocked_contacts (dest_hash, identity_id, blocked_at)
             VALUES (?1, 'me', 0)",
            params![hash],
        )
        .unwrap();
    }

    fn activity_count(pool: &DbPool, hash: &str) -> i64 {
        let conn = pool.get().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM identity_activity WHERE dest_hash = ?1",
            params![hash],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    }

    #[test]
    fn clear_discovered_identity_activity_preserves_user_owned_rows() {
        let pool = test_pool();
        for hash in [
            "drop-me",
            "contact-peer",
            "blocked-peer",
            "message-source",
            "message-dest",
            "prop-node",
        ] {
            touch(&pool, hash, 100.0);
        }
        add_contact(&pool, "contact-peer", "Contact");
        block(&pool, "blocked-peer");
        {
            let conn = pool.get().unwrap();
            conn.execute(
                "INSERT INTO messages (id, source, destination, timestamp, state, direction, identity_id)
                 VALUES ('msg-clear-cache', 'message-source', 'message-dest', 100.0, 'delivered', 'inbound', 'me')",
                [],
            )
            .unwrap();
        }
        save_identity(&pool, "identity", "lxmf", "Me", "Me");
        set_identity_propagation_node(&pool, "identity", "prop-node").unwrap();

        assert_eq!(clear_discovered_identity_activity(&pool), 1);
        assert_eq!(activity_count(&pool, "drop-me"), 0);
        for hash in [
            "contact-peer",
            "blocked-peer",
            "message-source",
            "message-dest",
            "prop-node",
        ] {
            assert_eq!(activity_count(&pool, hash), 1, "{hash} should be preserved");
        }
    }

    #[test]
    fn snapshot_returns_recent_non_contacts_with_announce_name() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        set_announce_name(&pool, "alice", "Alice");
        touch(&pool, "bob", 200.0);
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 2);
        let alice = rows.iter().find(|r| r.hash == "alice").unwrap();
        let bob = rows.iter().find(|r| r.hash == "bob").unwrap();
        assert_eq!(alice.display_name, "Alice");
        assert_eq!(alice.last_seen, Some(100.0));
        assert!(!alice.is_contact);
        assert_eq!(alice.services, vec![PEER_SERVICE_LXMF_DELIVERY]);
        assert_eq!(bob.display_name, "");
        assert!(!bob.is_contact);
    }

    #[test]
    fn snapshot_excludes_non_actionable_service_announces() {
        let pool = test_pool();
        let rows = vec![("node".to_string(), 100.0, Some("Node".to_string()), None)];
        touch_identity_activity_for_service(&pool, &rows, None, "nomadnetwork.node");

        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn snapshot_includes_lxst_telephony_peers() {
        let pool = test_pool();
        let rows = vec![("voice-peer".to_string(), 100.0, None, None)];
        touch_identity_activity_for_service(
            &pool,
            &rows,
            Some("identity-peer"),
            PEER_SERVICE_LXST_TELEPHONY,
        );

        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "voice-peer");
        assert_eq!(rows[0].identity_hash, "identity-peer");
        assert_eq!(rows[0].services, vec![PEER_SERVICE_LXST_TELEPHONY]);
    }

    #[test]
    fn snapshot_filters_by_cutoff() {
        let pool = test_pool();
        touch(&pool, "old", 100.0);
        touch(&pool, "fresh", 300.0);
        let rows = get_peers_snapshot(&pool, 200.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "fresh");
    }

    #[test]
    fn snapshot_includes_never_seen_contacts() {
        let pool = test_pool();
        add_contact(&pool, "stranger", "Stranger");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "stranger");
        assert_eq!(rows[0].display_name, "Stranger");
        assert!(rows[0].is_contact);
        assert!(rows[0].last_seen.is_none());
    }

    #[test]
    fn snapshot_contact_name_overrides_announce_name() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        set_announce_name(&pool, "alice", "alice-from-announce");
        add_contact(&pool, "alice", "Alice The Friend");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "alice");
        assert_eq!(rows[0].display_name, "Alice The Friend");
        assert!(rows[0].is_contact);
        assert_eq!(rows[0].last_seen, Some(100.0));
    }

    #[test]
    fn snapshot_falls_back_to_announce_name_when_contact_name_empty() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        set_announce_name(&pool, "alice", "Alice The Mesh");
        add_contact(&pool, "alice", "");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "Alice The Mesh");
        assert!(rows[0].is_contact);
    }

    #[test]
    fn snapshot_excludes_blocked_peers_even_if_seen_recently() {
        let pool = test_pool();
        touch(&pool, "spammer", 100.0);
        block(&pool, "spammer");
        touch(&pool, "alice", 100.0);
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "alice");
    }

    #[test]
    fn snapshot_excludes_blocked_contacts_too() {
        let pool = test_pool();
        add_contact(&pool, "ex", "Ex Friend");
        block(&pool, "ex");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 0);
    }

    #[test]
    fn snapshot_uses_announce_name_for_non_contacts() {
        let pool = test_pool();
        touch(&pool, "stranger", 100.0);
        set_announce_name(&pool, "stranger", "Stranger Joe");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "Stranger Joe");
        assert!(!rows[0].is_contact);
    }

    #[test]
    fn snapshot_keeps_old_activity_for_contacts() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        add_contact(&pool, "alice", "Alice");
        let rows = get_peers_snapshot(&pool, 200.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "alice");
        assert_eq!(rows[0].last_seen, Some(100.0));
        assert!(rows[0].is_contact);
    }

    #[test]
    fn snapshot_scopes_contacts_to_identity() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        add_contact_for(&pool, "other", "alice", "Other Alice");
        let rows = get_peers_snapshot(&pool, 0.0, "me");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hash, "alice");
        assert!(!rows[0].is_contact);
        assert_ne!(rows[0].display_name, "Other Alice");
    }

    #[test]
    fn peer_by_hashes_scopes_contact_state_to_identity() {
        let pool = test_pool();
        touch(&pool, "alice", 100.0);
        add_contact_for(&pool, "other", "alice", "Other Alice");
        let rows = get_peers_by_hashes(&pool, &["alice".to_string()], "me");
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].is_contact);
        assert_ne!(rows[0].display_name, "Other Alice");
    }

    #[test]
    fn touch_identity_last_heard_does_not_increment_announce_count() {
        let pool = test_pool();
        assert!(touch_identity_last_heard(&pool, "alice", 100.0));
        assert!(touch_identity_last_heard(&pool, "alice", 200.0));
        let conn = pool.get().unwrap();
        let (last_seen, announce_count): (f64, i64) = conn
            .query_row(
                "SELECT last_seen, announce_count FROM identity_activity WHERE dest_hash = 'alice'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(last_seen, 200.0);
        assert_eq!(announce_count, 0);
    }

    #[test]
    fn identity_activity_first_seen_lookup_preserves_original_timestamp() {
        let pool = test_pool();
        assert_eq!(get_identity_activity_first_seen(&pool, "alice"), None);
        assert!(touch_identity_last_heard(&pool, "alice", 100.0));
        assert!(touch_identity_last_heard(&pool, "alice", 200.0));
        assert_eq!(
            get_identity_activity_first_seen(&pool, "alice"),
            Some(100.0)
        );
    }
}

#[cfg(test)]
mod pending_blackhole_tests {
    use super::*;
    use r2d2_sqlite::SqliteConnectionManager;

    fn test_pool() -> DbPool {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        init_schema(&pool).unwrap();
        pool
    }

    #[test]
    fn enqueue_then_list_then_clear_round_trip() {
        let pool = test_pool();
        assert!(enqueue_pending_blackhole(
            &pool,
            "deadbeef",
            "me",
            Some("test"),
            Some(3600.0)
        ));
        let by_dest = list_pending_blackholes_by_dest(&pool, "deadbeef");
        assert_eq!(by_dest.len(), 1);
        assert_eq!(by_dest[0].identity_id, "me");
        assert_eq!(by_dest[0].reason_label.as_deref(), Some("test"));
        assert_eq!(by_dest[0].ttl_seconds, Some(3600.0));

        let by_id = list_pending_blackholes_for_identity(&pool, "me");
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].dest_hash, "deadbeef");

        assert!(clear_pending_blackhole(&pool, "deadbeef", "me"));
        assert!(list_pending_blackholes_by_dest(&pool, "deadbeef").is_empty());
        // Idempotent: second clear returns false.
        assert!(!clear_pending_blackhole(&pool, "deadbeef", "me"));
    }

    #[test]
    fn enqueue_replaces_existing_row_for_same_key() {
        let pool = test_pool();
        assert!(enqueue_pending_blackhole(&pool, "abc", "me", None, None));
        assert!(enqueue_pending_blackhole(
            &pool,
            "abc",
            "me",
            Some("rate_limit"),
            Some(60.0)
        ));
        let rows = list_pending_blackholes_by_dest(&pool, "abc");
        assert_eq!(
            rows.len(),
            1,
            "key (dest, identity) is primary so REPLACE collapses"
        );
        assert_eq!(rows[0].reason_label.as_deref(), Some("rate_limit"));
        assert_eq!(rows[0].ttl_seconds, Some(60.0));
    }

    #[test]
    fn list_by_dest_returns_all_local_identities() {
        let pool = test_pool();
        assert!(enqueue_pending_blackhole(
            &pool, "shared", "alice", None, None
        ));
        assert!(enqueue_pending_blackhole(
            &pool, "shared", "bob", None, None
        ));
        let rows = list_pending_blackholes_by_dest(&pool, "shared");
        assert_eq!(rows.len(), 2);
        let ids: std::collections::HashSet<_> =
            rows.iter().map(|r| r.identity_id.clone()).collect();
        assert!(ids.contains("alice"));
        assert!(ids.contains("bob"));
    }

    #[test]
    fn identity_activity_resolves_dest_to_identity_for_blackhole_fallbacks() {
        let pool = test_pool();
        let dest_a = "11111111111111111111111111111111".to_string();
        let dest_b = "22222222222222222222222222222222".to_string();
        let identity_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let identity_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let rows = [
            (dest_a.clone(), 1.0, None, None),
            (dest_b.clone(), 2.0, None, None),
        ];

        assert_eq!(
            touch_identity_activity_for_service(
                &pool,
                &rows[..1],
                Some(identity_a),
                "lxmf.delivery"
            ),
            1
        );
        assert_eq!(
            touch_identity_activity_for_service(
                &pool,
                &rows[1..],
                Some(identity_b),
                "lxst.telephony"
            ),
            1
        );

        assert_eq!(
            identity_hash_for_dest(&pool, &dest_a).as_deref(),
            Some(identity_a)
        );
        let found = identity_hashes_for_dests(&pool, &[dest_a.clone(), dest_b.clone()]);
        assert_eq!(found.get(&dest_a).map(String::as_str), Some(identity_a));
        assert_eq!(found.get(&dest_b).map(String::as_str), Some(identity_b));
        assert!(identity_hash_for_dest(&pool, "33333333333333333333333333333333").is_none());
    }

    #[test]
    fn migration_from_v27_creates_pending_blackholes_table() {
        // Build a pre-fix DB at version 27, then run init_schema and confirm
        // the migration runs and the table is queryable.
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        let conn = pool.get().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (27);",
        )
        .unwrap();
        drop(conn);

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        let v: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        // Table is queryable.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM pending_blackholes", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migration_from_v31_repairs_missing_identity_status_columns() {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (31);

                CREATE TABLE identities (
                    hash TEXT PRIMARY KEY,
                    lxmf_hash TEXT,
                    nickname TEXT DEFAULT '',
                    display_name TEXT DEFAULT '',
                    created_at REAL NOT NULL,
                    last_used REAL,
                    is_active INTEGER DEFAULT 0,
                    propagation_node TEXT DEFAULT '',
                    propagation_enabled INTEGER DEFAULT 0,
                    propagation_mode TEXT NOT NULL DEFAULT 'auto',
                    propagation_auto_favor_static INTEGER NOT NULL DEFAULT 1
                );
                INSERT INTO identities
                    (hash, lxmf_hash, nickname, display_name, created_at, last_used, is_active)
                VALUES
                    ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     'Default',
                     'Default',
                     1.0,
                     2.0,
                     1);

                CREATE TABLE identity_activity (
                    dest_hash TEXT PRIMARY KEY,
                    identity_hash TEXT NOT NULL DEFAULT '',
                    last_seen REAL NOT NULL,
                    first_seen REAL NOT NULL,
                    announce_count INTEGER NOT NULL DEFAULT 1,
                    display_name TEXT NOT NULL DEFAULT '',
                    last_interface TEXT NOT NULL DEFAULT '',
                    services TEXT NOT NULL DEFAULT ''
                );
                "#,
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        let identity_cols = get_column_names(&conn, "identities").unwrap();
        assert!(identity_cols.iter().any(|c| c == "status"));
        let activity_cols = get_column_names(&conn, "identity_activity").unwrap();
        assert!(activity_cols.iter().any(|c| c == "status"));
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(conn);

        let active = get_active_identity(&pool).expect("active identity remains readable");
        assert_eq!(
            active.get("hash").and_then(|v| v.as_str()),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(active.get("status").and_then(|v| v.as_str()), Some(""));
    }

    #[test]
    fn migration_from_v32_adds_lxmf_compression_support_column() {
        let mgr = SqliteConnectionManager::memory();
        let pool = r2d2::Pool::builder().max_size(1).build(mgr).unwrap();
        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (32);

                CREATE TABLE identity_activity (
                    dest_hash TEXT PRIMARY KEY,
                    identity_hash TEXT NOT NULL DEFAULT '',
                    last_seen REAL NOT NULL,
                    first_seen REAL NOT NULL,
                    announce_count INTEGER NOT NULL DEFAULT 1,
                    display_name TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT '',
                    last_interface TEXT NOT NULL DEFAULT '',
                    services TEXT NOT NULL DEFAULT ''
                );
                INSERT INTO identity_activity
                    (dest_hash, identity_hash, last_seen, first_seen, announce_count, display_name, status, last_interface, services)
                VALUES
                    ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                     10.0,
                     5.0,
                     3,
                     'Peer',
                     'Ready',
                     'RNode',
                     'lxmf.delivery');
                "#,
            )
            .unwrap();
        }

        init_schema(&pool).unwrap();

        let conn = pool.get().unwrap();
        let activity_cols = get_column_names(&conn, "identity_activity").unwrap();
        assert!(
            activity_cols
                .iter()
                .any(|c| c == "lxmf_compression_support")
        );
        let row: (String, String, String, String) = conn
            .query_row(
                "SELECT display_name, status, services, lxmf_compression_support
                 FROM identity_activity
                 WHERE dest_hash = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "Peer".into(),
                "Ready".into(),
                "lxmf.delivery".into(),
                "".into()
            )
        );
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
