//! Global shortcuts through the desktop portal
//! (`org.freedesktop.portal.GlobalShortcuts`): KDE Plasma, Hyprland, GNOME
//! without Rustle's extension, and any other desktop whose portal offers it.
//!
//! Rustle asks for two shortcuts, `dictate` and `cancel`, suggesting the
//! configured keys. The desktop decides: the first time it shows a dialog
//! where the user accepts or changes them, and after that it hands back
//! what was chosen. That answer is kept in [`state`] so the app can show the
//! real key. `dictate` reports press and release (Activated, Deactivated),
//! which HoldOrTap turns into hold-to-talk or tap-to-toggle.
//!
//! Every call is the portal's Request/Response dance: subscribe to the
//! Response signal on the request's predictable path, make the call, wait.
//! All of it runs on one worker thread per session, because the wait for a
//! dialog is as long as the user takes.
//!
//! A program outside Flatpak is known to the portal by the app id it
//! registers, which the portal only accepts when a desktop entry of that
//! name is installed; see [`register`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use rustle_core::engine::{DesktopError, Event, Hotkey, HotkeyEvent};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

const DESKTOP: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";
const REQUEST: &str = "org.freedesktop.portal.Request";
const SESSION: &str = "org.freedesktop.portal.Session";
/// Where the portal puts its session objects.
const SESSIONS: &str = "/org/freedesktop/portal/desktop/session";
const REGISTRY: &str = "org.freedesktop.host.portal.Registry";

/// Who Rustle is to the portal: the bundle identifier, and the name of the
/// desktop entry the installers put in place.
pub const APP_ID: &str = "dev.ceyhun.Rustle";
pub const DICTATE: &str = "dictate";
pub const CANCEL: &str = "cancel";
/// The cancel key the GNOME extension uses too. Ctrl+Alt+Escape would be
/// the obvious one, but KDE kills a window with it and GNOME switches
/// system controls.
const CANCEL_TRIGGER: &str = "LOGO+CTRL+Escape";

/// Plain calls only; a wait for the user's answer is not bounded.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// GNOME repeats Activated while a key is held: first after the keyboard's
/// repeat delay (500 ms by default, up to 2 s in Settings), then every
/// 30 ms or so. Anything closer than this to the last one is a repeat; a
/// fresh press after a lost release comes later than that.
const REPEAT_GAP: Duration = Duration::from_millis(2500);
/// How long a newly written desktop entry may take to reach the portal.
const ENTRY_WAIT: Duration = Duration::from_secs(3);
/// After the desktop dropped the session, how long to wait before asking
/// again, so a portal that is restarting has come back.
const RESTART_GRACE: Duration = Duration::from_secs(1);

/// The GlobalShortcuts version the desktop's portal offers, or `None` when
/// it offers none (xdg-desktop-portal-wlr, GNOME before 48, no portal).
/// Cheap and silent: no session, no dialog. The settings window's status
/// poll asks every few seconds, so the answer is kept for a while rather
/// than opening a bus connection each time.
pub fn version() -> Option<u32> {
    const KEEP: Duration = Duration::from_secs(30);
    static ASKED: Mutex<Option<(Instant, Option<u32>)>> = Mutex::new(None);
    if let Some((at, version)) = *lock(&ASKED) {
        if at.elapsed() < KEEP {
            return version;
        }
    }
    let version = ask_version();
    *lock(&ASKED) = Some((Instant::now(), version));
    version
}

