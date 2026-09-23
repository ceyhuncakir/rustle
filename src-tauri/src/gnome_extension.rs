//! Flow's GNOME Shell extension, seen from the app: installing the copy the
//! Linux packages carry, and reading or changing its dictation shortcut.
//!
//! scripts/install-app.sh installs the extension itself; a deb, rpm or
//! AppImage cannot write into the user's home, so the wizard does it here.
//! GNOME on Wayland only loads a new extension at the next login.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use log::info;
use tauri::{AppHandle, Manager};

pub const UUID: &str = "flow@ceyhun.dev";
const SCHEMA: &str = "org.gnome.shell.extensions.flow";
const TOGGLE_KEY: &str = "toggle-dictation";

fn installed_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("gnome-shell/extensions").join(UUID))
}

/// The copy that ships with the app: bundled as a resource, or the source
/// tree for a development build.
fn bundled(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let resource = app.path().resource_dir().map(|dir| dir.join("gnome-extension"));
    if let Ok(dir) = &resource {
        if dir.join("metadata.json").exists() {
            return Ok(dir.clone());
        }
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension");
    if cfg!(debug_assertions) && source.join("metadata.json").exists() {
        return Ok(source);
    }
    bail!("this build of Flow does not carry the GNOME extension; install it with scripts/install-app.sh")
}

/// Copy the extension into the user's extensions folder, compile its
/// settings schema and mark it enabled. It runs after the next login.
pub fn install(app: &AppHandle) -> anyhow::Result<()> {
    let source = bundled(app)?;
    let target = installed_dir().context("no data folder to install the extension into")?;
    copy_dir(&source, &target).with_context(|| format!("copying the extension to {}", target.display()))?;

    let schemas = target.join("schemas");
    let compiled = Command::new("glib-compile-schemas")
        .arg(&schemas)
        .status()
        .context("glib-compile-schemas is missing; install your distribution's glib2 tools")?;
    if !compiled.success() {
        bail!("glib-compile-schemas could not compile {}", schemas.display());
    }

    enable()?;
    info!("installed the GNOME extension into {}", target.display());
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// `gnome-extensions enable` only knows extensions the running Shell has
/// loaded, which a freshly copied one is not; adding it to the enabled list
/// directly works either way.
fn enable() -> anyhow::Result<()> {
    let tried = Command::new("gnome-extensions").args(["enable", UUID]).output();
    if tried.is_ok_and(|out| out.status.success()) {
        return Ok(());
    }
    let out = Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .context("gsettings is missing")?;
    if !out.status.success() {
        bail!("could not read the enabled extensions: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut enabled = parse_strv(&String::from_utf8_lossy(&out.stdout));
    if enabled.iter().any(|uuid| uuid == UUID) {
        return Ok(());
    }
    enabled.push(UUID.to_string());
    let set = Command::new("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", &format_strv(&enabled)])
        .status()
        .context("gsettings is missing")?;
    if !set.success() {
        bail!("could not enable the extension");
    }
    Ok(())
}

fn gsettings(args: &[&str]) -> Option<std::process::Output> {
    let schemadir = installed_dir()?.join("schemas");
    let mut command = Command::new("gsettings");
    command.arg("--schemadir").arg(schemadir).args(args);
    command.output().ok()
}

/// The extension's dictation shortcut, in the shortcut plugin's notation.
pub fn binding() -> Option<String> {
    let out = gsettings(&["get", SCHEMA, TOGGLE_KEY])?;
    if !out.status.success() {
        return None;
    }
    let first = parse_strv(&String::from_utf8_lossy(&out.stdout)).into_iter().next()?;
    Some(from_accelerator(&first).unwrap_or(first))
}

/// Point the extension's dictation shortcut at `combo`, written in the
/// shortcut plugin's notation.
pub fn set_binding(combo: &str) -> anyhow::Result<()> {
    let accelerator =
        to_accelerator(combo).with_context(|| format!("{combo:?} cannot be a GNOME shortcut"))?;
    let value = format_strv(&[accelerator]);
    let out = gsettings(&["set", SCHEMA, TOGGLE_KEY, &value]).context("gsettings is missing")?;
    if !out.status.success() {
        bail!("could not change the extension's shortcut: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// `['<Super>d', 'x']` or `@as []`, as gsettings prints a string array.
fn parse_strv(text: &str) -> Vec<String> {
    text.trim()
        .trim_start_matches("@as")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| item.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn format_strv(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|item| format!("'{}'", item.replace('\'', "\\'"))).collect();
    format!("[{}]", quoted.join(", "))
}

/// Key names that differ between the shortcut plugin and X keysyms, which
/// is what GNOME accelerators use. Letters, digits and F-keys match apart
/// from case.
const KEYS: &[(&str, &str)] = &[
    ("Space", "space"),
    ("Enter", "Return"),
    ("Tab", "Tab"),
    ("Escape", "Escape"),
    ("Backspace", "BackSpace"),
    ("Delete", "Delete"),
    ("Insert", "Insert"),
    ("Home", "Home"),
    ("End", "End"),
    ("PageUp", "Page_Up"),
    ("PageDown", "Page_Down"),
    ("ArrowUp", "Up"),
    ("ArrowDown", "Down"),
    ("ArrowLeft", "Left"),
    ("ArrowRight", "Right"),
    ("Backquote", "grave"),
    ("Minus", "minus"),
    ("Equal", "equal"),
    ("BracketLeft", "bracketleft"),
    ("BracketRight", "bracketright"),
    ("Backslash", "backslash"),
    ("Semicolon", "semicolon"),
    ("Quote", "apostrophe"),
    ("Comma", "comma"),
    ("Period", "period"),
    ("Slash", "slash"),
    ("PrintScreen", "Print"),
    ("Pause", "Pause"),
    ("CapsLock", "Caps_Lock"),
    ("ScrollLock", "Scroll_Lock"),
    ("NumLock", "Num_Lock"),
    ("Numpad0", "KP_0"),
    ("Numpad1", "KP_1"),
    ("Numpad2", "KP_2"),
    ("Numpad3", "KP_3"),
    ("Numpad4", "KP_4"),
    ("Numpad5", "KP_5"),
    ("Numpad6", "KP_6"),
    ("Numpad7", "KP_7"),
    ("Numpad8", "KP_8"),
    ("Numpad9", "KP_9"),
    ("NumpadAdd", "KP_Add"),
    ("NumpadSubtract", "KP_Subtract"),
    ("NumpadMultiply", "KP_Multiply"),
    ("NumpadDivide", "KP_Divide"),
    ("NumpadDecimal", "KP_Decimal"),
    ("NumpadEnter", "KP_Enter"),
    ("NumpadEqual", "KP_Equal"),
];

/// "Ctrl+Alt+Space" -> "<Control><Alt>space".
fn to_accelerator(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, modifiers) = parts.split_last()?;
    let mut out = String::new();
    for modifier in modifiers {
        out.push_str(match modifier.to_ascii_uppercase().as_str() {
            "CTRL" | "CONTROL" | "CMDORCTRL" | "COMMANDORCONTROL" => "<Control>",
            "ALT" | "OPTION" => "<Alt>",
            "SHIFT" => "<Shift>",
            "SUPER" | "META" | "CMD" | "COMMAND" => "<Super>",
            _ => return None,
        });
    }
    let bare = key.strip_prefix("Key").filter(|k| k.len() == 1).or(key.strip_prefix("Digit")).unwrap_or(key);
    // The plugin also takes the short forms (Up, Esc), which are keysyms.
    let bare = if bare.eq_ignore_ascii_case("esc") { "Escape" } else { bare };
    let keysym = if let Some((_, sym)) =
        KEYS.iter().find(|(name, sym)| name.eq_ignore_ascii_case(bare) || sym.eq_ignore_ascii_case(bare))
    {
        (*sym).to_string()
    } else if bare.len() == 1 && bare.chars().all(|c| c.is_ascii_alphanumeric()) {
        bare.to_ascii_lowercase()
    } else if is_function_key(bare) {
        bare.to_ascii_uppercase()
    } else {
        return None;
    };
    Some(out + &keysym)
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n))
}

/// "<Super>d" -> "Super+D"; `None` for what the plugin cannot name.
fn from_accelerator(accelerator: &str) -> Option<String> {
    let mut rest = accelerator.trim();
    let mut parts = Vec::new();
    while let Some(stripped) = rest.strip_prefix('<') {
        let (modifier, after) = stripped.split_once('>')?;
        parts.push(match modifier.to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "primary" => "Ctrl",
            "alt" | "mod1" => "Alt",
            "shift" => "Shift",
            "super" | "meta" | "mod4" => "Super",
            _ => return None,
        });
        rest = after;
    }
    let key = if let Some((name, _)) = KEYS.iter().find(|(_, sym)| sym.eq_ignore_ascii_case(rest)) {
        (*name).to_string()
    } else if (rest.len() == 1 && rest.chars().all(|c| c.is_ascii_alphanumeric())) || is_function_key(rest) {
        rest.to_ascii_uppercase()
    } else {
        return None;
    };
    let mut combo = parts.join("+");
    if !combo.is_empty() {
        combo.push('+');
    }
    Some(combo + &key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_combos_become_gnome_accelerators() {
        assert_eq!(to_accelerator("Ctrl+Alt+Space").as_deref(), Some("<Control><Alt>space"));
        assert_eq!(to_accelerator("Super+D").as_deref(), Some("<Super>d"));
        assert_eq!(to_accelerator("Shift+KeyK").as_deref(), Some("<Shift>k"));
        assert_eq!(to_accelerator("Ctrl+Backquote").as_deref(), Some("<Control>grave"));
        assert_eq!(to_accelerator("Alt+F9").as_deref(), Some("<Alt>F9"));
        assert_eq!(to_accelerator("Ctrl+Digit1").as_deref(), Some("<Control>1"));
        assert_eq!(to_accelerator("Ctrl+Up").as_deref(), Some("<Control>Up"));
        assert_eq!(to_accelerator("Alt+Quote").as_deref(), Some("<Alt>apostrophe"));
        assert_eq!(to_accelerator("Ctrl+NumpadAdd").as_deref(), Some("<Control>KP_Add"));
        assert_eq!(to_accelerator("Super+Esc").as_deref(), Some("<Super>Escape"));
        assert_eq!(to_accelerator("Hyper+D"), None);
    }

    #[test]
    fn gnome_accelerators_become_plugin_combos() {
        assert_eq!(from_accelerator("<Super>d").as_deref(), Some("Super+D"));
        assert_eq!(from_accelerator("<Primary><Alt>space").as_deref(), Some("Ctrl+Alt+Space"));
        assert_eq!(from_accelerator("<Control>Page_Up").as_deref(), Some("Ctrl+PageUp"));
        assert_eq!(from_accelerator("<Super>XF86AudioMute"), None);
    }

    #[test]
    fn every_converted_combo_parses_in_the_plugin() {
        for combo in [
            "Super+D",
            "Ctrl+Alt+Space",
            "Ctrl+PageUp",
            "Alt+Quote",
            "Shift+F12",
            "Ctrl+Enter",
            "Ctrl+Numpad5",
            "Alt+Semicolon",
            "Ctrl+CapsLock",
        ] {
            let round = from_accelerator(&to_accelerator(combo).unwrap()).unwrap();
            assert!(round.parse::<tauri_plugin_global_shortcut::Shortcut>().is_ok(), "{round}");
        }
    }

    #[test]
    fn string_arrays_round_trip() {
        assert_eq!(parse_strv("['<Super>d']\n"), vec!["<Super>d"]);
        assert_eq!(parse_strv("@as []\n"), Vec::<String>::new());
        assert_eq!(parse_strv("['a@b', 'flow@ceyhun.dev']"), vec!["a@b", "flow@ceyhun.dev"]);
        assert_eq!(format_strv(&["a".into(), "b".into()]), "['a', 'b']");
    }
}
