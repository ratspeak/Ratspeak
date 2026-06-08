use serde_json::Value;

use crate::error::CliResult;

#[derive(Debug, Clone, Copy, Default)]
pub struct OutputFormat {
    pub pretty: bool,
}

pub fn print_json(value: &Value, format: OutputFormat) -> CliResult<()> {
    if format.pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}
