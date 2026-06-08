#[tokio::main]
async fn main() {
    if let Err(err) = ratspeak_cli::commands::run_daemon(std::env::args().skip(1).collect()).await {
        eprintln!("ratspeakd: {err}");
        std::process::exit(err.exit_code());
    }
}
