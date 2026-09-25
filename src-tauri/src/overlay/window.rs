//! The island's OS window: made once, hidden; shown and hidden without ever
//! taking focus (the dictated text is pasted into whatever app the user is
//! in); above other windows, on every workspace, click-through, bottom-centre.
//!
//! The per-desktop work lives in modules that take raw handles only
//! (`layer_shell`, `ns_panel`, `win32`). Their calls must run on the main
//! thread, and the functions here are reached from the main thread (app
//! setup, sync commands), from command workers and from the engine thread,
//! so they go through `run_on_main_thread`: it runs the closure at once when
//! already there, and otherwise posts it without waiting.

use std::sync::atomic::{AtomicBool, Ordering};

use log::warn;
use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use super::WebviewHost;

#[cfg(target_os = "linux")]
#[path = "layer_shell.rs"]
mod layer_shell;
#[cfg(target_os = "macos")]
#[path = "ns_panel.rs"]
mod ns_panel;
#[cfg(target_os = "windows")]
#[path = "win32.rs"]
mod win32;

pub const LABEL: &str = "overlay";
/// Gap between the pill and the bottom of the work area, so it clears a dock.
const BOTTOM_MARGIN: f64 = 56.0;
/// The pill's size before the page reports one.
const IDLE_SIZE: (f64, f64) = (132.0, 44.0);
/// Room around the pill for its drop shadow (`0 10px 30px` in
/// ui/overlay/overlay.css). The page centres the pill, so the room above
/// matches the room below.
const SHADOW_ROOM: (f64, f64) = (30.0, 40.0);
/// Gap between the window and the bottom of the work area.
const WINDOW_BOTTOM_GAP: f64 = BOTTOM_MARGIN - SHADOW_ROOM.1;

/// Set once the window is a layer-shell surface. The compositor places it
/// then, from its anchor and margin; positions we set would be ignored.
static ANCHORED: AtomicBool = AtomicBool::new(false);

/// Create the window once, hidden. It is never destroyed.
pub fn ensure(app: &AppHandle, host: WebviewHost) -> anyhow::Result<WebviewWindow> {
    if let Some(window) = app.get_webview_window(LABEL) {
        return Ok(window);
    }
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay/index.html".into()))
        .title("Rustle")
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
        .inner_size(IDLE_SIZE.0 + 2.0 * SHADOW_ROOM.0, IDLE_SIZE.1 + 2.0 * SHADOW_ROOM.1);
    if !cfg!(target_os = "windows") {
        builder = builder.visible_on_all_workspaces(true);
    }
    let window = builder.build()?;
    // Off the main thread the window is only created once the main thread
    // gets to it; this closure is queued behind that, and ahead of any
    // later show, which is what gtk-layer-shell needs.
    on_main(&window, move |window| {
        prepare(window, host);
        place(window, IDLE_SIZE.0, IDLE_SIZE.1);
    });
    Ok(window)
}

/// Per-desktop setup, on the main thread, before the first show.
fn prepare(window: &WebviewWindow, host: WebviewHost) {
    match host {
        #[cfg(target_os = "linux")]
        WebviewHost::LayerShell => prepare_gtk(window, true),
        // X11, or Wayland with the overlay set to "window": a plain window,
        // kept above others by `always_on_top`, out of focus by `focusable`.
        #[cfg(target_os = "linux")]
        WebviewHost::TopLevel => prepare_gtk(window, false),
        #[cfg(target_os = "macos")]
        WebviewHost::NsPanel => match window.ns_window() {
            // SAFETY: Tauri's live NSWindow, and we are on the main thread.
            Ok(ns_window) => {
                if unsafe { ns_panel::prepare(ns_window) } {
                    log::info!("overlay: non-activating panel");
                }
            }
            Err(err) => {
                warn!("overlay: no NSWindow ({err}); the island may take focus");
                let _ = window.set_ignore_cursor_events(true);
            }
        },
        #[cfg(target_os = "windows")]
        WebviewHost::TopLevel => {
            if let Some(hwnd) = hwnd(window) {
                // SAFETY: Tauri's live HWND, on the thread that owns it.
                unsafe { win32::prepare(hwnd) };
            }
        }
        other => warn!("overlay: {other:?} is not available on this platform; using a plain window"),
    }
}

