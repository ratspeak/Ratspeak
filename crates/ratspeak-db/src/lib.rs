//! Ratspeak SQLite layer. Owns the schema, migrations, connection pool, and
//! per-domain queries. Depends on `ratspeak-core` for shared types only.

pub mod admission;
pub mod db;
pub mod static_nodes;
pub mod write_error;

pub use db::*;
pub use write_error::{StorageWriteError, report_write_error, set_storage_write_error_hook};
