//! Error type for Tauri commands. `code` is snake_case and stable; `message`
//! is human-readable and may change; optional `details` carries stable,
//! code-specific data.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl AppError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new("unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new("forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new("conflict", message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new("service_unavailable", message)
    }

    pub fn database_unavailable(message: impl Into<String>) -> Self {
        Self::new("database_unavailable", message)
    }

    pub fn lxmf_not_initialized(message: impl Into<String>) -> Self {
        Self::new("lxmf_not_initialized", message)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        Self::new("database_error", err.to_string())
    }
}

impl From<r2d2::Error> for AppError {
    fn from(err: r2d2::Error) -> Self {
        Self::database_unavailable(err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        Self::new("serialization_error", err.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