fn ask_version() -> Option<u32> {
    let conn =
        zbus::blocking::connection::Builder::session().ok()?.method_timeout(CALL_TIMEOUT).build().ok()?;
    let reply = conn
        .call_method(
            Some(DESKTOP),
            PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(SHORTCUTS, "version"),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    u32::try_from(value).ok()
}

// -- what the desktop said -------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub id: String,
    /// The desktop's own description of the key, e.g. "Ctrl+Alt+Space" on
    /// KDE, "Press <Control><Alt>space" on GNOME. See [`display_trigger`].
    pub trigger: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PortalState {
    /// No session: no engine is running on the portal in this process.
    #[default]
    Off,
    /// Waiting for the desktop, whose dialog may be on screen.
    Binding,
    /// The shortcuts the desktop delivers.
    Bound(Vec<Bound>),
    /// The user closed or refused the desktop's dialog, or the desktop
    /// would not have Rustle.
    Declined,
    Failed(String),
}

impl PortalState {
    /// The dictation key as the desktop describes it, once it has one.
    pub fn dictate_trigger(&self) -> Option<&str> {
        match self {
            PortalState::Bound(list) => list.iter().find(|b| b.id == DICTATE).map(|b| b.trigger.as_str()),
            _ => None,
        }
    }

    /// Whether the desktop delivers the dictation shortcut to Rustle.
    pub fn is_bound(&self) -> bool {
        matches!(self, PortalState::Bound(list) if list.iter().any(|b| b.id == DICTATE))
    }
}

static STATE: Mutex<PortalState> = Mutex::new(PortalState::Off);
static STATE_CHANGED: Condvar = Condvar::new();
/// The live session, for [`rebind`]. One engine per process, so one session.
static ACTIVE: Mutex<Option<Arc<Session>>> = Mutex::new(None);
/// Held while a BindShortcuts call waits, so two never race to the user.
static BINDING: Mutex<()> = Mutex::new(());

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Where the shortcuts stand in this process.
pub fn state() -> PortalState {
    lock(&STATE).clone()
}

/// Whether an engine in this process listens on the portal.
pub fn active() -> bool {
    lock(&ACTIVE).is_some()
}

/// Wait while the desktop's dialog is up, for at most `timeout`.
pub fn wait_while_binding(timeout: Duration) -> PortalState {
    let state = lock(&STATE);
    let (state, _) = STATE_CHANGED
        .wait_timeout_while(state, timeout, |s| *s == PortalState::Binding)
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.clone()
}

/// Record what the desktop said about `session`, unless that session has
/// been closed since: a late answer must not overwrite a newer state.
fn publish(session: &Arc<Session>, state: PortalState) {
    let active = lock(&ACTIVE);
    if active.as_ref().is_some_and(|a| Arc::ptr_eq(a, session)) {
        *lock(&STATE) = state;
        STATE_CHANGED.notify_all();
    }
}

fn set_state(state: PortalState) {
    *lock(&STATE) = state;
    STATE_CHANGED.notify_all();
}

/// Ask the desktop for the shortcuts again, after its dialog was closed or
/// the binding failed, and wait for the user's answer. Nothing to do once
/// bound: the desktop's settings own the key from then on. When a request
/// is already on screen, waits for that one instead of stacking a second.
pub fn rebind() -> Result<PortalState, String> {
    let current = lock(&ACTIVE).clone().ok_or_else(|| "Rustle is not listening on the portal".to_string())?;
    let _one_at_a_time = match BINDING.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            drop(lock(&BINDING));
            return Ok(state());
        }
    };
    if state().is_bound() {
        return Ok(state());
    }
    // A session binds once (GNOME answers a second try with nothing), so
    // ask in a fresh one on the same connection and let the old one go.
    let handle =
        create_session(&current.conn).map_err(|e| format!("could not open a shortcuts session: {e}"))?;
    let fresh = Arc::new(Session { conn: current.conn.clone(), handle, wanted: current.wanted.clone() });
    {
        let mut active = lock(&ACTIVE);
        if !active.as_ref().is_some_and(|a| Arc::ptr_eq(a, &current)) {
            // Stopped meanwhile.
            return Ok(state());
        }
        *active = Some(fresh.clone());
    }
    close_session(&current);
    bind_now(&fresh);
    Ok(state())
}

// -- the hotkey --------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Wanted {
    id: &'static str,
    description: &'static str,
    trigger: Option<String>,
}

struct Session {
    conn: Connection,
    handle: OwnedObjectPath,
    wanted: Vec<Wanted>,
}

/// The engine's hotkey on a desktop whose portal offers global shortcuts.
pub struct PortalHotkey {
    wanted: Vec<Wanted>,
    /// The connection the worker is on. It starts over on a new one when
    /// the desktop drops the session, so [`Hotkey::stop`] closes whichever
    /// is current.
    conn: Arc<Mutex<Option<Connection>>>,
    stopped: Arc<AtomicBool>,
}

impl PortalHotkey {
    /// `hotkey` is the configured combination ("Ctrl+Alt+Space"), offered
    /// to the desktop as the suggestion for the dictation shortcut.
    pub fn new(hotkey: &str) -> PortalHotkey {
        let trigger = portal_trigger(hotkey);
        if trigger.is_none() {
            warn!(
                "{hotkey:?} has no name in the portal's notation; the desktop will ask without a suggestion"
            );
        }
        let wanted = vec![
            Wanted { id: DICTATE, description: "Dictate (hold to talk, tap to toggle)", trigger },
            Wanted { id: CANCEL, description: "Cancel dictation", trigger: Some(CANCEL_TRIGGER.into()) },
        ];
        PortalHotkey { wanted, conn: Arc::default(), stopped: Arc::default() }
    }
}

