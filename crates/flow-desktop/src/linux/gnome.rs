//! GNOME Shell: everything goes through the Flow Shell extension.
//!
//! Mutter has no layer-shell, exposes no virtual keyboard to clients, and
//! only the Shell can see the focused window. The extension therefore owns
//! the island, the hotkey, focus context and text injection, and publishes
//! them on the session bus as `ai.flow.Island`. This module is the client.
//! GNOME on X11 uses it too whenever the extension is there, because the
//! extension grabs the same shortcut an X11 grab would.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flow_core::engine::{
    DesktopError, Event, Focus, FocusContext, Hotkey, HotkeyEvent, Injector, Overlay, State,
};
use zbus::blocking::fdo::DBusProxy;
use zbus::blocking::Connection;

pub const BUS_NAME: &str = "ai.flow.Island";

/// Longest wait for any reply from the extension. Every method returns at
/// once (InsertText finishes its paste afterwards), so this only trips when
/// the Shell is wedged, and then an error beats a daemon stuck forever.
const CALL_TIMEOUT: Duration = Duration::from_secs(2);
/// How often [`wait_for_extension`] looks for the bus name.
const PRESENCE_POLL: Duration = Duration::from_millis(250);

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
    /// Waits for the reply, unlike the calls above: a lost dictation must
    /// surface as an error, not vanish because the extension went away or
    /// threw.
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
    bus().is_some_and(|dbus| owned(&dbus).unwrap_or(false))
}

/// Block until the extension owns its bus name, for at most `timeout`.
///
/// At login `flow --headless` can start before the Shell has loaded its
/// extensions; checking once would leave it running with no hotkey. Returns
/// whether the extension showed up.
pub fn wait_for_extension(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut dbus = None;
    loop {
        // The session bus itself may not be up yet either, so keep trying
        // to connect as well, and start over if the connection breaks.
        if dbus.is_none() {
            dbus = bus();
        }
        match dbus.as_ref().map(owned) {
            Some(Ok(true)) => return true,
            Some(Err(_)) => dbus = None,
            _ => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        std::thread::sleep(PRESENCE_POLL.min(left));
    }
}

/// The version of the extension this build of Flow carries, from its
/// metadata.json; the extension reports the same number as `Version`.
pub fn bundled_extension_version() -> Option<u32> {
    const METADATA: &str = include_str!("../../../../extension/metadata.json");
    let (_, rest) = METADATA.split_once("\"version\"")?;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The version the running extension reports, if one is running and new
/// enough to say.
pub fn running_extension_version() -> Option<String> {
    GnomeIsland::connect().ok()?.extension_version()
}

/// Whether `running` (the running extension's version) is older than the
/// copy this build carries. The Shell keeps running an old copy after Flow
/// is upgraded, without the calls and bindings the new daemon expects. One
/// too old to report a version counts as older.
pub fn extension_outdated(running: Option<&str>) -> bool {
    let Some(bundled) = bundled_extension_version() else { return false };
    running.and_then(|v| v.trim().parse::<u32>().ok()).is_none_or(|v| v < bundled)
}

fn bus() -> Option<DBusProxy<'static>> {
    let conn = Connection::session().ok()?;
    DBusProxy::new(&conn).ok()
}

fn owned(dbus: &DBusProxy<'_>) -> zbus::Result<bool> {
    let name = zbus::names::BusName::try_from(BUS_NAME)?;
    Ok(dbus.name_has_owner(name)?)
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
        let conn = zbus::blocking::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build()?;
        let proxy = IslandProxy::new(&conn)?;
        Ok(GnomeIsland { proxy })
    }

    /// `None` from an extension too old to have the property.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_version_is_the_one_the_extension_reports() {
        let js = include_str!("../../../../extension/extension.js");
        let reported = js
            .split_once("const EXTENSION_VERSION = '")
            .and_then(|(_, rest)| rest.split_once('\''))
            .and_then(|(version, _)| version.parse::<u32>().ok());
        assert!(reported.is_some());
        assert_eq!(reported, bundled_extension_version());
    }

    #[test]
    fn older_or_unversioned_extensions_are_outdated() {
        let bundled = bundled_extension_version().unwrap();
        assert!(extension_outdated(None));
        assert!(extension_outdated(Some("garbage")));
        assert!(extension_outdated(Some(&(bundled - 1).to_string())));
        assert!(!extension_outdated(Some(&bundled.to_string())));
        assert!(!extension_outdated(Some(&(bundled + 1).to_string())));
    }
}
