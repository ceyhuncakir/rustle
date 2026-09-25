//! The island as a window of our own, for desktops without the GNOME
//! extension. The pill itself is `ui/overlay`, a canvas port of the Shell
//! island; this side drives it from the engine, and `window` owns the OS
//! window: on top, transparent, never focused, click-through, sized and
//! centred on request.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustle_core::engine::{Overlay, State};
use rustle_desktop::WebviewHost;
use tauri::{AppHandle, Emitter, Manager};

mod window;

pub const WINDOW_LABEL: &str = window::LABEL;
/// The pill fades for 180 ms before the window can go.
const HIDE_AFTER: Duration = Duration::from_millis(220);

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
        window::ensure(app, host)?;
        Ok(WebviewOverlay { app: app.clone(), hide_seq: Arc::default(), shown: Arc::default() })
    }

    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.app.emit_to(WINDOW_LABEL, event, payload);
    }
}

impl Overlay for WebviewOverlay {
    fn set_state(&self, state: State) {
        let seq = self.hide_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.emit("rustle:state", serde_json::json!({ "state": state.as_str() }));
        let Some(overlay) = self.app.get_webview_window(WINDOW_LABEL) else { return };

        if state == State::Hidden {
            let hide_seq = self.hide_seq.clone();
            let shown = self.shown.clone();
            std::thread::spawn(move || {
                std::thread::sleep(HIDE_AFTER);
                if hide_seq.load(Ordering::SeqCst) == seq {
                    shown.store(false, Ordering::SeqCst);
                    window::hide(&overlay);
                }
            });
        } else if !self.shown.swap(true, Ordering::SeqCst) {
            window::show(&overlay);
        }
    }

    fn set_text(&self, text: &str) {
        self.emit("rustle:text", serde_json::json!({ "text": text }));
    }

    fn push_level(&self, level: f32) {
        self.emit("rustle:level", serde_json::json!({ "level": level }));
    }
}

/// The pill asks for its size; we resize and re-centre the OS window.
#[tauri::command]
pub fn overlay_resize(app: AppHandle, width: f64, height: f64) {
    if let Some(overlay) = app.get_webview_window(WINDOW_LABEL) {
        window::place(&overlay, width, height);
    }
}

/// The page loaded; tell it the current state in case one was set before.
#[tauri::command]
pub fn overlay_ready(app: AppHandle, shared: tauri::State<'_, Arc<crate::host::Shared>>) {
    let state = *shared.state.lock().unwrap();
    let _ = app.emit_to(WINDOW_LABEL, "rustle:state", serde_json::json!({ "state": state.as_str() }));
}
