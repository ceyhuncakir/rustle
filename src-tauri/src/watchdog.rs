//! Unloads the local cleanup model from Ollama when Flow ends without doing
//! it itself: `kill -9`, a crash, the OOM killer, "End task" on Windows.
//! Ollama cannot tell that Flow is gone and would otherwise keep the model
//! in video memory for `keep_alive`, an hour by default. Flow's own GPU
//! memory needs no such help: the driver frees it when the process ends.
//!
//! The watchdog is a second `flow` process that holds the read end of a pipe
//! whose write end only Flow has. However Flow ends, the OS closes the write
//! end and the watchdog reads end-of-file. A clean shutdown writes one byte
//! first, meaning "unloaded already"; end-of-file with nothing before it
//! means Flow died, and the watchdog unloads the model. It also unloads when
//! it is itself told to stop (SIGTERM, Ctrl-C), which is what systemd does to
//! the rest of the service when its main process dies.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use flow_core::backends::{Backend, OllamaBackend};
use flow_core::config::CleanupConfig;
use log::{info, warn};

/// The byte a clean shutdown sends before closing the pipe.
const STAND_DOWN: &[u8] = b"x";

pub struct Watchdog {
    child: Child,
    pipe: Option<ChildStdin>,
}

impl Watchdog {
    /// Start one for the configured cleanup model, when that is a local
    /// Ollama model. `None` for hosted backends, which hold no GPU of ours.
    pub fn spawn(cleanup: &CleanupConfig) -> Option<Watchdog> {
        if !cleanup.enabled || cleanup.backend != "ollama" {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let mut command = Command::new(exe);
        command
            .args(["watch-ollama", "--endpoint", &cleanup.endpoint, "--model", &cleanup.model])
            .stdin(Stdio::piped())
            .stdout(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        match command.spawn() {
            Ok(mut child) => {
                let pipe = child.stdin.take();
                Some(Watchdog { child, pipe })
            }
            Err(err) => {
                warn!("could not start the Ollama watchdog: {err}");
                None
            }
        }
    }

    /// Flow stopped cleanly and unloaded the model itself.
    pub fn stand_down(mut self) {
        if let Some(mut pipe) = self.pipe.take() {
            // Fails harmlessly when the watchdog already went, say because
            // it got the same SIGTERM from systemd.
            let _ = pipe.write_all(STAND_DOWN);
        }
        let _ = self.child.wait();
    }
}

/// `flow watch-ollama`: wait for Flow to end, then unload the model unless
/// Flow said it had.
pub fn run(endpoint: &str, model: &str) -> anyhow::Result<()> {
    let backend = OllamaBackend::new(model, endpoint, "0");
    let on_signal = OllamaBackend::new(model, endpoint, "0");
    if let Err(err) = ctrlc::set_handler(move || {
        on_signal.unload();
        std::process::exit(0);
    }) {
        warn!("watchdog: cannot catch termination signals: {err}");
    }
    if flow_died(std::io::stdin().lock()) {
        info!("watchdog: Flow ended without unloading {model}; unloading it");
        backend.unload();
    }
    Ok(())
}

/// Blocks until end-of-file. True when nothing came first: Flow died.
fn flow_died(mut pipe: impl Read) -> bool {
    let mut received = Vec::new();
    let _ = pipe.read_to_end(&mut received);
    received.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_of_file_alone_means_flow_died() {
        assert!(flow_died(&b""[..]));
    }

    #[test]
    fn a_clean_shutdown_stands_the_watchdog_down() {
        assert!(!flow_died(STAND_DOWN));
    }

    #[test]
    fn hosted_backends_need_no_watchdog() {
        let mut cleanup = CleanupConfig { backend: "anthropic".into(), ..CleanupConfig::default() };
        assert!(Watchdog::spawn(&cleanup).is_none());
        cleanup.backend = "ollama".into();
        cleanup.enabled = false;
        assert!(Watchdog::spawn(&cleanup).is_none());
    }
}