/// A connection of its own: the app id is registered per connection, and
/// must be before any other portal call on it.
fn connect() -> zbus::Result<Connection> {
    zbus::blocking::connection::Builder::session()?.method_timeout(CALL_TIMEOUT).build()
}

impl Hotkey for PortalHotkey {
    fn start(&mut self, sink: Sender<Event>) -> Result<(), DesktopError> {
        let conn = connect().map_err(|e| DesktopError::Unavailable(format!("session bus: {e}")))?;
        // A flag per start: a worker from before a stop, still waiting to
        // start over, must see its own stop and not take this one's place.
        self.stopped = Arc::default();
        set_state(PortalState::Binding);
        *lock(&self.conn) = Some(conn.clone());
        let (wanted, stopped, slot) = (self.wanted.clone(), self.stopped.clone(), self.conn.clone());
        std::thread::Builder::new()
            .name("rustle-portal".into())
            .spawn(move || run(conn, wanted, sink, stopped, slot))
            .map_err(|e| DesktopError::Unavailable(format!("portal thread: {e}")))?;
        Ok(())
    }

    fn stop(&mut self) {
        // Under the slot's lock, so a worker starting over cannot put a new
        // connection in after this took the old one.
        let conn = {
            let mut slot = lock(&self.conn);
            self.stopped.store(true, Ordering::SeqCst);
            slot.take()
        };
        let Some(conn) = conn else { return };
        lock(&ACTIVE).take();
        set_state(PortalState::Off);
        // Closing the connection ends the session (the portal drops the
        // shortcuts of a peer that leaves), any dialog still waiting, and
        // the worker's signal loop with it.
        if let Err(err) = conn.close() {
            debug!("closing the portal connection: {err}");
        }
    }
}

impl Drop for PortalHotkey {
    fn drop(&mut self) {
        Hotkey::stop(self);
    }
}

/// The worker: register, open a session, bind, then relay key events until
/// the connection closes. The desktop can drop the shortcuts without a
/// word, by closing the session or by its portal leaving the bus (a
/// restart, an upgrade, a crash); [`watch`] notices, closes the connection,
/// and the worker starts over on a new one, as at startup.
fn run(
    mut conn: Connection,
    wanted: Vec<Wanted>,
    sink: Sender<Event>,
    stopped: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Connection>>>,
) {
    while let Served::Lost = serve(&conn, &wanted, &sink, &stopped) {
        // Give a restarting portal a moment; asking starts it again if
        // nothing else does.
        std::thread::sleep(RESTART_GRACE);
        conn = match connect() {
            Ok(conn) => conn,
            Err(err) => {
                return fail_unless_stopped(&stopped, format!("could not reach the portal again: {err}"))
            }
        };
        let mut current = lock(&slot);
        if stopped.load(Ordering::SeqCst) {
            drop(current);
            let _ = conn.close();
            return;
        }
        *current = Some(conn.clone());
    }
    debug!("portal listener ended");
}

enum Served {
    /// The desktop dropped the session; start over.
    Lost,
    /// Stopped, or the portal could not be used at all.
    Over,
}

/// One connection's worth of [`run`].
fn serve(conn: &Connection, wanted: &[Wanted], sink: &Sender<Event>, stopped: &AtomicBool) -> Served {
    register(conn);
    let signals = match subscribe(conn) {
        Ok(signals) => signals,
        Err(err) => {
            fail_unless_stopped(stopped, format!("could not listen to the portal: {err}"));
            return Served::Over;
        }
    };
    let handle = match create_session(conn) {
        Ok(handle) => handle,
        Err(err) => {
            fail_unless_stopped(stopped, format!("could not open a shortcuts session: {err}"));
            return Served::Over;
        }
    };
    let session = Arc::new(Session { conn: conn.clone(), handle, wanted: wanted.to_vec() });
    {
        let mut active = lock(&ACTIVE);
        if stopped.load(Ordering::SeqCst) {
            return Served::Over;
        }
        *active = Some(session.clone());
    }
    info!("portal shortcuts session {}", session.handle.as_str());
    let lost = Arc::new(AtomicBool::new(false));
    watch(conn, &lost);
    bind(&session);
    listen(signals, sink, stopped);
    if lost.load(Ordering::SeqCst) && !stopped.load(Ordering::SeqCst) {
        Served::Lost
    } else {
        Served::Over
    }
}

