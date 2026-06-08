use std::error::Error;
use std::fmt::{Display, Formatter};

pub type CliResult<T> = Result<T, CliError>;

#[derive(Debug)]
pub struct CliError {
    message: String,
    exit_code: i32,
}

impl CliError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: 2,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: 1,
        }
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }
}

impl Display for CliError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(value: std::io::Error) -> Self {
        Self::failed(value.to_string())
    }
}

impl From<serde_json::Error> for CliError {
    fn from(value: serde_json::Error) -> Self {
        Self::failed(value.to_string())
    }
}
