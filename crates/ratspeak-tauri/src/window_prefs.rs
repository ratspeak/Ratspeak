//! Desktop window decoration preference resolution.
//!
//! GTK3 on Wayland always installs a client-side header bar (tao's `WlHeader`),
//! which tiling compositors render as a second title bar below their own.
//! `auto` drops decorations when a tiling Wayland compositor is detected;
//! `on`/`off` force either way. X11 is untouched: tao draws no GTK titlebar
//! there, so the WM decorates per its own policy.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecorationsPref {
    Auto,
    On,
    Off,
}

impl DecorationsPref {
    pub fn from_setting(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("on") => Self::On,
            Some(v) if v.eq_ignore_ascii_case("off") => Self::Off,
            _ => Self::Auto,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

// Socket vars are the primary signal (exported by the compositor itself);
// desktop-name matching covers compositors without one (river, dwl).
const TILING_SOCKET_VARS: [&str; 4] = [
    "SWAYSOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "NIRI_SOCKET",
    "WAYFIRE_SOCKET",
];
const TILING_DESKTOP_NAMES: [&str; 7] = [
    "sway", "hyprland", "niri", "river", "dwl", "qtile", "wayfire",
];

/// Match `XDG_CURRENT_DESKTOP` / `XDG_SESSION_DESKTOP` against known tiling
/// compositors. Values are colon-separated lists; segments compare exactly.
pub fn desktop_names_indicate_tiling(
    current_desktop: Option<&str>,
    session_desktop: Option<&str>,
) -> bool {
    [current_desktop, session_desktop]
        .into_iter()
        .flatten()
        .flat_map(|v| v.split(':'))
        .map(|s| s.trim().to_ascii_lowercase())
        .any(|s| TILING_DESKTOP_NAMES.contains(&s.as_str()))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionEnv {
    pub wayland_session: bool,
    pub tiling_compositor: bool,
}

pub fn detect_session_env() -> SessionEnv {
    let wayland_session = std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var("XDG_SESSION_TYPE")
            .map(|v| v.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);
    let socket_marker = TILING_SOCKET_VARS
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
    let session = std::env::var("XDG_SESSION_DESKTOP").ok();
    SessionEnv {
        wayland_session,
        tiling_compositor: socket_marker
            || desktop_names_indicate_tiling(current.as_deref(), session.as_deref()),
    }
}

/// Effective decorations for the main window. Only meaningful on Linux; other
/// platforms keep native decorations unconditionally.
pub fn resolve_window_decorations(pref: DecorationsPref, env: SessionEnv) -> bool {
    match pref {
        DecorationsPref::On => true,
        DecorationsPref::Off => false,
        DecorationsPref::Auto => !(env.wayland_session && env.tiling_compositor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(wayland: bool, tiling: bool) -> SessionEnv {
        SessionEnv {
            wayland_session: wayland,
            tiling_compositor: tiling,
        }
    }

    #[test]
    fn pref_parses_and_defaults_to_auto() {
        assert_eq!(DecorationsPref::from_setting(None), DecorationsPref::Auto);
        assert_eq!(
            DecorationsPref::from_setting(Some("auto")),
            DecorationsPref::Auto
        );
        assert_eq!(DecorationsPref::from_setting(Some("on")), DecorationsPref::On);
        assert_eq!(
            DecorationsPref::from_setting(Some(" OFF ")),
            DecorationsPref::Off
        );
        assert_eq!(
            DecorationsPref::from_setting(Some("garbage")),
            DecorationsPref::Auto
        );
    }

    #[test]
    fn auto_keeps_decorations_on_stacking_desktops() {
        assert!(resolve_window_decorations(DecorationsPref::Auto, env(true, false)));
        assert!(resolve_window_decorations(DecorationsPref::Auto, env(false, false)));
    }

    #[test]
    fn auto_drops_decorations_only_for_tiling_wayland() {
        assert!(!resolve_window_decorations(DecorationsPref::Auto, env(true, true)));
        // X11 tiling (i3): tao draws no GTK titlebar, leave decorations alone.
        assert!(resolve_window_decorations(DecorationsPref::Auto, env(false, true)));
    }

    #[test]
    fn explicit_pref_overrides_detection_both_ways() {
        assert!(resolve_window_decorations(DecorationsPref::On, env(true, true)));
        assert!(!resolve_window_decorations(DecorationsPref::Off, env(false, false)));
    }

    #[test]
    fn desktop_name_matching_is_segment_exact() {
        assert!(desktop_names_indicate_tiling(Some("sway"), None));
        assert!(desktop_names_indicate_tiling(Some("Hyprland"), None));
        assert!(desktop_names_indicate_tiling(None, Some("niri")));
        assert!(desktop_names_indicate_tiling(Some("wlroots:river"), None));
        // Substrings and unrelated desktops must not match.
        assert!(!desktop_names_indicate_tiling(Some("swayimitator"), None));
        assert!(!desktop_names_indicate_tiling(Some("GNOME"), Some("gnome")));
        assert!(!desktop_names_indicate_tiling(Some("KDE"), None));
        assert!(!desktop_names_indicate_tiling(None, None));
    }
}
