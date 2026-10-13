//! The focused window, asked of the compositor through
//! wlr-foreign-toplevel-management: sway, river, niri, labwc, Wayfire and
//! Hyprland offer it to ordinary clients.
//!
//! `active-win-pos-rs` knows KWin and Hyprland, and otherwise reads
//! XWayland's `_NET_ACTIVE_WINDOW`, which is empty while a native Wayland
//! window has focus. On sway that left every terminal looking like an
//! editor, so it was sent Ctrl+V, which pastes nothing there. The protocol
//! lists every toplevel with its app id, title and state; the one marked
//! `activated` has focus.

use std::sync::mpsc;
use std::time::Duration;

use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};

/// Every paste asks, so a compositor slow to answer must not hold the paste
/// up. A healthy one answers in a millisecond or two.
const TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Toplevel {
    pub app_id: String,
    pub title: String,
}

/// The activated toplevel, or `None` when the compositor does not offer the
/// protocol (GNOME, KDE), nothing has focus, or it did not answer within
/// [`TIMEOUT`]. A compositor that never answers keeps the lookup's thread,
/// not the caller.
pub(crate) fn focused() -> Option<Toplevel> {
    std::env::var_os("WAYLAND_DISPLAY")?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("rustle-toplevel".into())
        .spawn(move || {
            let _ = tx.send(lookup());
        })
        .ok()?;
    match rx.recv_timeout(TIMEOUT) {
        Ok(found) => found,
        Err(_) => {
            log::debug!("the compositor did not list its windows within {} ms", TIMEOUT.as_millis());
            None
        }
    }
}

fn lookup() -> Option<Toplevel> {
    let conn = Connection::connect_to_env().map_err(|e| log::debug!("wayland: {e}")).ok()?;
    let (globals, mut queue) = registry_queue_init::<Toplevels>(&conn).ok()?;
    let qh = queue.handle();
    let manager: ZwlrForeignToplevelManagerV1 = globals.bind(&qh, 1..=3, ()).ok()?;
    let mut toplevels = Toplevels::default();
    // The first roundtrip brings the manager's announcement of every
    // toplevel; each one's app id, title and state follow it, and the
    // second roundtrip makes sure they are all in.
    let listed = queue.roundtrip(&mut toplevels).and_then(|_| queue.roundtrip(&mut toplevels));
    // Let the compositor stop sending before the connection goes.
    manager.stop();
    for handle in &toplevels.handles {
        handle.destroy();
    }
    let _ = conn.flush();
    if let Err(err) = listed {
        log::debug!("listing toplevels: {err}");
        return None;
    }
    activated(&toplevels.windows)
}

/// What one toplevel has told us about itself so far.
#[derive(Debug, Clone, Default)]
struct Window {
    app_id: String,
    title: String,
    activated: bool,
    closed: bool,
}

/// The first open, activated window.
fn activated(windows: &[(ObjectId, Window)]) -> Option<Toplevel> {
    windows
        .iter()
        .map(|(_, w)| w)
        .find(|w| w.activated && !w.closed)
        .map(|w| Toplevel { app_id: w.app_id.clone(), title: w.title.clone() })
}

/// Whether a `state` event's array (native-endian u32 values) says the
/// window has focus.
fn says_activated(state: &[u8]) -> bool {
    let activated = zwlr_foreign_toplevel_handle_v1::State::Activated as u32;
    state.as_chunks::<4>().0.iter().any(|v| u32::from_ne_bytes(*v) == activated)
}

#[derive(Default)]
struct Toplevels {
    windows: Vec<(ObjectId, Window)>,
    handles: Vec<ZwlrForeignToplevelHandleV1>,
}

impl Dispatch<WlRegistry, GlobalListContents> for Toplevels {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state.windows.push((toplevel.id(), Window::default()));
            state.handles.push(toplevel);
        }
    }

    event_created_child!(Toplevels, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Toplevels {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        let Some((_, window)) = state.windows.iter_mut().find(|(w, _)| *w == id) else { return };
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => window.app_id = app_id,
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => window.title = title,
            zwlr_foreign_toplevel_handle_v1::Event::State { state } => {
                window.activated = says_activated(&state)
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => window.closed = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    #[test]
    fn reads_the_activated_flag_among_the_others() {
        // maximized = 0, minimized = 1, activated = 2, fullscreen = 3
        assert!(says_activated(&state(&[2])));
        assert!(says_activated(&state(&[0, 3, 2])));
        assert!(!says_activated(&state(&[0, 1, 3])));
        assert!(!says_activated(&[]));
        // A torn value is ignored rather than misread.
        assert!(!says_activated(&[2, 0]));
    }

    #[test]
    fn picks_the_open_activated_window() {
        let window = |app_id: &str, activated: bool, closed: bool| Window {
            app_id: app_id.into(),
            title: format!("{app_id} title"),
            activated,
            closed,
        };
        let id = ObjectId::null;
        let windows = vec![
            (id(), window("firefox", false, false)),
            (id(), window("org.gnome.Nautilus", true, true)),
            (id(), window("foot", true, false)),
        ];
        assert_eq!(activated(&windows), Some(Toplevel { app_id: "foot".into(), title: "foot title".into() }));
        assert_eq!(activated(&windows[..2]), None);
        assert_eq!(activated(&[]), None);
    }
}
