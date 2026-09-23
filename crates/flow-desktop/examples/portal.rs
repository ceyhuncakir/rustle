//! Live check of the GlobalShortcuts portal: bind Flow's shortcuts, print
//! what the desktop says and every event that arrives, then let go. The
//! desktop may show its dialog the first time.
//!
//!     cargo run -p flow-desktop --example portal -- [Ctrl+Alt+Space] [seconds]

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the desktop portal exists on Linux only");
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::time::{Duration, Instant};

    use flow_core::engine::{Event, Hotkey};
    use flow_desktop::linux::portal::{self, PortalHotkey};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let mut args = std::env::args().skip(1);
    let combo = args.next().unwrap_or_else(|| "Ctrl+Alt+Space".into());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);

    println!("GlobalShortcuts version: {:?}", portal::version());
    println!("suggesting {:?}", portal::portal_trigger(&combo));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut hotkey = PortalHotkey::new(&combo);
    hotkey.start(tx)?;
    let state = portal::wait_while_binding(Duration::from_secs(180));
    println!("state: {state:?}");
    if let Some(trigger) = state.dictate_trigger() {
        println!("dictate is {trigger:?}, shown as {:?}", portal::display_trigger(trigger));
    }
    // What the wizard's Grant button does while dictation runs.
    println!("rebind: {:?}", portal::rebind().map(|s| s.is_bound()));

    println!("press it within {seconds}s");
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Hotkey(event)) => println!("{:>8.3}s  {event:?}", start.elapsed().as_secs_f32()),
            Ok(_) => {}
            Err(_) => break,
        }
    }

    hotkey.stop();
    std::thread::sleep(Duration::from_millis(300));
    println!(
        "stopped; state {:?}; portal thread still alive: {}",
        portal::state(),
        thread_alive("flow-portal")
    );
    Ok(())
}

/// Whether a thread of this process has the given name.
#[cfg(target_os = "linux")]
fn thread_alive(name: &str) -> bool {
    std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
        .any(|task| std::fs::read_to_string(task.path().join("comm")).is_ok_and(|comm| comm.trim() == name))
}
