//! Which desktop are we on? Decided once at startup.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    Windows,
    MacOs,
    X11,
    /// GNOME on Wayland. `extension` says whether the Flow Shell extension is
    /// on the session bus right now.
    GnomeWayland {
        extension: bool,
    },
    KdeWayland,
    /// wlroots-style compositors and others that speak wlr-layer-shell
    /// (Hyprland, sway, river, niri, Wayfire, COSMIC).
    LayerShellWayland,
    OtherWayland,
}

impl fmt::Display for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Session::Windows => write!(f, "Windows"),
            Session::MacOs => write!(f, "macOS"),
            Session::X11 => write!(f, "Linux X11"),
            Session::GnomeWayland { extension: true } => write!(f, "GNOME Wayland (Flow extension present)"),
            Session::GnomeWayland { extension: false } => write!(f, "GNOME Wayland (Flow extension missing)"),
            Session::KdeWayland => write!(f, "KDE Plasma Wayland"),
            Session::LayerShellWayland => write!(f, "Wayland (layer-shell compositor)"),
            Session::OtherWayland => write!(f, "Wayland (unknown compositor)"),
        }
    }
}

pub fn detect() -> Session {
    if cfg!(target_os = "windows") {
        return Session::Windows;
    }
    if cfg!(target_os = "macos") {
        return Session::MacOs;
    }
    detect_linux(&LinuxEnv::from_process())
}

/// The environment variables Linux detection reads, so the logic is testable.
#[derive(Debug, Default, Clone)]
pub struct LinuxEnv {
    pub flow_desktop: Option<String>,
    pub session_type: Option<String>,
    pub current_desktop: Option<String>,
    pub wayland_display: Option<String>,
    /// Whether `ai.flow.Island` has an owner on the session bus.
    pub extension_present: bool,
}

impl LinuxEnv {
    pub fn from_process() -> LinuxEnv {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        // Only Linux looks for the GNOME extension below.
        #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
        let mut this = LinuxEnv {
            flow_desktop: env("FLOW_DESKTOP"),
            session_type: env("XDG_SESSION_TYPE"),
            current_desktop: env("XDG_CURRENT_DESKTOP"),
            wayland_display: env("WAYLAND_DISPLAY"),
            extension_present: false,
        };
        #[cfg(target_os = "linux")]
        {
            let looks_like_gnome = this
                .current_desktop
                .as_deref()
                .map(|d| d.to_ascii_lowercase().contains("gnome"))
                .unwrap_or(false);
            if looks_like_gnome || this.flow_desktop.as_deref() == Some("gnome") {
                this.extension_present = crate::linux::gnome::extension_present();
            }
        }
        this
    }
}

pub fn detect_linux(env: &LinuxEnv) -> Session {
    if let Some(forced) = env.flow_desktop.as_deref() {
        match forced {
            "gnome" => return Session::GnomeWayland { extension: env.extension_present },
            "x11" => return Session::X11,
            "kde" => return Session::KdeWayland,
            "layer-shell" | "wlroots" | "hyprland" | "sway" => return Session::LayerShellWayland,
            "wayland" => return Session::OtherWayland,
            other => log::warn!("FLOW_DESKTOP={other:?} not understood, detecting instead"),
        }
    }

    let wayland = env.wayland_display.is_some() || env.session_type.as_deref() == Some("wayland");
    if !wayland {
        return Session::X11;
    }

    let desktop = env.current_desktop.as_deref().unwrap_or("").to_ascii_lowercase();
    if desktop.contains("gnome") {
        return Session::GnomeWayland { extension: env.extension_present };
    }
    if desktop.contains("kde") || desktop.contains("plasma") {
        return Session::KdeWayland;
    }
    for name in ["hyprland", "sway", "river", "niri", "wayfire", "cosmic", "labwc"] {
        if desktop.contains(name) {
            return Session::LayerShellWayland;
        }
    }
    Session::OtherWayland
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(session_type: &str, desktop: &str, wayland: bool) -> LinuxEnv {
        LinuxEnv {
            flow_desktop: None,
            session_type: Some(session_type.into()),
            current_desktop: Some(desktop.into()),
            wayland_display: wayland.then(|| "wayland-0".into()),
            extension_present: true,
        }
    }

    #[test]
    fn gnome_wayland() {
        assert_eq!(detect_linux(&env("wayland", "GNOME", true)), Session::GnomeWayland { extension: true });
        assert_eq!(
            detect_linux(&env("wayland", "ubuntu:GNOME", true)),
            Session::GnomeWayland { extension: true }
        );
    }

    #[test]
    fn x11_sessions() {
        assert_eq!(detect_linux(&env("x11", "GNOME", false)), Session::X11);
        assert_eq!(detect_linux(&env("x11", "XFCE", false)), Session::X11);
    }

    #[test]
    fn kde_and_wlroots() {
        assert_eq!(detect_linux(&env("wayland", "KDE", true)), Session::KdeWayland);
        assert_eq!(detect_linux(&env("wayland", "Hyprland", true)), Session::LayerShellWayland);
        assert_eq!(detect_linux(&env("wayland", "sway", true)), Session::LayerShellWayland);
        assert_eq!(detect_linux(&env("wayland", "Weston", true)), Session::OtherWayland);
    }

    #[test]
    fn override_wins() {
        let mut e = env("wayland", "GNOME", true);
        e.flow_desktop = Some("x11".into());
        assert_eq!(detect_linux(&e), Session::X11);
    }
}
