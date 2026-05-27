// Hint cargo to rerun when frontend assets change so Tauri's bundler picks up
// fresh JS/CSS without a manual `cargo clean`.
fn main() {
    println!("cargo::rerun-if-changed=../../dashboard/static/");
    println!("cargo::rerun-if-changed=../../dashboard/index.html");
    println!("cargo::rerun-if-changed=../../VERSION");
    println!("cargo::rerun-if-env-changed=RATSPEAK_DISPLAY_VERSION");
    println!("cargo::rerun-if-env-changed=GITHUB_REF_NAME");

    if let Some(version) = display_version() {
        println!("cargo::rustc-env=RATSPEAK_DISPLAY_VERSION={version}");
    }
}

fn display_version() -> Option<String> {
    std::env::var("RATSPEAK_DISPLAY_VERSION")
        .ok()
        .or_else(|| std::env::var("GITHUB_REF_NAME").ok())
        .or_else(|| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../VERSION"),
            )
            .ok()
        })
        .and_then(|value| normalize_display_version(&value))
}

fn normalize_display_version(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let version = trimmed.strip_prefix('v').unwrap_or(trimmed).trim();
    version
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit())
        .then(|| version.to_string())
}