/// Watch for the desktop dropping the session behind Rustle's back: the
/// portal closing it, or leaving the bus and taking every session with it.
/// Either marks `lost` and closes the connection, which ends the listener
/// and any request still waiting for an answer that will never come. Both
/// watchers end with the connection.
fn watch(conn: &Connection, lost: &Arc<AtomicBool>) {
    match subscribe_closed(conn) {
        Ok(closed) => {
            let (conn, lost) = (conn.clone(), lost.clone());
            spawn_watcher("rustle-portal-session", move || {
                for message in closed {
                    let Ok(message) = message else { continue };
                    let Some(path) = message.header().path().map(|p| p.to_string()) else { continue };
                    if is_current(&path) {
                        return give_up(conn, &lost, "the desktop closed the shortcuts session");
                    }
                }
            });
        }
        Err(err) => debug!("cannot watch the portal session: {err}"),
    }
    let owner = match portal_owner(conn) {
        Ok(owner) => owner,
        Err(err) => return debug!("cannot tell which process is the portal: {err}"),
    };
    match subscribe_owner(conn) {
        Ok(changes) => {
            let (conn, lost) = (conn.clone(), lost.clone());
            spawn_watcher("rustle-portal-owner", move || {
                for message in changes {
                    let Ok(message) = message else { continue };
                    let Ok((_, old, _)) = message.body().deserialize::<(String, String, String)>() else {
                        continue;
                    };
                    if portal_left(&owner, &old) {
                        return give_up(conn, &lost, "the desktop portal left the session bus");
                    }
                }
            });
        }
        Err(err) => debug!("cannot watch the portal: {err}"),
    }
}

fn spawn_watcher(name: &str, watch: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new().name(name.into()).spawn(watch) {
        warn!("could not watch the portal: {err}");
    }
}

/// Whether `path` is the live session's.
fn is_current(path: &str) -> bool {
    lock(&ACTIVE).as_ref().is_some_and(|a| a.handle.as_str() == path)
}

/// Whether a NameOwnerChanged whose old owner is `old` is the portal Rustle
/// talks to leaving: gone, or replaced by a new one that knows none of its
/// sessions. The portal it starts by asking again appears with no old owner.
fn portal_left(ours: &str, old: &str) -> bool {
    !ours.is_empty() && old == ours
}

fn give_up(conn: Connection, lost: &AtomicBool, why: &str) {
    warn!("{why}; asking for the shortcuts again");
    lost.store(true, Ordering::SeqCst);
    if let Some(session) = lock(&ACTIVE).clone() {
        publish(&session, PortalState::Failed(format!("{why}; asking again")));
    }
    if let Err(err) = conn.close() {
        debug!("closing the portal connection: {err}");
    }
}

fn fail_unless_stopped(stopped: &AtomicBool, message: String) {
    if !stopped.load(Ordering::SeqCst) {
        warn!("{message}");
        set_state(PortalState::Failed(message));
    }
}

fn bind(session: &Arc<Session>) {
    let _one_at_a_time = lock(&BINDING);
    bind_now(session);
}

/// Bind, with [`BINDING`] already held.
fn bind_now(session: &Arc<Session>) {
    publish(session, PortalState::Binding);
    let state = match bind_shortcuts(&session.conn, &session.handle, &session.wanted) {
        Ok(bound) => {
            for b in &bound {
                info!("portal shortcut {}: {}", b.id, b.trigger);
            }
            PortalState::Bound(bound)
        }
        Err(PortalError::Declined) => PortalState::Declined,
        Err(err) => PortalState::Failed(err.to_string()),
    };
    publish(session, state);
}

/// The live session, if `handle` is it. [`rebind`] can swap sessions under
/// a running listener.
fn current(handle: &OwnedObjectPath) -> Option<Arc<Session>> {
    lock(&ACTIVE).as_ref().filter(|a| a.handle == *handle).cloned()
}

fn listen(signals: MessageIterator, sink: &Sender<Event>, stopped: &AtomicBool) {
    let mut keys = Keys::default();
    for message in signals {
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let Ok(message) = message else { continue };
        let header = message.header();
        let Some(member) = header.member().map(|m| m.to_string()) else { continue };
        let body = message.body();
        let event = match member.as_str() {
            "Activated" | "Deactivated" => {
                let Ok((handle, id, _, _)) = body.deserialize::<(OwnedObjectPath, String, u64, Options)>()
                else {
                    continue;
                };
                if current(&handle).is_none() {
                    continue;
                }
                let now = Instant::now();
                if member == "Activated" {
                    keys.activated(&id, now)
                } else {
                    keys.deactivated(&id)
                }
            }
            "ShortcutsChanged" => {
                if let Ok((handle, list)) = body.deserialize::<(OwnedObjectPath, Vec<(String, Options)>)>() {
                    if let Some(session) = current(&handle) {
                        publish(&session, PortalState::Bound(bound_from(list)));
                    }
                }
                None
            }
            _ => None,
        };
        if let Some(event) = event {
            if sink.send(Event::Hotkey(event)).is_err() {
                break;
            }
        }
    }
}

