#[tokio::main]
async fn main() {
    if let Err(err) = ratspeak_cli::commands::run_ctl(std::env::args().skip(1).collect()).await {
        eprintln!(
            "{}",
            serde_json::to_string(&err.json_envelope()).unwrap_or_else(|_| {
                format!(
                    r#"{{"ok":false,"error":{{"code":"failed","message":"{err}"}},"exit_code":{}}}"#,
                    err.exit_code()
                )
            })
        );
        std::process::exit(err.exit_code());
    }
}
