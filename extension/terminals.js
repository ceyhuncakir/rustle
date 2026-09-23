// Which windows paste with Ctrl+Shift+V instead of Ctrl+V.
//
// In a terminal Ctrl+V is a control character (^V, "insert the next key
// literally" in bash and zsh), so the ordinary chord pastes nothing and every
// Linux terminal moves paste to Ctrl+Shift+V.
//
// crates/flow-desktop/src/terminal.rs keeps the same list for X11 and the
// other Wayland desktops; change both together.

// Lower-case app ids and WM_CLASS names. A name matches whole or by its last
// dot-separated part, so 'org.gnome.Ptyxis', 'Ptyxis' and 'ptyxis' all match
// 'ptyxis'; ids whose last part is too generic ('org.gnome.Terminal') are
// listed in full.
const TERMINALS = new Set([
    'alacritty',
    'blackbox',
    'cool-retro-term',
    'deepin-terminal',
    'foot',
    'footclient',
    'ghostty',
    'gnome-terminal',
    'gnome-terminal-server',
    'guake',
    'io.elementary.terminal',
    'kgx',
    'kitty',
    'konsole',
    'lxterminal',
    'mate-terminal',
    'org.gnome.console',
    'org.gnome.terminal',
    'ptyxis',
    'qterminal',
    'rio',
    'st',
    'st-256color',
    'terminator',
    'terminology',
    'tilda',
    'tilix',
    'wezterm',
    'wezterm-gui',
    'xfce4-terminal',
    'yakuake',
]);

function matches(name) {
    const lower = (name ?? '').trim().toLowerCase();
    if (!lower)
        return false;
    return TERMINALS.has(lower) || TERMINALS.has(lower.split('.').pop());
}

/**
 * Whether `window` (a Meta.Window, or null) is a terminal. On Wayland the
 * WM class is the app id; on X11 it is WM_CLASS, whose instance part is
 * checked too.
 */
export function isTerminal(window) {
    if (!window)
        return false;
    return matches(window.get_wm_class()) || matches(window.get_wm_class_instance());
}