/// Turns the portal's activations into engine events. Only the first of a
/// run of repeated Activated counts, or HoldOrTap would read the repeats of
/// a held key as taps.
#[derive(Debug, Default)]
struct Keys {
    /// When `dictate` was last activated, while it is down.
    held: Option<Instant>,
}

impl Keys {
    fn activated(&mut self, id: &str, now: Instant) -> Option<HotkeyEvent> {
        match id {
            DICTATE => {
                let fresh = self.held.is_none_or(|last| now.duration_since(last) >= REPEAT_GAP);
                self.held = Some(now);
                fresh.then_some(HotkeyEvent::Down)
            }
            CANCEL => Some(HotkeyEvent::Cancel),
            _ => None,
        }
    }

    fn deactivated(&mut self, id: &str) -> Option<HotkeyEvent> {
        (id == DICTATE && self.held.take().is_some()).then_some(HotkeyEvent::Up)
    }
}

// -- D-Bus -------------------------------------------------------------------------------

type Options = HashMap<String, OwnedValue>;

#[derive(Debug, thiserror::Error)]
enum PortalError {
    #[error(transparent)]
    Bus(#[from] zbus::Error),
    #[error("the desktop's dialog was closed or refused")]
    Declined,
    #[error("the connection to the portal closed")]
    Closed,
    #[error("the portal's answer had no {0}")]
    Missing(&'static str),
}

/// Tell the portal who we are. Outside Flatpak it otherwise guesses from
/// the systemd unit, which for Rustle gives nothing it accepts. Portals
/// before 1.19 have no registry and keep guessing; that is not an error.
fn register(conn: &Connection) {
    match call_register(conn) {
        Ok(()) => debug!("registered with the portal as {APP_ID}"),
        Err(err) if missing_entry(&err) && write_desktop_entry() => {
            // The portal notices the new file after a moment.
            let deadline = Instant::now() + ENTRY_WAIT;
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(200));
                if call_register(conn).is_ok() {
                    return debug!("registered with the portal as {APP_ID}");
                }
            }
            warn!("the portal still does not know {APP_ID}; the desktop may refuse the shortcut");
        }
        Err(err) => debug!("portal registry: {err}"),
    }
}

fn call_register(conn: &Connection) -> zbus::Result<()> {
    let options: HashMap<&str, Value> = HashMap::new();
    conn.call_method(Some(DESKTOP), PATH, Some(REGISTRY), "Register", &(APP_ID, options))?;
    Ok(())
}

/// "App info not found": no desktop entry by that name is installed.
fn missing_entry(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::MethodError(_, Some(text), _) if text.contains("not found"))
}

/// Install the desktop entry the portal wants, hidden from menus, when the
/// way Rustle was installed brought none by that name: the .deb and .rpm
/// carry `Rustle.desktop`, an AppImage or a build tree carry nothing. Returns
/// whether one was written.
fn write_desktop_entry() -> bool {
    let Some(dir) = std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")))
        .map(|d| d.join("applications"))
    else {
        return false;
    };
    let path = dir.join(format!("{APP_ID}.desktop"));
    if path.exists() {
        return false;
    }
    // An AppImage runs from a mount that is gone next time; $APPIMAGE is
    // the file itself.
    let exe =
        std::env::var_os("APPIMAGE").map(std::path::PathBuf::from).or_else(|| std::env::current_exe().ok());
    let exec = exe.map(|p| format!("\"{}\"", p.display())).unwrap_or_else(|| "rustle".into());
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Rustle\nComment=Local offline dictation\n\
         Exec={exec}\nIcon=rustle\nNoDisplay=true\n"
    );
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, entry)) {
        Ok(()) => {
            info!("wrote {} so the desktop portal can tell who Rustle is", path.display());
            true
        }
        Err(err) => {
            warn!("could not write {}: {err}", path.display());
            false
        }
    }
}

/// Every signal of the GlobalShortcuts interface. Filtered by session later.
fn subscribe(conn: &Connection) -> zbus::Result<MessageIterator> {
    let rule =
        MatchRule::builder().msg_type(zbus::message::Type::Signal).interface(SHORTCUTS)?.path(PATH)?.build();
    MessageIterator::for_match_rule(rule, conn, Some(64))
}

/// Closed, on any session; [`watch`] picks out the live one.
fn subscribe_closed(conn: &Connection) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(SESSION)?
        .member("Closed")?
        .path_namespace(SESSIONS)?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(8))
}

