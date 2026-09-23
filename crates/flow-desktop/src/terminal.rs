//! Which windows paste with Ctrl+Shift+V instead of Ctrl+V.
//!
//! In a terminal Ctrl+V is a control character (^V, "insert the next key
//! literally" in bash and zsh), so the ordinary chord pastes nothing and
//! every terminal on Linux moves paste to Ctrl+Shift+V. Windows and macOS
//! terminals accept Ctrl+V and Cmd+V, so only Linux asks.
//!
//! The Shell extension keeps the same list in `extension/terminals.js` for
//! GNOME; change both together.

/// Lower-case app ids, `WM_CLASS` names and process names. A name matches
/// whole or by its last dot-separated part, so `org.gnome.Ptyxis`, `Ptyxis`
/// and `ptyxis` all match `ptyxis`; ids whose last part is too generic
/// (`org.gnome.Terminal`) are listed in full.
const TERMINALS: &[&str] = &[
    "alacritty",
    "blackbox",
    "cool-retro-term",
    "deepin-terminal",
    "foot",
    "footclient",
    "ghostty",
    "gnome-terminal",
    "gnome-terminal-server",
    "guake",
    "io.elementary.terminal",
    "kgx",
    "kitty",
    "konsole",
    "lxterminal",
    "mate-terminal",
    "org.gnome.console",
    "org.gnome.terminal",
    "ptyxis",
    "qterminal",
    "rio",
    "st",
    "st-256color",
    "terminator",
    "terminology",
    "tilda",
    "tilix",
    "wezterm",
    "wezterm-gui",
    "xfce4-terminal",
    "yakuake",
];

/// Whether an app id, window class or process name is a terminal's.
pub(crate) fn is_terminal(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    let last = name.rsplit('.').next().unwrap_or(&name);
    TERMINALS.iter().any(|t| *t == name || *t == last)
}

/// Whether the focused window is a terminal, judged by its class (the app id
/// on Wayland) and its process name, since some terminals run under an
/// interpreter or a differently named server process.
pub(crate) fn focused_is_terminal() -> bool {
    let Ok(win) = active_win_pos_rs::get_active_window() else {
        return false;
    };
    let process = win.process_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    is_terminal(&win.app_name) || is_terminal(&process)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_ids_classes_and_processes() {
        for name in [
            "org.gnome.Ptyxis",
            "Ptyxis",
            "org.gnome.Terminal",
            "Gnome-terminal",
            "gnome-terminal-server",
            "org.gnome.Console",
            "org.kde.konsole",
            "kitty",
            "Alacritty",
            "foot",
            "org.wezfurlong.wezterm",
            "wezterm-gui",
            "com.mitchellh.ghostty",
            "com.gexperts.Tilix",
            "Terminator",
            " xfce4-terminal ",
        ] {
            assert!(is_terminal(name), "{name} should be a terminal");
        }
    }

    #[test]
    fn leaves_other_apps_alone() {
        for name in
            ["", "firefox", "org.gnome.TextEditor", "code", "org.gnome.Settings", "python3.12", "steam"]
        {
            assert!(!is_terminal(name), "{name} should not be a terminal");
        }
    }
}
