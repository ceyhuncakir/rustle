//! The settings and first-run windows, created on demand.

use log::warn;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

pub const SETTINGS: &str = "settings";
pub const FIRST_RUN: &str = "first-run";

pub fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    let result = WebviewWindowBuilder::new(app, SETTINGS, WebviewUrl::App("settings/index.html".into()))
        .title("Rustle")
        .inner_size(720.0, 820.0)
        .min_inner_size(560.0, 480.0)
        .center()
        .build();
    if let Err(err) = result {
        warn!("could not open settings: {err}");
    }
}

pub fn open_first_run(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(FIRST_RUN) {
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let result = WebviewWindowBuilder::new(app, FIRST_RUN, WebviewUrl::App("first-run/index.html".into()))
        .title("Welcome to Rustle")
        .inner_size(760.0, 640.0)
        .min_inner_size(640.0, 520.0)
        .center()
        .build();
    if let Err(err) = result {
        warn!("could not open the setup wizard: {err}");
    }
}

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        warn!("notification failed: {err}");
    }
}