/// The portal's bus name changing hands.
fn subscribe_owner(conn: &Connection) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")?
        .interface("org.freedesktop.DBus")?
        .member("NameOwnerChanged")?
        .arg(0, DESKTOP)?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(8))
}

/// The unique name of the portal process this connection talks to.
fn portal_owner(conn: &Connection) -> zbus::Result<String> {
    let dbus = zbus::blocking::fdo::DBusProxy::new(conn)?;
    let owner = dbus.get_name_owner(zbus::names::BusName::try_from(DESKTOP)?)?;
    Ok(owner.to_string())
}

fn token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("rustle_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst))
}

/// The path the portal will give the request made with `token`.
fn request_path(conn: &Connection, token: &str) -> Result<String, PortalError> {
    let unique = conn.unique_name().ok_or(PortalError::Missing("unique bus name"))?;
    let sender = unique.as_str().trim_start_matches(':').replace('.', "_");
    Ok(format!("{PATH}/request/{sender}/{token}"))
}

fn responses(conn: &Connection, path: &str) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface(REQUEST)?
        .member("Response")?
        .path(path)?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(1))
}

/// Make a portal call with `call` and wait for its Response.
fn request(
    conn: &Connection,
    token: &str,
    call: impl FnOnce() -> zbus::Result<zbus::Message>,
) -> Result<Options, PortalError> {
    let expected = request_path(conn, token)?;
    // Subscribed before the call, so a quick answer cannot be missed.
    let mut answers = responses(conn, &expected)?;
    let handle: OwnedObjectPath = call()?.body().deserialize()?;
    if handle.as_str() != expected {
        // Portals before 0.9 chose their own path.
        answers = responses(conn, handle.as_str())?;
    }
    let message = answers.next().ok_or(PortalError::Closed)??;
    let (code, results): (u32, Options) = message.body().deserialize()?;
    debug!("portal answered {code} to request {token}");
    // GNOME answers a closed dialog with "ended otherwise" (2), not
    // "cancelled" (1), so both mean no. But xdg-desktop-portal-gnome 48
    // never sets the code of a successful bind (it arrives as garbage, 2
    // here), and only a success carries the shortcuts.
    match code {
        0 => Ok(results),
        _ if results.contains_key("shortcuts") => Ok(results),
        _ => Err(PortalError::Declined),
    }
}

fn create_session(conn: &Connection) -> Result<OwnedObjectPath, PortalError> {
    let token = token();
    let options: HashMap<&str, Value> = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("session_handle_token", Value::from(token.as_str())),
    ]);
    let results = request(conn, &token, || {
        conn.call_method(Some(DESKTOP), PATH, Some(SHORTCUTS), "CreateSession", &options)
    })?;
    // A string by the spec; some portals send an object path.
    let handle = results.get("session_handle").ok_or(PortalError::Missing("session_handle"))?;
    let path = match &**handle {
        Value::Str(s) => ObjectPath::try_from(s.as_str()).map(|p| p.into()),
        Value::ObjectPath(p) => Ok(p.clone().into()),
        _ => return Err(PortalError::Missing("session_handle")),
    };
    path.map_err(|e| PortalError::Bus(e.into()))
}

/// Let a session go; its shortcuts go with it.
fn close_session(session: &Session) {
    let closed = session.conn.call_method(
        Some(DESKTOP),
        session.handle.as_str(),
        Some("org.freedesktop.portal.Session"),
        "Close",
        &(),
    );
    if let Err(err) = closed {
        debug!("closing portal session {}: {err}", session.handle.as_str());
    }
}

fn bind_shortcuts(
    conn: &Connection,
    session: &ObjectPath<'_>,
    wanted: &[Wanted],
) -> Result<Vec<Bound>, PortalError> {
    let token = token();
    let shortcuts: Vec<(&str, HashMap<&str, Value>)> = wanted
        .iter()
        .map(|w| {
            let mut props = HashMap::from([("description", Value::from(w.description))]);
            if let Some(trigger) = &w.trigger {
                props.insert("preferred_trigger", Value::from(trigger.as_str()));
            }
            (w.id, props)
        })
        .collect();
    let options: HashMap<&str, Value> = HashMap::from([("handle_token", Value::from(token.as_str()))]);
    // No parent window: the dialog is the desktop's own.
    let results = request(conn, &token, || {
        conn.call_method(
            Some(DESKTOP),
            PATH,
            Some(SHORTCUTS),
            "BindShortcuts",
            &(session, shortcuts, "", options),
        )
    })?;
    let list = match results.get("shortcuts") {
        Some(value) => Vec::<(String, Options)>::try_from(value.try_clone().map_err(zbus::Error::from)?)
            .map_err(zbus::Error::from)?,
        None => Vec::new(),
    };
    Ok(bound_from(list))
}

