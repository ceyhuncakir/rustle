//! Linux: one binary, every session. The choice is made at runtime.

use std::sync::Arc;

use flow_core::config::DesktopConfig;

use crate::generic::{ActiveWindowFocus, ClipboardPasteInjector};
use crate::{Backends, HotkeySource, OverlayChoice, OverlayMode, Session, WebviewHost};

pub mod gnome;
pub mod wayland;

pub fn build(session: Session, config: &DesktopConfig) -> anyhow::Result<Backends> {
    let mode = OverlayMode::from_config(config);
    let mut notes = vec![format!("session: {session}")];

    match session {
        Session::GnomeWayland { extension: true } => {
            let island = Arc::new(gnome::GnomeIsland::connect()?);
            notes.push("overlay, hotkey, focus and paste: GNOME Shell extension over D-Bus".into());
            if let Some(version) = island.extension_version() {
                notes.push(format!("extension version {version}"));
            }
            Ok(Backends {
                session,
                overlay: OverlayChoice::Native(island.clone()),
                hotkey: HotkeySource::Builtin(Box::new(gnome::GnomeHotkey::new(island.clone()))),
                focus: island.clone(),
                injector: island,
                notes,
            })
        }

        Session::GnomeWayland { extension: false } => {
            // Mutter has no layer-shell and exposes no virtual keyboard, so
            // without the extension there is no overlay and no reliable
            // paste. Degrade rather than refuse: pasting through the
            // portal-less tool cascade may still work with ydotool.
            notes.push("Flow's GNOME Shell extension is not enabled: no island, no hotkey".into());
            Ok(Backends {
                session,
                overlay: OverlayChoice::Off,
                hotkey: HotkeySource::Unsupported(
                    "GNOME Wayland delivers global shortcuts only through the Flow Shell extension; enable it and log out".into(),
                ),
                focus: Arc::new(ActiveWindowFocus),
                injector: Arc::new(wayland::ToolCascadeInjector::default()),
                notes,
            })
        }

        Session::X11 => {
            notes.push("hotkey: X11 grab; paste: clipboard + XTEST".into());
            Ok(Backends {
                session,
                overlay: mode.window(WebviewHost::TopLevel),
                hotkey: HotkeySource::AppShortcut(config.hotkey.clone()),
                focus: Arc::new(ActiveWindowFocus),
                injector: Arc::new(ClipboardPasteInjector::default()),
                notes,
            })
        }

        Session::KdeWayland | Session::LayerShellWayland | Session::OtherWayland => {
            let overlay = match (mode, session) {
                (OverlayMode::Off, _) => OverlayChoice::Off,
                (OverlayMode::Window, _) => OverlayChoice::Webview(WebviewHost::TopLevel),
                (OverlayMode::Auto, Session::OtherWayland) => {
                    notes.push(
                        "overlay off: unknown compositor, cannot guarantee it will not take focus".into(),
                    );
                    OverlayChoice::Off
                }
                (OverlayMode::Auto, _) => OverlayChoice::Webview(WebviewHost::LayerShell),
            };
            let injector = wayland::ToolCascadeInjector::default();
            notes.push(format!("paste: {}", injector.describe()));
            Ok(Backends {
                session,
                overlay,
                // Portal and evdev hotkeys land in M5; until then the app's
                // shortcut plugin is a no-op on Wayland and doctor says so.
                hotkey: HotkeySource::Unsupported(
                    "global shortcuts on this Wayland desktop are not wired up yet".into(),
                ),
                focus: Arc::new(ActiveWindowFocus),
                injector: Arc::new(injector),
                notes,
            })
        }

        Session::Windows | Session::MacOs => unreachable!("not a Linux session"),
    }
}
