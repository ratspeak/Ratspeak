//! Isolation invariant: two distinct CLI profiles must never share on-disk
//! state or a Reticulum network identity, so a bot can never co-own another
//! bot's (or the desktop app's) database, config, socket, or shared instance.

use ratspeak_cli::profile;
use ratspeak_runtime::config::DashboardConfig;

fn headless_config(root: &str) -> DashboardConfig {
    DashboardConfig::from_env_and_defaults(std::path::PathBuf::from(root))
        .with_headless_rns_policy(true, None)
}

#[test]
fn two_profiles_are_fully_disjoint() {
    let a = headless_config("/tmp/ratspeak-isolation-a");
    let b = headless_config("/tmp/ratspeak-isolation-b");

    // On-disk state.
    assert_ne!(a.db_path(), b.db_path());
    assert_ne!(a.rns_config_dir, b.rns_config_dir);
    assert_ne!(a.data_dir, b.data_dir);
    // Daemon API endpoint manifest lives under each profile's data_dir.
    assert_ne!(
        a.data_dir.join("ratspeakd-api.json"),
        b.data_dir.join("ratspeakd-api.json")
    );

    // Reticulum network identity (the part --data-dir alone did NOT isolate).
    let ia = a.rns_instance_identity();
    let ib = b.rns_instance_identity();
    assert_ne!(ia.instance_name, ib.instance_name);
    assert_ne!(ia.shared_instance_port, ib.shared_instance_port);
    assert_ne!(ia.instance_control_port, ib.instance_control_port);
}

#[test]
fn derived_identity_never_collides_with_python_default() {
    let cfg = headless_config("/tmp/ratspeak-isolation-c");
    let identity = cfg.rns_instance_identity();
    assert!(identity.instance_name.starts_with("rsk-"));
    assert_ne!(identity.instance_name, "default");
}

#[test]
fn headless_defaults_to_standalone() {
    // No opt-in → Standalone (share_instance = false), so no rendezvous probe.
    let cfg = DashboardConfig::from_env_and_defaults(std::path::PathBuf::from(
        "/tmp/ratspeak-isolation-standalone",
    ))
    .with_headless_rns_policy(false, None);
    assert!(!cfg.rns_instance_identity().share_instance);
}

#[test]
fn cli_default_root_is_never_the_desktop_app() {
    // The desktop app's profile must not be reachable as a CLI default.
    assert!(!profile::is_desktop_app_root(&profile::desktop_app_data_root().join("nope")));
    assert!(profile::is_desktop_app_root(&profile::desktop_app_data_root()));
}