fn bound_from(list: Vec<(String, Options)>) -> Vec<Bound> {
    list.into_iter()
        .map(|(id, props)| {
            let trigger = props
                .get("trigger_description")
                .and_then(|v| <&str>::try_from(v).ok())
                .unwrap_or_default()
                .to_string();
            Bound { id, trigger }
        })
        .collect()
}

// -- key names --------------------------------------------------------------------------

/// "Ctrl+Alt+Space", as config.toml and the shortcut recorder spell it, in
/// the XDG shortcuts notation: "CTRL+ALT+space" (modifier names, then an
/// xkb keysym name). `None` for a part it has no name for.
pub fn portal_trigger(combo: &str) -> Option<String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let (key, modifiers) = parts.split_last()?;
    let mut out = Vec::with_capacity(parts.len());
    for modifier in modifiers {
        out.push(match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmdorctrl" | "commandorcontrol" => "CTRL",
            "alt" | "option" => "ALT",
            "shift" => "SHIFT",
            "super" | "meta" | "cmd" | "command" | "logo" | "win" => "LOGO",
            _ => return None,
        });
    }
    let key = keysym(key)?;
    out.push(&key);
    Some(out.join("+"))
}

/// The xkb keysym name for a key as the shortcut recorder names it.
fn keysym(key: &str) -> Option<String> {
    let bare =
        key.strip_prefix("Key").or_else(|| key.strip_prefix("Digit")).filter(|k| k.len() == 1).unwrap_or(key);
    if bare.len() == 1 && bare.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Some(bare.to_ascii_lowercase());
    }
    let lower = key.to_ascii_lowercase();
    if let Some(n) =
        lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()).filter(|n| (1..=24).contains(n))
    {
        return Some(format!("F{n}"));
    }
    if let Some(digit) =
        lower.strip_prefix("numpad").filter(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(format!("KP_{digit}"));
    }
    let name = match lower.as_str() {
        "space" => "space",
        "enter" | "return" => "Return",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "escape" | "esc" => "Escape",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "up" | "arrowup" => "Up",
        "down" | "arrowdown" => "Down",
        "left" | "arrowleft" => "Left",
        "right" | "arrowright" => "Right",
        "capslock" => "Caps_Lock",
        "numlock" => "Num_Lock",
        "scrolllock" => "Scroll_Lock",
        "pause" => "Pause",
        "printscreen" => "Print",
        "minus" => "minus",
        "equal" => "equal",
        "bracketleft" => "bracketleft",
        "bracketright" => "bracketright",
        "semicolon" => "semicolon",
        "quote" => "apostrophe",
        "backquote" => "grave",
        "backslash" => "backslash",
        "comma" => "comma",
        "period" => "period",
        "slash" => "slash",
        "numpadadd" => "KP_Add",
        "numpadsubtract" => "KP_Subtract",
        "numpadmultiply" => "KP_Multiply",
        "numpaddivide" => "KP_Divide",
        "numpaddecimal" => "KP_Decimal",
        "numpadenter" => "KP_Enter",
        "numpadequal" => "KP_Equal",
        _ => return None,
    };
    Some(name.to_string())
}

/// A desktop's description of a key, as the app shows combinations:
/// "Press <Control><Alt>space" (GNOME) becomes "Ctrl+Alt+Space". Anything
/// not in GTK's accelerator notation (KDE already says "Ctrl+Alt+Space") is
/// returned as it is.
pub fn display_trigger(description: &str) -> String {
    // GNOME wraps the accelerator in a translated sentence, and may list
    // alternatives after it; the first accelerator is the one.
    let Some(start) = description.find('<') else { return description.trim().to_string() };
    let accel = description[start..].split_whitespace().next().unwrap_or_default();
    let mut parts = Vec::new();
    let mut rest = accel;
    while let Some(inner) = rest.strip_prefix('<') {
        let Some(end) = inner.find('>') else { break };
        parts.push(match inner[..end].to_ascii_lowercase().as_str() {
            "control" | "ctrl" | "primary" => "Ctrl".to_string(),
            "alt" | "mod1" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "super" | "meta" | "mod4" | "hyper" => "Super".to_string(),
            other => other.to_string(),
        });
        rest = &inner[end + 1..];
    }
    if !rest.is_empty() {
        parts.push(key_label(rest));
    }
    parts.join("+")
}

