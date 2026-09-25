//! The Rustle desktop app: assembles the engine from the crates and puts a
//! tray icon, an overlay window and a settings window around it.

pub mod cli;
pub mod commands;
pub mod doctor;
pub mod eval;
#[cfg(target_os = "linux")]
pub mod gnome_extension;
pub mod host;
pub mod overlay;
pub mod tray;
pub mod updates;
pub mod watchdog;
pub mod windows;

use std::sync::Arc;

use log::{error, info, warn};
use tauri::Manager;

use host::Shared;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the full desktop app.
pub fn run() {
    let shared = Arc::new(Shared::load());

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch just brings up the settings window.
            windows::open_settings(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(shared.clone())
        .invoke_handler(tauri::generate_handler![
            overlay::overlay_resize,
            overlay::overlay_ready,
            commands::get_config,
            commands::set_config_value,
            commands::get_status,
            commands::set_running,
            commands::restart_engine,
            commands::list_input_devices,
            commands::list_stt_models,
            commands::download_model,
            commands::get_download,
            commands::cancel_download,
            commands::get_compute_report,
            commands::detect_gpu,
            commands::list_providers,
            commands::list_provider_models,
            commands::get_key_source,
            commands::set_api_key,
            commands::clear_api_key,
            commands::get_learning_summary,
            commands::forget_history,
            commands::run_doctor,
            commands::get_config_path,
            commands::open_config_file,
            commands::set_hotkey,
            commands::get_autostart,
            commands::set_autostart,
            updates::check_for_updates,
            updates::install_update,
            updates::open_release_page,
            commands::get_permissions,
            commands::request_permission,
            commands::copy_diagnostics,
            commands::wizard_test_mic,
            commands::wizard_test_transcribe,
            commands::wizard_test_paste,
            commands::wizard_test_cleanup,
            commands::wizard_complete,
        ])
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            tray::build(app.handle())?;

            // A signal quits like the tray's Quit, through RunEvent::Exit.
            let quitter = app.handle().clone();
            host::on_termination(move || quitter.exit(0));

            // At most once a day, and only a notification (updates.rs).
            updates::watch(app.handle().clone(), shared.clone());

            let handle = app.handle().clone();
            if host::first_run_pending() {
                info!("first run: opening the setup wizard");
                windows::open_first_run(&handle);
            } else {
                // Start dictating straight away; that is what the app is for.
                if let Err(err) = host::start(&shared, Some(&handle)) {
                    error!("could not start the engine: {err:#}");
                    windows::notify(&handle, "Rustle could not start", &format!("{err:#}"));
                    windows::open_settings(&handle);
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Rustle app")
        .run(|app, event| match event {
            // Closing the last window must not quit a tray app.
            tauri::RunEvent::ExitRequested { api, code: None, .. } => api.prevent_exit(),
            // Every way out comes through here: Quit, a signal, the last
            // call to `exit`. Stop the engine (which unloads the cleanup
            // model), then hand the GPU back.
            tauri::RunEvent::Exit => {
                if let Some(shared) = app.try_state::<Arc<Shared>>() {
                    if host::stop_for_exit(&shared, Some(app)) {
                        host::release_gpu(&shared);
                    }
                }
            }
            _ => {}
        });
}

/// Run the engine without any windows: the systemd path on GNOME, where the
/// Shell extension owns every pixel. Blocks until the engine stops, which a
/// signal asks it to do (`systemctl --user stop rustle`, Ctrl-C).
pub fn run_headless() -> i32 {
    let shared = Shared::load();
    // At login Rustle can be quicker than the Shell; without the extension
    // there is no shortcut. Failing lets systemd try again shortly.
    #[cfg(target_os = "linux")]
    if gnome_without_extension() {
        info!("waiting for Rustle's GNOME Shell extension");
        if !rustle_desktop::linux::gnome::wait_for_extension(std::time::Duration::from_secs(30)) {
            error!(
                "Rustle's GNOME Shell extension is not running. Enable it with `gnome-extensions enable {}` and log out and back in.",
                gnome_extension::UUID
            );
            return 1;
        }
    }
    match host::start(&shared, None) {
        Ok(()) => {}
        Err(err) => {
            error!("could not start: {err:#}");
            return 1;
        }
    }
    let Some(running) = shared.engine.lock().unwrap().take() else {
        warn!("engine did not start");
        return 1;
    };
    let tx = running.tx.clone();
    host::on_termination(move || {
        let _ = tx.send(rustle_core::engine::Event::Shutdown);
    });
    // The engine unloads the cleanup model on its way out; then the GPU.
    let _ = running.thread.join();
    if let Some(watchdog) = running.watchdog {
        watchdog.stand_down();
    }
    host::release_gpu(&shared);
    info!("stopped");
    0
}

/// A GNOME session whose Shell does not (yet) run Rustle's extension.
#[cfg(target_os = "linux")]
fn gnome_without_extension() -> bool {
    let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_lowercase().contains("gnome"));
    gnome && !rustle_desktop::linux::gnome::extension_present()
}
