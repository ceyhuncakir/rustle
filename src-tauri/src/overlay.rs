//! The island as a window of our own, for desktops without the GNOME
//! extension. The pill itself is `ui/overlay`, a canvas port of the Shell
//! island; this side owns the OS window: on top, transparent, never focused,
//! click-through, sized and centred on request.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flow_core::engine::{Overlay, State};
use flow_desktop::WebviewHost;
use log::warn;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const WINDOW_LABEL: &str = "overlay";
/// Gap from the bottom of the work area, so the pill clears a dock.
const BOTTOM_MARGIN: f64 = 56.0;
/// The pill fades for 180 ms before the window can go.
const HIDE_AFTER: Duration = Duration::from_millis(220);
const IDLE_SIZE: (f64, f64) = (132.0, 44.0);

pub struct WebviewOverlay {
    app: AppHandle,
    /// Bumped on every state change so a pending hide can tell it was
    /// superseded.
    hide_seq: Arc<AtomicU64>,
    /// Whether the window is on screen. Kept here rather than asked with
    /// `is_visible`: that call waits for the main thread, and the engine
    /// thread calls in here while the main thread may be waiting for it.
    shown: Arc<AtomicBool>,
}

impl WebviewOverlay {
    pub fn create(app: &AppHandle, host: WebviewHost) -> anyhow::Result<WebviewOverlay> {
        ensure_window(app, host)?;
        Ok(WebviewOverlay { app: app.clone(), hide_seq: Arc::default(), shown: Arc::default() })
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.app.emit_to(WINDOW_LABEL, event, payload);
    }
}

impl Overlay for WebviewOverlay {
    fn set_state(&self, state: State) {
        let seq = self.hide_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.emit("flow:state", serde_json::json!({ "state": state.as_str() }));
        let Some(window) = self.app.get_webview_window(WINDOW_LABEL) else { return };

        if state == State::Hidden {
            let hide_seq = self.hide_seq.clone();
            let shown = self.shown.clone();
            std::thread::spawn(move || {
                std::thread::sleep(HIDE_AFTER);
                if hide_seq.load(Ordering::SeqCst) == seq {
                    shown.store(false, Ordering::SeqCst);
                    let _ = window.hide();
                }
            });
        } else if !self.shown.swap(true, Ordering::SeqCst) {
            let _ = window.show();
            // Click-through can only be applied once the window exists on
            // screen; on Linux it panics otherwise.
            let _ = window.set_ignore_cursor_events(true);
            #[cfg(target_os = "windows")]
            platform::force_topmost(&window);
        }
    }

    fn set_text(&self, text: &str) {
        self.emit("flow:text", serde_json::json!({ "text": text }));
    }

    fn push_level(&self, level: f32) {
        self.emit("flow:level", serde_json::json!({ "level": level }));
    }
}

/// Create the overlay window once, hidden. It is never destroyed.
fn ensure_window(app: &AppHandle, host: WebviewHost) -> anyhow::Result<WebviewWindow> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        return Ok(window);
    }
    let mut builder =
        WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("overlay/index.html".into()))
            .title("Flow")
            .decorations(false)
            .transparent(true)
            .shadow(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focusable(false)
            .focused(false)
            .resizable(false)
            .visible(false)
            .accept_first_mouse(false)
            .inner_size(IDLE_SIZE.0, IDLE_SIZE.1);
    if !cfg!(target_os = "windows") {
        builder = builder.visible_on_all_workspaces(true);
    }
    let window = builder.build()?;
    match host {
        WebviewHost::LayerShell => platform::init_layer_shell(&window),
        WebviewHost::NsPanel => platform::init_panel(&window),
        WebviewHost::TopLevel => {}
    }
    place(&window, IDLE_SIZE.0, IDLE_SIZE.1);
    Ok(window)
}

/// Bottom-centre of the primary monitor's work area, in logical pixels.
fn place(window: &WebviewWindow, width: f64, height: f64) {
    let Ok(Some(monitor)) = window.primary_monitor() else { return };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let (w, h) = (width * scale, height * scale);
    let x = work.position.x as f64 + (work.size.width as f64 - w) / 2.0;
    let y = work.position.y as f64 + work.size.height as f64 - h - BOTTOM_MARGIN * scale;
    let _ = window.set_size(tauri::PhysicalSize::new(w.round() as u32, h.round() as u32));
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

/// The pill asks for its size; we resize and re-centre the OS window.
#[tauri::command]
pub fn overlay_resize(app: AppHandle, width: f64, height: f64) {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        place(&window, width, height);
    }
}

/// The page loaded; tell it the current state in case one was set before.
#[tauri::command]
pub fn overlay_ready(app: AppHandle, shared: tauri::State<'_, Arc<crate::host::Shared>>) {
    let state = *shared.state.lock().unwrap();
    let _ = app.emit_to(WINDOW_LABEL, "flow:state", serde_json::json!({ "state": state.as_str() }));
}

/// Per-desktop window tweaks. Each is a milestone of its own; until then the
/// plain window is used and the log says so.
mod platform {
    use super::*;

    /// Windows: `always_on_top` can be overridden by other topmost windows;
    /// asserting Z-order directly is more reliable.
    #[cfg(target_os = "windows")]
    pub fn force_topmost(_window: &WebviewWindow) {}

    /// KDE / wlroots: a wlr-layer-shell overlay surface cannot take focus.
    /// gtk-layer-shell must be initialised before the window is realised.
    pub fn init_layer_shell(_window: &WebviewWindow) {
        warn!("layer-shell overlay not wired up yet; using a plain window");
    }

    /// macOS: a non-activating NSPanel so the user's app keeps focus.
    pub fn init_panel(_window: &WebviewWindow) {
        warn!("NSPanel overlay not wired up yet; using a plain window");
    }
}