fn key_label(keysym: &str) -> String {
    match keysym {
        "space" => "Space".into(),
        "Return" => "Enter".into(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_combinations_become_portal_triggers() {
        assert_eq!(portal_trigger("Ctrl+Alt+Space").as_deref(), Some("CTRL+ALT+space"));
        assert_eq!(portal_trigger("Super+D").as_deref(), Some("LOGO+d"));
        assert_eq!(portal_trigger("Ctrl+Shift+KeyK").as_deref(), Some("CTRL+SHIFT+k"));
        assert_eq!(portal_trigger("Alt+Digit1").as_deref(), Some("ALT+1"));
        assert_eq!(portal_trigger("F9").as_deref(), Some("F9"));
        assert_eq!(portal_trigger("Ctrl+Alt+Escape").as_deref(), Some("CTRL+ALT+Escape"));
        assert_eq!(portal_trigger("Ctrl+Backquote").as_deref(), Some("CTRL+grave"));
        assert_eq!(portal_trigger("Alt+PageDown").as_deref(), Some("ALT+Page_Down"));
        assert_eq!(portal_trigger("Ctrl+Numpad5").as_deref(), Some("CTRL+KP_5"));
        assert_eq!(portal_trigger("Ctrl + Alt + Space").as_deref(), Some("CTRL+ALT+space"));
    }

    #[test]
    fn unknown_parts_give_no_trigger() {
        assert_eq!(portal_trigger(""), None);
        assert_eq!(portal_trigger("Hyper+D"), None);
        assert_eq!(portal_trigger("Ctrl+MediaPlay"), None);
        assert_eq!(portal_trigger("F25"), None);
    }

    #[test]
    fn gnome_descriptions_read_like_the_config() {
        assert_eq!(display_trigger("Press <Control><Alt>space"), "Ctrl+Alt+Space");
        assert_eq!(display_trigger("Press <Super>d or <Control>F9"), "Super+D");
        assert_eq!(display_trigger("Drücken Sie <Shift><Super>Return"), "Shift+Super+Enter");
        assert_eq!(display_trigger("Ctrl+Alt+Space"), "Ctrl+Alt+Space");
        assert_eq!(display_trigger(""), "");
    }

    #[test]
    fn a_held_key_is_one_press_however_often_it_repeats() {
        let mut keys = Keys::default();
        let t = Instant::now();
        assert_eq!(keys.activated(DICTATE, t), Some(HotkeyEvent::Down));
        // GNOME's first repeat comes after the repeat delay, the rest fast.
        assert_eq!(keys.activated(DICTATE, t + Duration::from_millis(520)), None);
        for i in 1..60 {
            assert_eq!(keys.activated(DICTATE, t + Duration::from_millis(520 + 33 * i)), None);
        }
        assert_eq!(keys.deactivated(DICTATE), Some(HotkeyEvent::Up));
        // A release without a press is not passed on.
        assert_eq!(keys.deactivated(DICTATE), None);
        assert_eq!(keys.activated(DICTATE, t + Duration::from_secs(2)), Some(HotkeyEvent::Down));
    }

    #[test]
    fn a_lost_release_does_not_swallow_the_next_press() {
        let mut keys = Keys::default();
        let t = Instant::now();
        assert_eq!(keys.activated(DICTATE, t), Some(HotkeyEvent::Down));
        assert_eq!(keys.activated(DICTATE, t + Duration::from_secs(4)), Some(HotkeyEvent::Down));
    }

    #[test]
    fn cancel_and_strangers() {
        let mut keys = Keys::default();
        let t = Instant::now();
        assert_eq!(keys.activated(CANCEL, t), Some(HotkeyEvent::Cancel));
        assert_eq!(keys.deactivated(CANCEL), None);
        assert_eq!(keys.activated("other", t), None);
    }

    #[test]
    fn only_the_portal_in_use_leaving_counts() {
        // Ours went away, or was replaced by a new one.
        assert!(portal_left(":1.42", ":1.42"));
        // A portal appearing, say the one asking again started.
        assert!(!portal_left(":1.42", ""));
        // Some other owner changing hands.
        assert!(!portal_left(":1.42", ":1.7"));
        // Never knew which process it was: nothing to compare.
        assert!(!portal_left("", ""));
    }

    #[test]
    fn bound_state_knows_the_dictation_key() {
        let state = PortalState::Bound(vec![
            Bound { id: CANCEL.into(), trigger: "x".into() },
            Bound { id: DICTATE.into(), trigger: "Ctrl+Alt+Space".into() },
        ]);
        assert!(state.is_bound());
        assert_eq!(state.dictate_trigger(), Some("Ctrl+Alt+Space"));
        assert!(!PortalState::Bound(vec![Bound { id: CANCEL.into(), trigger: "x".into() }]).is_bound());
        assert!(!PortalState::Declined.is_bound());
    }
}
