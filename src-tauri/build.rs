fn main() {
    build_dashboard_css();
    tauri_build::build()
}

fn build_dashboard_css() {
    use std::{env, fs, path::PathBuf};

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dashboard_dir = manifest_dir
        .parent()
        .expect("src-tauri parent")
        .join("dashboard");
    let css_dir = dashboard_dir.join("static/css");
    let out = dashboard_dir.join("static/style.css");
    let modules = [
        "00-tokens.css",
        "01-reset.css",
        "02-typography.css",
        "03-scrollbar.css",
        "04-layout.css",
        "05-panels.css",
        "06-forms.css",
        "07-components.css",
        "08-modals.css",
        "09-messaging.css",
        "10-views.css",
        "11-games.css",
        "12-animations.css",
        "13-responsive.css",
    ];

    let mut bundle = String::new();
    for module in modules {
        let path = css_dir.join(module);
        println!("cargo:rerun-if-changed={}", path.display());
        let css = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {}", path.display(), err));
        bundle.push_str(&css);
        bundle.push('\n');
    }
    println!("cargo:rerun-if-changed={}", dashboard_dir.join("index.html").display());
    fs::write(&out, bundle)
        .unwrap_or_else(|err| panic!("failed to write {}: {}", out.display(), err));
}
