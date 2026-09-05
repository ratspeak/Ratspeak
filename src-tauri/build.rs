fn main() {
    configure_android_page_size();
    build_dashboard_css();
    tauri_build::build()
}

fn configure_android_page_size() {
    // The build script runs on the host, so cfg!(target_os) would select the
    // wrong platform during cross-compilation. Use Cargo's target metadata.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let pointer_width = std::env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap_or_default();
    for argument in android_page_size_link_args(&target_os, &pointer_width) {
        println!("cargo:rustc-link-arg={argument}");
    }
}

fn android_page_size_link_args(target_os: &str, pointer_width: &str) -> &'static [&'static str] {
    if target_os == "android" && pointer_width == "64" {
        // NDK r27 requires both options for 16 KiB PT_LOAD alignment and safe
        // RELRO boundaries. Keep the reviewed NDK pin; do not depend on a newer
        // SDK installation silently choosing different linker defaults.
        // https://developer.android.com/guide/practices/page-sizes
        &[
            "-Wl,-z,max-page-size=16384",
            "-Wl,-z,common-page-size=16384",
        ]
    } else {
        &[]
    }
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
        "00-palettes.css",
        "01-reset.css",
        "02-typography.css",
        "03-scrollbar.css",
        "04-layout.css",
        "05-panels.css",
        "06-forms.css",
        "07-components.css",
        "08-modals.css",
        "09-messaging.css",
        "09-channels.css",
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
    println!(
        "cargo:rerun-if-changed={}",
        dashboard_dir.join("index.html").display()
    );
    if fs::read_to_string(&out)
        .map(|existing| existing != bundle)
        .unwrap_or(true)
    {
        fs::write(&out, bundle)
            .unwrap_or_else(|err| panic!("failed to write {}: {}", out.display(), err));
    }
}

#[cfg(test)]
mod tests {
    use super::android_page_size_link_args;

    #[test]
    fn android_64_bit_targets_get_both_page_size_linker_options() {
        assert_eq!(
            android_page_size_link_args("android", "64"),
            &[
                "-Wl,-z,max-page-size=16384",
                "-Wl,-z,common-page-size=16384"
            ]
        );
    }

    #[test]
    fn android_32_bit_targets_are_unchanged() {
        assert!(android_page_size_link_args("android", "32").is_empty());
    }

    #[test]
    fn desktop_and_ios_linkers_are_unchanged() {
        for target in ["linux", "macos", "ios", "windows"] {
            assert!(android_page_size_link_args(target, "64").is_empty());
        }
    }

    #[test]
    fn missing_or_unknown_target_metadata_does_not_emit_android_flags() {
        for (target, width) in [("", "64"), ("android", ""), ("android", "128")] {
            assert!(android_page_size_link_args(target, width).is_empty());
        }
    }
}
