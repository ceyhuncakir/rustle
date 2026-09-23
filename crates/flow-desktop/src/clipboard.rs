//! Clipboard-based text injection, shared by every backend that pastes.
//!
//! Put the text on the clipboard, send the paste chord, put the previous
//! contents back. Pasting is atomic and O(1) regardless of length, where
//! typing one key event per character is slow and mangles dead keys and
//! non-Latin layouts. Only `text/plain` survives the round trip.
//!
//! On X11 (and XWayland) the clipboard is not a store: the app that set it
//! must stay around to answer every paste. `arboard` does that from a
//! thread for as long as one of its handles is alive, and when the last one
//! drops it offers the text to a clipboard manager for at most 100 ms and
//! stops answering. So a handle is never dropped while the clipboard still
//! holds something of ours: it moves to a background thread that keeps
//! ownership until another app takes the clipboard.

use std::time::Duration;

use flow_core::engine::DesktopError;

/// Let the clipboard manager observe the new offer before pasting; pasting
/// in the same frame races the selection ownership change.
const PASTE_DELAY: Duration = Duration::from_millis(40);
/// How long the target app gets to read the clipboard before the previous
/// contents go back.
const RESTORE_DELAY: Duration = Duration::from_millis(400);

fn open() -> Result<arboard::Clipboard, DesktopError> {
    arboard::Clipboard::new().map_err(|e| DesktopError::Unavailable(format!("clipboard: {e}")))
}

/// Set the clipboard to `text`, run `chord` to paste it, and restore the
/// previous text afterwards. If `chord` fails the text stays on the
/// clipboard so nothing is lost.
pub(crate) fn paste_with(
    text: &str,
    chord: impl FnOnce() -> Result<(), DesktopError>,
) -> Result<(), DesktopError> {
    if text.is_empty() {
        return Ok(());
    }
    let mut clipboard = open()?;
    let previous = clipboard.get_text().ok().filter(|p| !p.is_empty());
    clipboard.set_text(text).map_err(|e| DesktopError::Failed(format!("clipboard: {e}")))?;

    std::thread::sleep(PASTE_DELAY);
    let pasted = chord();

    // After a failed chord there is nothing to restore: the dictation stays
    // on the clipboard for a manual paste.
    let previous = if pasted.is_ok() { previous } else { None };
    let ours = text.to_string();
    std::thread::spawn(move || settle(clipboard, ours, previous));
    pasted
}

/// Runs on its own thread with the handle that set `ours`: give the target
/// app time to read it, then put `previous` back, and own whatever the
/// clipboard ends up holding until another app replaces it.
fn settle(mut clipboard: arboard::Clipboard, ours: String, previous: Option<String>) {
    std::thread::sleep(RESTORE_DELAY);
    // Only touch the clipboard while it still holds our text; if the user
    // copied something meanwhile, that wins.
    if clipboard.get_text().ok().as_deref() != Some(ours.as_str()) {
        return;
    }
    match previous {
        Some(previous) => hold(&mut clipboard, previous),
        // Nothing to restore, so the dictation stays; only Linux needs us
        // around to keep serving it.
        None if cfg!(target_os = "linux") => hold(&mut clipboard, ours),
        None => {}
    }
}

/// Set the clipboard to `text` and, on Linux, block until another app
/// takes it, serving pastes meanwhile. Windows and macOS keep clipboard
/// contents themselves, so there it only sets.
fn hold(clipboard: &mut arboard::Clipboard, text: String) {
    #[cfg(target_os = "linux")]
    let set = {
        use arboard::SetExtLinux;
        clipboard.set().wait().text(text)
    };
    #[cfg(not(target_os = "linux"))]
    let set = clipboard.set_text(text);
    if let Err(err) = set {
        log::warn!("clipboard: {err}");
    }
}

/// Put `text` on the clipboard for the user to paste themselves. It stays
/// there after this returns: on Linux a background thread keeps ownership
/// until another app copies something. Failures are logged.
pub fn copy_text(text: String) {
    let copy = move || match open() {
        Ok(mut clipboard) => hold(&mut clipboard, text),
        Err(err) => log::warn!("{err}"),
    };
    if cfg!(target_os = "linux") {
        std::thread::spawn(copy);
    } else {
        copy();
    }
}
