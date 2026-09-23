//! Everything only the desktop can do: show the island, hear the hotkey,
//! read which app is focused, and paste into it.
//!
//! `#[cfg(target_os)]` appears exactly once, here at the module root. Inside
//! `linux/` the choice between GNOME (the Shell extension over D-Bus), X11 and
//! the Wayland compositors is made at runtime by [`session::detect`], so one
//! Linux binary serves every session and degrades gracefully.
//!
//! The Tauri app only ever matches on [`OverlayChoice`] and [`HotkeySource`]:
//! it creates a webview window and registers a shortcut when asked to, and
//! plugs the results back in. No `AppHandle` leaks into this crate.

use std::sync::Arc;

use flow_core::config::DesktopConfig;
use flow_core::engine::{Focus, Hotkey, Injector, Overlay};

mod clipboard;
pub mod generic;
pub mod session;

#[cfg(target_os = "linux")]
pub mod linux;

pub use session::Session;

/// How the island should be hosted on this desktop.
pub enum OverlayChoice {
    /// Something native draws it (the GNOME Shell extension). Ready to use.
    Native(Arc<dyn Overlay>),
    /// The app must create a webview window of this kind and drive it.
    Webview(WebviewHost),
    /// Never show it.
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebviewHost {
    /// Plain topmost, no-activate window (Windows, X11).
    TopLevel,
    /// Non-activating floating panel (macOS).
    NsPanel,
    /// wlr-layer-shell overlay surface (KDE, wlroots, COSMIC).
    LayerShell,
}

/// Where hotkey events come from.
pub enum HotkeySource {
    /// Already wired: the backend delivers events itself once started.
    Builtin(Box<dyn Hotkey>),
    /// The app must register this combination with its global-shortcut
    /// plugin and forward press/release as `HotkeyEvent::Down` / `Up`.
    AppShortcut(String),
    /// Nothing on this desktop can deliver a global hotkey to us.
    Unsupported(String),
}

pub struct Backends {
    pub session: Session,
    pub overlay: OverlayChoice,
    pub hotkey: HotkeySource,
    pub focus: Arc<dyn Focus>,
    pub injector: Arc<dyn Injector>,
    /// Human-readable notes for `flow doctor`: what was chosen and why.
    pub notes: Vec<String>,
}

/// Choose and construct the backends for the desktop we are running on.
pub fn build(config: &DesktopConfig) -> anyhow::Result<Backends> {
    let session = session::detect();
    build_for(session, config)
}

pub fn build_for(session: Session, config: &DesktopConfig) -> anyhow::Result<Backends> {
    #[cfg(target_os = "linux")]
    {
        linux::build(session, config)
    }
    #[cfg(target_os = "windows")]
    {
        generic::build(session, config, WebviewHost::TopLevel)
    }
    #[cfg(target_os = "macos")]
    {
        generic::build(session, config, WebviewHost::NsPanel)
    }
}

/// The overlay mode from config, normalised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    Auto,
    Window,
    Off,
}

impl OverlayMode {
    pub fn from_config(config: &DesktopConfig) -> OverlayMode {
        match config.overlay.as_str() {
            "off" => OverlayMode::Off,
            "window" => OverlayMode::Window,
            _ => OverlayMode::Auto,
        }
    }

    /// The choice for a desktop where the island is a window of our own.
    pub fn window(self, host: WebviewHost) -> OverlayChoice {
        match self {
            OverlayMode::Off => OverlayChoice::Off,
            _ => OverlayChoice::Webview(host),
        }
    }
}
