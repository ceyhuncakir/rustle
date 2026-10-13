//! The cross-platform pieces: focused-window lookup through
//! `active-win-pos-rs`, and the paste chord through `enigo`. Used as-is on
//! Windows, macOS and X11.

use std::sync::{Arc, Mutex};

use rustle_core::config::DesktopConfig;
use rustle_core::engine::{DesktopError, Focus, FocusContext, Injector};

use crate::{clipboard, terminal, Backends, HotkeySource, OverlayMode, Session, WebviewHost};

pub struct ActiveWindowFocus;

impl Focus for ActiveWindowFocus {
    fn context(&self) -> Result<FocusContext, DesktopError> {
        let Some(win) = focused_window() else {
            return Ok(FocusContext::default());
        };
        Ok(FocusContext { app: win.app, title: win.title, role: win.process })
    }
}

/// The focused window: its class (the app id on Wayland), title and process
/// name. The process is empty when only the compositor was asked.
pub(crate) struct FocusedWindow {
    pub app: String,
    pub title: String,
    pub process: String,
}

/// Which window has focus. On Wayland compositors that list their windows
/// (sway, river, niri, labwc, Wayfire) the compositor is asked first:
/// `active-win-pos-rs` only knows KWin and Hyprland there, and otherwise
/// reads XWayland, which knows nothing of native Wayland windows.
pub(crate) fn focused_window() -> Option<FocusedWindow> {
    #[cfg(target_os = "linux")]
    if ask_the_compositor() {
        if let Some(toplevel) = crate::linux::toplevel::focused() {
            return Some(FocusedWindow {
                app: toplevel.app_id,
                title: toplevel.title,
                process: String::new(),
            });
        }
    }
    let win = active_win_pos_rs::get_active_window().ok()?;
    let process = win.process_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Some(FocusedWindow { app: win.app_name, title: win.title, process })
}

/// Wayland, except where `active-win-pos-rs` asks the compositor itself
/// (KWin, Hyprland). GNOME offers no window list, so the question costs one
/// round trip there before falling through.
#[cfg(target_os = "linux")]
fn ask_the_compositor() -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty());
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
    let native = desktop.contains("kde") || std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
    wayland && !native
}

/// Windows virtual-key code for V. Windows apps match shortcuts on virtual
/// keys, and every layout has a VK_V: Dvorak on its own V key, Russian or
/// Greek on the US one. `Key::Unicode('v')` has no key on those layouts, and
/// enigo would fall back to typing a literal 'v' that no app reads as paste.
const VK_V: u32 = 0x56;
/// macOS `kVK_ANSI_V`, the key in the US V position. A raw key code keeps
/// enigo away from the Text Input Sources API, which it would otherwise call
/// to find 'v' in the current layout and which asserts off the main thread
/// on macOS 14 and later. The cost: with plain Dvorak that key is '.', so
/// Dvorak users need the "Dvorak - QWERTY ⌘" layout.
const KVK_ANSI_V: u32 = 0x09;

/// Clipboard plus a synthesised Ctrl+V (Cmd+V on macOS, Ctrl+Shift+V in a
/// Linux terminal).
#[derive(Default)]
pub struct ClipboardPasteInjector {
    enigo: Mutex<Option<enigo::Enigo>>,
}

impl ClipboardPasteInjector {
    fn paste_chord(&self) -> Result<(), DesktopError> {
        use enigo::{Direction, Key, Keyboard};

        let mut guard = self.enigo.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let enigo = enigo::Enigo::new(&enigo::Settings::default())
                .map_err(|e| DesktopError::Unavailable(format!("input synthesis: {e}")))?;
            *guard = Some(enigo);
        }
        let enigo = guard.as_mut().expect("just set");

        let (modifier, v) = if cfg!(target_os = "macos") {
            (Key::Meta, Key::Other(KVK_ANSI_V))
        } else if cfg!(target_os = "windows") {
            (Key::Control, Key::Other(VK_V))
        } else {
            // X11: a keysym, which enigo finds on whatever key the layout
            // puts it, so Dvorak and friends press their own V.
            (Key::Control, Key::Unicode('v'))
        };
        let mut modifiers = vec![modifier];
        if cfg!(target_os = "linux") && terminal::focused_is_terminal() {
            modifiers.push(Key::Shift);
        }

        let fail = |e: enigo::InputError| DesktopError::Failed(format!("paste chord: {e}"));
        let mut pressed = Vec::new();
        let mut result = Ok(());
        for key in &modifiers {
            match enigo.key(*key, Direction::Press) {
                Ok(()) => pressed.push(*key),
                Err(e) => {
                    result = Err(fail(e));
                    break;
                }
            }
        }
        if result.is_ok() {
            result = enigo.key(v, Direction::Click).map_err(fail);
        }
        // Release whatever went down even after a failure, or the user is
        // left with a stuck Ctrl.
        for key in pressed.into_iter().rev() {
            let released = enigo.key(key, Direction::Release).map_err(fail);
            result = result.and(released);
        }
        result
    }
}

impl Injector for ClipboardPasteInjector {
    fn insert(&self, text: &str) -> Result<(), DesktopError> {
        clipboard::paste_with(text, || self.paste_chord())
    }
}

/// Backends for a desktop where our own window is the overlay and the app's
/// global-shortcut plugin is the hotkey (Windows, macOS, X11).
pub fn build(session: Session, config: &DesktopConfig, host: WebviewHost) -> anyhow::Result<Backends> {
    Ok(Backends {
        session,
        overlay: OverlayMode::from_config(config).window(host),
        hotkey: HotkeySource::AppShortcut(config.hotkey.clone()),
        focus: Arc::new(ActiveWindowFocus),
        injector: Arc::new(ClipboardPasteInjector::default()),
        notes: vec![format!("session: {session}"), "paste: clipboard + paste chord".into()],
    })
}
