//! GNOME Wayland: everything goes through the Flow Shell extension.
//!
//! Mutter has no layer-shell, exposes no virtual keyboard to clients, and
//! only the Shell can see the focused window. The extension therefore owns
//! the island, the hotkey, focus context and text injection, and publishes
//! them on the session bus as `ai.flow.Island`. This module is the client.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use flow_core::engine::{
    DesktopError, Event, Focus, FocusContext, Hotkey, HotkeyEvent, Injector, Overlay, State,
};
use zbus::blocking::Connection;

pub const BUS_NAME: &str = "ai.flow.Island";

#[zbus::proxy(
    interface = "ai.flow.Island",
    default_service = "ai.flow.Island",
    default_path = "/ai/flow/Island",
    gen_async = false
)]
trait Island {
    fn show(&self) -> zbus::Result<()>;
    fn hide(&self) -> zbus::Result<()>;
    /// Fire and forget: level pushes run at frame rate, and a blocking round
    /// trip per sample would show up as jitter in the waveform.
    #[zbus(no_reply)]
    fn set_state(&self, state: &str) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn set_text(&self, text: &str) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn push_level(&self, level: f64) -> zbus::Result<()>;
    #[zbus(no_reply)]
    fn insert_text(&self, text: &str) -> zbus::Result<()>;
    fn get_focus_context(&self) -> zbus::Result<HashMap<String, String>>;

    /// Added in extension version 4; older extensions have no such property.
    /// The extension never emits PropertiesChanged, so it must not be cached.
    #[zbus(property(emits_changed_signal = "false"))]
    fn version(&self) -> zbus::Result<String>;

    #[zbus(signal)]
    fn hotkey_pressed(&self, mode: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    fn hotkey_released(&self) -> zbus::Result<()>;
    #[zbus(signal)]
    fn cancel_requested(&self) -> zbus::Result<()>;
}

/// Whether the extension currently owns its bus name.
pub fn extension_present() -> bool {
    let Ok(conn) = Connection::session() else { return false };
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return false };
    let Ok(name) = zbus::names::BusName::try_from(BUS_NAME) else { return false };
    dbus.name_has_owner(name).unwrap_or(false)
}

pub struct GnomeIsland {
    proxy: IslandProxy<'static>,
}

impl GnomeIsland {
    /// Fail loudly at construction rather than silently dropping every later
    /// call: a missing extension is a setup error, not a runtime one.
    pub fn connect() -> anyhow::Result<GnomeIsland> {
        if !extension_present() {
            anyhow::bail!(
                "{BUS_NAME} is not on the session bus - is the Flow extension enabled? \
                 (gnome-extensions enable flow@ceyhun.dev, then log out and back in)"
            );
        }
        let conn = Connection::session()?;
        let proxy = IslandProxy::new(&conn)?;
        Ok(GnomeIsland { proxy })
    }

    pub fn extension_version(&self) -> Option<String> {
        self.proxy.version().ok()
    }

    fn proxy(&self) -> &IslandProxy<'static> {
        &self.proxy
    }
}

impl Overlay for GnomeIsland {
    fn set_state(&self, state: State) {
        if let Err(err) = self.proxy.set_state(state.as_str()) {
            log::debug!("SetState failed: {err}");
        }
    }
    fn set_text(&self, text: &str) {
        if let Err(err) = self.proxy.set_text(text) {
            log::debug!("SetText failed: {err}");
        }
    }
    fn push_level(&self, level: f32) {
        let _ = self.proxy.push_level(level as f64);
    }
}

impl Focus for GnomeIsland {
    fn context(&self) -> Result<FocusContext, DesktopError> {
        let map: HashMap<String, String> =
            self.proxy.get_focus_context().map_err(|e: zbus::Error| DesktopError::Failed(e.to_string()))?;
        let get = |k: &str| map.get(k).cloned().unwrap_or_default();
        Ok(FocusContext { app: get("app"), title: get("title"), role: get("role") })
    }
}

impl Injector for GnomeIsland {
    fn insert(&self, text: &str) -> Result<(), DesktopError> {
        self.proxy.insert_text(text).map_err(|e: zbus::Error| DesktopError::Failed(e.to_string()))
    }
}

/// Forwards the extension's already-resolved hotkey signals to the engine.
pub struct GnomeHotkey {
    island: Arc<GnomeIsland>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl GnomeHotkey {
    pub fn new(island: Arc<GnomeIsland>) -> GnomeHotkey {
        GnomeHotkey { island, threads: Vec::new() }
    }
}

impl Hotkey for GnomeHotkey {
    fn start(&mut self, sink: Sender<Event>) -> Result<(), DesktopError> {
        let fail = |e: zbus::Error| DesktopError::Unavailable(format!("signal subscription: {e}"));
        let pressed = self.island.proxy().receive_hotkey_pressed().map_err(fail)?;
        let released = self.island.proxy().receive_hotkey_released().map_err(fail)?;
        let cancel = self.island.proxy().receive_cancel_requested().map_err(fail)?;

        // One blocking iterator per signal; each ends when the connection
        // drops, which is when the island goes away.
        let tx = sink.clone();
        self.threads.push(std::thread::spawn(move || {
            for _ in pressed {
                if tx.send(Event::Hotkey(HotkeyEvent::Pressed)).is_err() {
                    break;
                }
            }
        }));
        let tx = sink.clone();
        self.threads.push(std::thread::spawn(move || {
            for _ in released {
                if tx.send(Event::Hotkey(HotkeyEvent::Released)).is_err() {
                    break;
                }
            }
        }));
        self.threads.push(std::thread::spawn(move || {
            for _ in cancel {
                if sink.send(Event::Hotkey(HotkeyEvent::Cancel)).is_err() {
                    break;
                }
            }
        }));
        Ok(())
    }

    fn stop(&mut self) {
        // The iterators end with the connection; nothing to interrupt.
        self.threads.clear();
    }
}
