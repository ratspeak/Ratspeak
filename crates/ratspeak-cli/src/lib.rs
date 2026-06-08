//! Headless Ratspeak command-line frontends.
//!
//! This crate intentionally depends on `ratspeak-runtime`, not
//! `ratspeak-tauri`, so CLI and daemon work can grow without pulling in a UI
//! host.

pub mod commands;
mod daemon_api;
mod error;
mod output;
mod profile;
mod runtime_host;

pub use error::{CliError, CliResult};
