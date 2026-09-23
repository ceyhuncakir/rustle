//! The dictation shortcut on Linux desktops without Flow's GNOME extension:
//! the desktop portal's global shortcuts where it has them, else the
//! control socket that the compositor's own key bindings reach through
//! `flow hotkey`. What the wizard and the settings window say about it,
//! and the Grant button behind the "hotkey-wayland" row.

use std::sync::Arc;
use std::time::Duration;

use flow_desktop::linux::portal::{self, PortalState};
use flow_desktop::linux::{binding_help, control};
use flow_desktop::Session;
use tauri::{AppHandle, Manager};

use super::Shared;

/// How long Grant waits for the user to answer the desktop's dialog.
const DIALOG_WAIT: Duration = Duration::from_secs(180);

/// Said when someone tries to change a key the desktop owns.
pub const OWNED_BY_THE_DESKTOP: &str = "Your desktop owns this shortcut, so change it in the desktop's \
     settings: on GNOME, Settings → Apps → Flow; on KDE, System Settings → Keyboard → Shortcuts.";

/// The dictation key as the desktop reports it, while Flow holds it
/// through the portal.
pub fn portal_key() -> Option<String> {
    portal::state().dictate_trigger().map(portal::display_trigger)
}

/// The "hotkey-wayland" row: whether the shortcut works, and what to do.
pub fn permission(session: Session) -> (bool, String) {
    if portal::version().is_none() {
        let help = format!("{} Then press it once to check.", binding_help(session));
        return (control::heard(), help);
    }
    let state = portal::state();
    let help = match &state {
        _ if state.is_bound() => format!(
            "Your desktop delivers {} to Flow. {OWNED_BY_THE_DESKTOP}",
            portal_key().unwrap_or_default()
        ),
        PortalState::Binding => {
            "Your desktop is asking you to confirm Flow's shortcut; answer its dialog.".into()
        }
        PortalState::Declined => {
            "The desktop's dialog was closed without adding the shortcut. Click Grant to see it again.".into()
        }
        PortalState::Failed(why) => format!("Your desktop could not bind the shortcut: {why}"),
        _ => {
            "Your desktop hands out global shortcuts. Click Grant and add Flow's in the dialog that appears."
                .into()
        }
    };
    (state.is_bound(), help)
}

/// Grant: have the desktop bind the shortcut, showing its dialog, and wait
/// for the user's answer. Starts dictation if it is off, since the binding
/// belongs to the running engine.
pub fn request(app: &AppHandle) -> Result<(), String> {
    let session = flow_desktop::session::detect();
    if portal::version().is_none() {
        return Err(binding_help(session));
    }
    let shared = app.state::<Arc<Shared>>().inner().clone();
    let state = if portal::active() {
        portal::rebind()?
    } else {
        // Starting (or, when the portal failed, restarting) the engine
        // binds the shortcut, dialog and all.
        let started = if shared.running() {
            super::restart(&shared, Some(app))
        } else {
            super::start(&shared, Some(app))
        };
        started.map_err(|e| format!("{e:#}"))?;
        crate::tray::sync_toggle(app);
        portal::wait_while_binding(DIALOG_WAIT)
    };
    match state {
        s if s.is_bound() => Ok(()),
        PortalState::Binding => Err("Still waiting for you to answer the desktop's dialog.".into()),
        PortalState::Declined => Err("The shortcut was not added: the desktop's dialog was closed.".into()),
        PortalState::Failed(why) => Err(why),
        _ => Err("The desktop answered without the dictation shortcut.".into()),
    }
}
