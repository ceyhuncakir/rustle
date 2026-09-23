//! Wayland without the GNOME extension: KDE, wlroots compositors and the
//! rest.
//!
//! No compositor except wlroots exposes `zwp_virtual_keyboard_v1` to a plain
//! client, so the paste chord goes through whichever helper is installed:
//! `dotool` or `ydotool` (uinput, work everywhere), or `wtype` (wlroots
//! only). The clipboard itself is fine: `arboard` speaks wl-clipboard.

use std::io::Write;
use std::process::{Command, Stdio};

use flow_core::engine::{DesktopError, Injector};

use crate::clipboard;

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

    fn paste(self) -> std::io::Result<()> {
        match self {
            PasteTool::Dotool => {
                let mut child = Command::new("dotool").stdin(Stdio::piped()).spawn()?;
                child.stdin.take().expect("piped").write_all(b"key ctrl+v\n")?;
                child.wait()?;
            }
            // Linux keycodes: 29 = LeftCtrl, 47 = V.
            PasteTool::Ydotool => {
                Command::new("ydotool").args(["key", "29:1", "47:1", "47:0", "29:0"]).status()?;
            }
            PasteTool::Wtype => {
                Command::new("wtype").args(["-M", "ctrl", "v", "-m", "ctrl"]).status()?;
            }
        }
        Ok(())
    }
}

fn on_path(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file()))
        .unwrap_or(false)
}

/// The first installed helper.
pub fn detect_tool() -> Option<PasteTool> {
    PasteTool::ALL.into_iter().find(|t| on_path(t.binary()))
}

pub struct ToolCascadeInjector {
    tool: Option<PasteTool>,
}

impl Default for ToolCascadeInjector {
    fn default() -> Self {
        ToolCascadeInjector { tool: detect_tool() }
    }
}

impl ToolCascadeInjector {
    pub fn describe(&self) -> String {
        match self.tool {
            Some(tool) => format!("clipboard + {}", tool.binary()),
            None => "clipboard only - install dotool or ydotool to paste automatically".into(),
        }
    }
}

impl Injector for ToolCascadeInjector {
    fn insert(&self, text: &str) -> Result<(), DesktopError> {
        clipboard::paste_with(text, || {
            // Without a helper the text stays on the clipboard for a manual
            // paste; the island shows the transcript so nothing is lost.
            let tool = self.tool.ok_or_else(|| {
                DesktopError::Unavailable(
                    "no paste helper installed (dotool, ydotool or wtype); text left on the clipboard".into(),
                )
            })?;
            tool.paste().map_err(|e| DesktopError::Failed(format!("{}: {e}", tool.binary())))
        })
    }
}
