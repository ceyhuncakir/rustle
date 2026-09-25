//! Smoke test against a live GNOME session: drive the island through D-Bus.
#[cfg(target_os = "linux")]
use rustle_core::engine::{Focus, Overlay, State};
#[cfg(target_os = "linux")]
use rustle_desktop::linux::gnome::GnomeIsland;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the GNOME island exists on Linux only");
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let island = GnomeIsland::connect()?;
    println!("extension version: {:?}", island.extension_version());
    println!("focus: {:?}", island.context()?);
    island.set_text("");
    island.set_state(State::Listening);
    for i in 0..60 {
        island.push_level(((i as f32 * 0.4).sin().abs()) * 0.9);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    island.set_state(State::Thinking);
    std::thread::sleep(std::time::Duration::from_millis(600));
    island.set_text("Rust says hello over D-Bus");
    island.set_state(State::Inserting);
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::thread::sleep(std::time::Duration::from_millis(1600));
    island.set_state(State::Hidden);
    println!("done");
    Ok(())
}
