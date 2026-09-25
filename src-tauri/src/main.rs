// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;

fn main() {
    let cli = rustle_lib::cli::Cli::parse();

    // A release build on Windows has no console of its own, so `rustle doctor`
    // typed in a terminal would print nothing. Borrow the terminal's.
    #[cfg(windows)]
    if cli.command.is_some() || cli.headless {
        attach_parent_console();
    }

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(if cli.verbose {
        "debug"
    } else {
        "info"
    }))
    .format_timestamp_secs()
    .init();

    // Once, on a machine where the app was installed under its old name.
    for note in rustle_core::config::migrate_from_flow() {
        log::info!("{note}");
    }

    if let Some(command) = cli.command {
        std::process::exit(rustle_lib::cli::run(command));
    }

    if cli.headless {
        std::process::exit(rustle_lib::run_headless());
    }

    rustle_lib::run();
}

#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    // SAFETY: plain Win32 call; fails harmlessly when started from Explorer.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
