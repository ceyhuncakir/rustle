//! Linux: one binary, every session. The choice is made at runtime.

use std::sync::Arc;

use rustle_core::config::DesktopConfig;

use crate::generic::{ActiveWindowFocus, ClipboardPasteInjector};
use crate::{Backends, HotkeySource, OverlayChoice, OverlayMode, Session, WebviewHost};

pub mod control;
pub mod gnome;
pub mod portal;
pub mod wayland;

pub fn build(session: Session, config: &DesktopConfig) -> anyhow::Result<Backends> {
    let mode = OverlayMode::from_config(config);
    let mut notes = vec![format!("session: {session}")];

    match session {
        Session::GnomeWayland { extension: true } => {
            let island = Arc::new(gnome::GnomeIsland::connect()?);
            notes.push("overlay, hotkey, focus and paste: GNOME Shell extension over D-Bus".into());
            let version = island.extension_version();
            if let Some(version) = &version {
                notes.push(format!("extension version {version}"));
            }
            if gnome::extension_outdated(version.as_deref()) {
                log::warn!(
                    "the running GNOME extension ({}) is older than version {} that this Rustle carries; \
                     update it under Settings > Desktop and log out and back in",
                    version.as_deref().unwrap_or("no version"),
                    gnome::bundled_extension_version().unwrap_or_default()
                );
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
            // portal-less tool cascade may still work with ydotool, and
            // GNOME 48 and later hand out shortcuts through the portal.
            notes.push("Rustle's GNOME Shell extension is not enabled: no island".into());
            let hotkey = wayland_hotkey(session, config, &mut notes);
            Ok(Backends {
                session,
                overlay: OverlayChoice::Off,
                hotkey,
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
            let hotkey = wayland_hotkey(session, config, &mut notes);
            Ok(Backends {
                session,
                overlay,
                hotkey,
                focus: Arc::new(ActiveWindowFocus),
                injector: Arc::new(injector),
                notes,
            })
        }

        Session::Windows | Session::MacOs => unreachable!("not a Linux session"),
    }
}

/// Wayland without Rustle's extension: the portal's global shortcuts where
/// the desktop offers them, else the compositor's own key bindings running
/// `rustle hotkey`. Only asks the portal what it offers; binding waits for
/// the engine to start, so `rustle doctor` never pops up a dialog.
fn wayland_hotkey(session: Session, config: &DesktopConfig, notes: &mut Vec<String>) -> HotkeySource {
    match portal::version() {
        Some(version) => {
            notes.push(format!("hotkey: desktop portal global shortcuts (version {version})"));
            HotkeySource::Builtin(Box::new(portal::PortalHotkey::new(&config.hotkey)))
        }
        None => {
            notes.push(
                "hotkey: the desktop portal has no global shortcuts; key bindings must run `rustle hotkey`"
                    .into(),
            );
            HotkeySource::External(binding_help(session))
        }
    }
}

/// How to reach Rustle from a desktop's own key bindings, for a desktop whose
/// portal has no global shortcuts.
pub fn binding_help(session: Session) -> String {
    match session {
        Session::GnomeWayland { .. } => {
            "This GNOME has no shortcut portal (that came with GNOME 48). Enable Rustle's GNOME Shell \
             extension, or add a custom shortcut in Settings → Keyboard → Keyboard Shortcuts that runs \
             `rustle hotkey toggle`."
        }
        Session::KdeWayland => {
            "This Plasma has no shortcut portal. Add a custom shortcut in System Settings → Keyboard → \
             Shortcuts that runs `rustle hotkey toggle`."
        }
        _ => {
            "Your compositor's portal has no global shortcuts, so bind a key in its config to run \
             `rustle hotkey down` on press and `rustle hotkey up` on release (sway: `bindsym --no-repeat \
             Ctrl+Alt+space exec rustle hotkey down` and `bindsym --release Ctrl+Alt+space exec rustle hotkey \
             up`), or one key to `rustle hotkey toggle`."
        }
    }
    .into()
}
