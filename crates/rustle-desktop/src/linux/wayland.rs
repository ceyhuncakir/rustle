//! Wayland without the GNOME extension: KDE, wlroots compositors and the
//! rest.
//!
//! No compositor except wlroots exposes `zwp_virtual_keyboard_v1` to a plain
//! client, so the paste chord goes through whichever helper is installed:
//! `dotool` or `ydotool` (uinput, work everywhere), or `wtype` (wlroots
//! only). The clipboard goes through `arboard`'s data-control support where
//! the compositor has it (KDE, wlroots), and through XWayland where it does
//! not (GNOME).

use std::io::Write;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rustle_core::engine::{DesktopError, Injector};

use crate::{clipboard, terminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteTool {
    Dotool,
    Ydotool,
    Wtype,
}

impl PasteTool {
    /// In order of how widely each works.
    const ALL: [PasteTool; 3] = [PasteTool::Dotool, PasteTool::Ydotool, PasteTool::Wtype];

    pub fn binary(self) -> &'static str {
        match self {
            PasteTool::Dotool => "dotool",
            PasteTool::Ydotool => "ydotool",
            PasteTool::Wtype => "wtype",
        }
    }

    /// Send Ctrl+V, or Ctrl+Shift+V with `shift`. A helper that cannot reach
    /// its device still starts fine and only says so in its exit status, so
    /// that is what decides success.
    fn paste(self, shift: bool) -> Result<(), String> {
        let output = match self {
            PasteTool::Dotool => {
                let mut child = Command::new("dotool")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| e.to_string())?;
                let chord: &[u8] = if shift { b"key ctrl+shift+v\n" } else { b"key ctrl+v\n" };
                // Dropping stdin right after the write is the EOF that ends
                // dotool. A failed write means it already exited; its exit
                // status and stderr say why.
                let written = child.stdin.take().expect("piped").write_all(chord);
                let output = child.wait_with_output().map_err(|e| e.to_string())?;
                if output.status.success() {
                    written.map_err(|e| format!("writing to dotool: {e}"))?;
                }
                output
            }
            // Linux keycodes: 29 = LeftCtrl, 42 = LeftShift, 47 = V.
            PasteTool::Ydotool => {
                let keys: &[&str] = if shift {
                    &["key", "29:1", "42:1", "47:1", "47:0", "42:0", "29:0"]
                } else {
                    &["key", "29:1", "47:1", "47:0", "29:0"]
                };
                run(Command::new("ydotool").args(keys))?
            }
            PasteTool::Wtype => {
                let keys: &[&str] = if shift {
                    &["-M", "ctrl", "-M", "shift", "v", "-m", "shift", "-m", "ctrl"]
                } else {
                    &["-M", "ctrl", "v", "-m", "ctrl"]
                };
                run(Command::new("wtype").args(keys))?
            }
        };
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        match stderr.trim().lines().last() {
            Some(line) if !line.is_empty() => Err(format!("{}: {line}", output.status)),
            _ => Err(output.status.to_string()),
        }
    }

    /// Whether this helper can send keys in this session, beyond being
    /// installed; `Err` says what is missing, in words for the user.
    fn probe(self) -> Result<(), String> {
        match self {
            PasteTool::Dotool => std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/uinput")
                .map(drop)
                .map_err(|e| {
                    format!(
                        "cannot write /dev/uinput ({e}); install dotool's udev rule and add yourself to the input group"
                    )
                }),
            PasteTool::Ydotool => ydotool_socket().map(drop),
            PasteTool::Wtype => {
                if std::env::var_os("WAYLAND_DISPLAY").is_none() {
                    return Err("not a Wayland session".into());
                }
                let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_ascii_lowercase();
                if desktop.contains("kde") || desktop.contains("gnome") {
                    return Err("KDE Plasma and GNOME do not give ordinary apps a virtual keyboard".into());
                }
                Ok(())
            }
        }
    }
}

fn run(command: &mut Command) -> Result<Output, String> {
    command.stdin(Stdio::null()).output().map_err(|e| e.to_string())
}

/// The ydotoold socket the `ydotool` client would use, if a daemon is
/// listening on it.
fn ydotool_socket() -> Result<PathBuf, String> {
    let candidates: Vec<PathBuf> = match std::env::var_os("YDOTOOL_SOCKET") {
        Some(path) => vec![path.into()],
        None => {
            // ydotool 1.0 looks in the runtime directory, older releases
            // in /tmp.
            let runtime =
                std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join(".ydotool_socket"));
            runtime.into_iter().chain([PathBuf::from("/tmp/.ydotool_socket")]).collect()
        }
    };
    first_listening(&candidates)
}