/// Bring the window up, without activating it.
pub fn show(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        // Not `show()`: on macOS that is `makeKeyAndOrderFront:`.
        on_main(window, |window| {
            if let Ok(ns_window) = window.ns_window() {
                // SAFETY: Tauri's live NSWindow, and we are on the main thread.
                unsafe { ns_panel::show(ns_window) };
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Tao shows with SW_SHOWNOACTIVATE on Windows (the window was built
        // unfocused) and with a plain map on Linux.
        let _ = window.show();
        // Click-through can only be applied once the window exists on
        // screen; on Linux it panics otherwise.
        let _ = window.set_ignore_cursor_events(true);
        #[cfg(target_os = "windows")]
        on_main(window, |window| {
            if let Some(hwnd) = hwnd(window) {
                // SAFETY: Tauri's live HWND, on the thread that owns it.
                unsafe { win32::raise(hwnd) };
            }
        });
    }
}

pub fn hide(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    on_main(window, |window| {
        if let Ok(ns_window) = window.ns_window() {
            // SAFETY: Tauri's live NSWindow, and we are on the main thread.
            unsafe { ns_panel::hide(ns_window) };
        }
    });
    #[cfg(not(target_os = "macos"))]
    let _ = window.hide();
}

/// Fit the window to a pill of `width` × `height` logical pixels plus its
/// shadow, at the bottom centre of the primary monitor's work area.
pub fn place(window: &WebviewWindow, width: f64, height: f64) {
    let (width, height) = (width + 2.0 * SHADOW_ROOM.0, height + 2.0 * SHADOW_ROOM.1);
    #[cfg(target_os = "linux")]
    request_size(window, width, height);
    if ANCHORED.load(Ordering::SeqCst) {
        // The compositor centres it on the output it picks (the focused one
        // on most) and keeps it clear of panels.
        let _ = window.set_size(tauri::LogicalSize::new(width, height));
        return;
    }
    let Ok(Some(monitor)) = window.primary_monitor() else { return };
    let scale = monitor.scale_factor();
    let work = monitor.work_area();
    let (w, h) = (width * scale, height * scale);
    let x = work.position.x as f64 + (work.size.width as f64 - w) / 2.0;
    let y = work.position.y as f64 + work.size.height as f64 - h - WINDOW_BOTTOM_GAP * scale;
    let _ = window.set_size(tauri::PhysicalSize::new(w.round() as u32, h.round() as u32));
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn on_main(window: &WebviewWindow, task: impl FnOnce(&WebviewWindow) + Send + 'static) {
    let target = window.clone();
    if let Err(err) = window.run_on_main_thread(move || task(&target)) {
        warn!("overlay: could not reach the main thread: {err}");
    }
}

#[cfg(target_os = "linux")]
fn prepare_gtk(window: &WebviewWindow, layer_shell: bool) {
    use gtk::prelude::*;

    let gtk_window = match window.gtk_window() {
        Ok(gtk_window) => gtk_window,
        Err(err) => {
            warn!("overlay: no GTK window ({err}); using a plain window");
            return;
        }
    };
    // Both changes below have to land before GDK makes a surface for the
    // window. Tao and wry leave a hidden window unrealized; if that ever
    // changes, undo it rather than fail.
    if gtk_window.is_realized() {
        gtk_window.unrealize();
    }
    // Tao gives every Wayland window a header bar as its titlebar, which
    // turns on GTK's client-side decorations even for an undecorated
    // window. GTK then resets the input region to the whole window on every
    // resize, and clicks on the pill stop going through to the app below.
    gtk_window.set_titlebar(None::<&gtk::Widget>);
    if !layer_shell {
        return;
    }
    let ptr = gtk_window.upcast_ref::<gtk::Window>().as_ptr();
    // SAFETY: a live, unrealized GtkWindow, and we are on the GTK main thread.
    match unsafe { layer_shell::init(ptr.cast(), WINDOW_BOTTOM_GAP as i32) } {
        Ok(()) => {
            ANCHORED.store(true, Ordering::SeqCst);
            log::info!("overlay: layer-shell surface");
        }
        Err(why) => warn!("overlay: {why}; using a plain window, which may take focus"),
    }
}

/// GTK sizes a non-resizable window to its natural size, and gives a window
/// whose content has none (a webview) a 200 × 200 fallback, so a resize
/// alone leaves the pill in a window far taller than asked for, and off
/// centre. An explicit size request replaces that fallback. (Making the
/// window resizable instead stops a layer surface from following resizes.)
#[cfg(target_os = "linux")]
fn request_size(window: &WebviewWindow, width: f64, height: f64) {
    use gtk::prelude::*;

    let (width, height) = (width.round() as i32, height.round() as i32);
    on_main(window, move |window| {
        if let Ok(gtk_window) = window.gtk_window() {
            gtk_window.set_size_request(width, height);
        }
    });
}

#[cfg(target_os = "windows")]
fn hwnd(window: &WebviewWindow) -> Option<*mut std::ffi::c_void> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    match window.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get() as *mut std::ffi::c_void),
        Ok(_) => None,
        Err(err) => {
            warn!("overlay: no window handle: {err}");
            None
        }
    }
}
