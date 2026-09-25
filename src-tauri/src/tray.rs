//! The tray icon. On Windows and macOS this is the app; on Linux the tray
//! has no left-click event, so everything lives in the menu.

use std::sync::Arc;

use log::warn;
use rustle_core::engine::State;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

use crate::host::Shared;

pub const TRAY_ID: &str = "rustle-tray";
const ITEM_TOGGLE: &str = "toggle";
const ITEM_SETTINGS: &str = "settings";
const ITEM_DOCTOR: &str = "doctor";
const ITEM_QUIT: &str = "quit";

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu(app, false)?)
        .tooltip("Rustle")
        .show_menu_on_left_click(true)
        // Menu events arrive on the main thread, which starting and stopping
        // the engine may wait for; see `host`. So the work goes elsewhere.
        .on_menu_event(|app, event| match event.id().as_ref() {
            ITEM_TOGGLE => in_background(app, |app| {
                let shared = app.state::<Arc<Shared>>();
                if shared.running() {
                    crate::host::turn_off(&shared, Some(app));
                } else if let Err(err) = crate::host::start(&shared, Some(app)) {
                    warn!("could not start: {err:#}");
                    crate::windows::notify(app, "Rustle could not start", &format!("{err:#}"));
                }
                sync_toggle(app);
            }),
            ITEM_SETTINGS => crate::windows::open_settings(app),
            ITEM_DOCTOR => crate::windows::open_settings(app),
            ITEM_QUIT => in_background(app, |app| {
                crate::host::stop(&app.state::<Arc<Shared>>(), Some(app));
                app.exit(0);
            }),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }
    builder.build(app)?;
    sync_toggle(app);
    Ok(())
}

fn in_background(app: &AppHandle, work: impl FnOnce(&AppHandle) + Send + 'static) {
    let app = app.clone();
    if let Err(err) = std::thread::Builder::new().name("rustle-tray".into()).spawn(move || work(&app)) {
        warn!("could not act on the tray menu: {err}");
    }
}

/// Menu items cannot be updated in place portably, so the menu is rebuilt
/// whenever the running state changes. It is four items; that is cheap.
fn menu(app: &AppHandle, running: bool) -> tauri::Result<Menu<Wry>> {
    Menu::with_items(
        app,
        &[
            &CheckMenuItem::with_id(app, ITEM_TOGGLE, "Dictation", true, running, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, ITEM_SETTINGS, "Settings…", true, None::<&str>)?,
            &MenuItem::with_id(app, ITEM_DOCTOR, "Check setup", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, ITEM_QUIT, "Quit Rustle", true, None::<&str>)?,
        ],
    )
}

/// Keep the "Dictation" check mark in step with whether the engine runs.
pub fn sync_toggle(app: &AppHandle) {
    let running = app.state::<Arc<Shared>>().running();
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let _ = tray.set_tooltip(Some(if running { "Rustle - ready" } else { "Rustle - stopped" }));
    if let Ok(menu) = menu(app, running) {
        let _ = tray.set_menu(Some(menu));
    }
}

/// Called by the engine for every state change; the tooltip is the cheapest
/// place to reflect it.
pub fn reflect_state(app: &AppHandle, state: State) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let text = match state {
        State::Listening => "Rustle - listening",
        State::Thinking => "Rustle - thinking",
        State::Inserting => "Rustle - inserting",
        State::Error => "Rustle - error",
        State::Idle | State::Hidden => "Rustle - ready",
    };
    let _ = tray.set_tooltip(Some(text));
}