/// Connecting a datagram socket sends nothing, but fails when no daemon is
/// bound to the path or when the socket belongs to root.
fn first_listening(candidates: &[PathBuf]) -> Result<PathBuf, String> {
    let mut problem = None;
    for path in candidates {
        let connected = UnixDatagram::unbound().and_then(|socket| socket.connect(path));
        match connected {
            Ok(()) => return Ok(path.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => problem = Some(format!("cannot reach ydotoold at {} ({e})", path.display())),
        }
    }
    Err(problem.unwrap_or_else(|| {
        let tried: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
        format!("ydotoold is not running (no socket at {})", tried.join(" or "))
    }))
}

fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

fn installed() -> Vec<PasteTool> {
    PasteTool::ALL.into_iter().filter(|t| on_path(t.binary())).collect()
}

/// The first installed helper.
pub fn detect_tool() -> Option<PasteTool> {
    installed().into_iter().next()
}

/// The first installed helper that can actually send keys here, or why none
/// can: ydotool needs its daemon, dotool needs write access to
/// `/dev/uinput`, and wtype does nothing on KDE or GNOME.
pub fn probe_tool() -> Result<&'static str, String> {
    let mut reasons = Vec::new();
    for tool in installed() {
        match tool.probe() {
            Ok(()) => return Ok(tool.binary()),
            Err(why) => reasons.push(format!("{}: {why}", tool.binary())),
        }
    }
    if reasons.is_empty() {
        return Err("no paste helper installed; install dotool or ydotool".into());
    }
    Err(reasons.join("; "))
}

pub struct ToolCascadeInjector {
    tools: Vec<PasteTool>,
}

impl Default for ToolCascadeInjector {
    fn default() -> Self {
        ToolCascadeInjector { tools: installed() }
    }
}

impl ToolCascadeInjector {
    pub fn describe(&self) -> String {
        match self.tools.as_slice() {
            [] => "clipboard only - install dotool or ydotool to paste automatically".into(),
            [only] => format!("clipboard + {}", only.binary()),
            [first, rest @ ..] => {
                let rest: Vec<&str> = rest.iter().map(|t| t.binary()).collect();
                format!("clipboard + {}, falling back to {}", first.binary(), rest.join(", then "))
            }
        }
    }
}

impl Injector for ToolCascadeInjector {
    fn insert(&self, text: &str) -> Result<(), DesktopError> {
        clipboard::paste_with(text, || {
            // Without a helper the text stays on the clipboard for a manual
            // paste; the island shows the transcript so nothing is lost.
            if self.tools.is_empty() {
                return Err(DesktopError::Unavailable(
                    "no paste helper installed (dotool, ydotool or wtype); text left on the clipboard".into(),
                ));
            }
            let shift = terminal::focused_is_terminal();
            let mut failures = Vec::new();
            for tool in &self.tools {
                match tool.paste(shift) {
                    Ok(()) => return Ok(()),
                    Err(why) => {
                        log::warn!("{} could not paste: {why}", tool.binary());
                        failures.push(format!("{}: {why}", tool.binary()));
                    }
                }
            }
            Err(DesktopError::Failed(format!(
                "no paste helper worked ({}); text left on the clipboard",
                failures.join("; ")
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_the_cascade() {
        let none = ToolCascadeInjector { tools: vec![] };
        assert!(none.describe().starts_with("clipboard only"));
        let one = ToolCascadeInjector { tools: vec![PasteTool::Ydotool] };
        assert_eq!(one.describe(), "clipboard + ydotool");
        let all = ToolCascadeInjector { tools: PasteTool::ALL.to_vec() };
        assert_eq!(all.describe(), "clipboard + dotool, falling back to ydotool, then wtype");
    }

    #[test]
    fn finds_a_listening_socket() {
        let dir = std::env::temp_dir().join(format!("rustle-ydotool-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("missing.sock");
        let stale = dir.join("stale.sock");
        let live = dir.join("live.sock");
        for path in [&stale, &live] {
            let _ = std::fs::remove_file(path);
        }
        // A socket file whose daemon has gone: bound, then closed.
        drop(UnixDatagram::bind(&stale).unwrap());
        let _daemon = UnixDatagram::bind(&live).unwrap();

        let err = first_listening(std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.contains("not running") && err.contains("missing.sock"), "{err}");
        let err = first_listening(&[missing.clone(), stale.clone()]).unwrap_err();
        assert!(err.contains("cannot reach") && err.contains("stale.sock"), "{err}");
        assert_eq!(first_listening(&[missing, stale, live.clone()]), Ok(live));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
