//! Clipboard-based text injection, shared by every backend that pastes.
//!
//! Put the text on the clipboard, send the paste chord, put the previous
//! contents back. Pasting is atomic and O(1) regardless of length, where
//! typing one key event per character is slow and mangles dead keys and
//! non-Latin layouts. Only `text/plain` survives the round trip.

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
    chord()?;

    if let Some(previous) = previous {
        let ours = text.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(RESTORE_DELAY);
            // Only restore while the clipboard still holds our text; if the
            // user copied something meanwhile, that wins.
            if let Ok(mut clipboard) = open() {
                if clipboard.get_text().ok().as_deref() == Some(ours.as_str()) {
                    let _ = clipboard.set_text(previous);
                }
            }
        });
    }
    Ok(())
}
