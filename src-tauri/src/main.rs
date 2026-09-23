// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;

fn main() {
    let cli = flow_lib::cli::Cli::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(if cli.verbose {
        "debug"
    } else {
        "info"
    }))
    .format_timestamp_secs()
    .init();

    if let Some(command) = cli.command {
        std::process::exit(flow_lib::cli::run(command));
    }

    if cli.headless {
        std::process::exit(flow_lib::run_headless());
    }

    flow_lib::run();
}
