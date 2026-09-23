//! The control socket: any program running as this user can drive
//! dictation by writing `down`, `up`, `toggle` or `cancel` to it, one per
//! line. Each line is answered with `ok` or `error: ...`.
//!
//! This is how compositors whose portal has no global shortcuts (sway,
//! river, niri and the other wlroots ones behind xdg-desktop-portal-wlr)
//! reach Flow: their own key bindings run `flow hotkey down` on press and
//! `flow hotkey up` on release. It runs on every Linux session, next to the
//! real shortcut, so scripts work everywhere.
//!
//! The socket sits in `$XDG_RUNTIME_DIR/flow/`, a directory only this user
//! may enter, and is itself 0600, so nobody else can reach it.

use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use flow_core::engine::{DesktopError, Event, Hotkey, HotkeyEvent};

/// A client that connects and then says nothing must not hold up the next.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
/// Commands are a few bytes; anything past this is not a client of ours.
const CLIENT_BYTES: u64 = 64 * 1024;

/// Whether any key binding has reached Flow through the socket in this
/// process, which is the only sign that the user's compositor is set up.
static HEARD: AtomicBool = AtomicBool::new(false);

/// The event a command line stands for.
pub fn parse(line: &str) -> Option<HotkeyEvent> {
    match line.trim() {
        "down" => Some(HotkeyEvent::Down),
        "up" => Some(HotkeyEvent::Up),
        "toggle" => Some(HotkeyEvent::Toggle),
        "cancel" => Some(HotkeyEvent::Cancel),
        _ => None,
    }
}

/// `$XDG_RUNTIME_DIR/flow/control.sock`, or `None` without a runtime dir.
pub fn socket_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())?;
    Some(socket_path_in(Path::new(&runtime)))
}

pub fn socket_path_in(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("flow").join("control.sock")
}

/// Whether a key binding has driven dictation through the socket yet.
pub fn heard() -> bool {
    HEARD.load(Ordering::SeqCst)
}

/// Whether a running Flow answers on the socket.
pub fn listening() -> bool {
    socket_path().is_some_and(|path| UnixStream::connect(path).is_ok())
}

/// Listens on the socket and turns commands into hotkey events.
pub struct ControlSocket {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlSocket {
    /// At the standard path; `None` without `$XDG_RUNTIME_DIR`.
    pub fn new() -> Option<ControlSocket> {
        socket_path().map(ControlSocket::at)
    }

    pub fn at(path: PathBuf) -> ControlSocket {
        ControlSocket { path, stop: Arc::default(), thread: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn bind(&self) -> io::Result<UnixListener> {
        if let Some(dir) = self.path.parent() {
            private_dir(dir)?;
        }
        clear_stale(&self.path)?;
        let listener = UnixListener::bind(&self.path)?;
        // The directory already keeps others out; this is belt and braces.
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(listener)
    }
}

impl Hotkey for ControlSocket {
    fn start(&mut self, sink: Sender<Event>) -> Result<(), DesktopError> {
        let fail =
            |e: io::Error| DesktopError::Unavailable(format!("control socket {}: {e}", self.path.display()));
        let listener = self.bind().map_err(fail)?;
        self.stop.store(false, Ordering::SeqCst);
        let stop = self.stop.clone();
        let thread = std::thread::Builder::new()
            .name("flow-control".into())
            .spawn(move || serve(listener, sink, stop))
            .map_err(fail)?;
        self.thread = Some(thread);
        log::info!("control socket: {}", self.path.display());
        Ok(())
    }

    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        self.stop.store(true, Ordering::SeqCst);
        // `accept` only returns for a connection, so make one to wake it.
        let _ = UnixStream::connect(&self.path);
        let _ = thread.join();
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for ControlSocket {
    /// `flow --headless` lets its engine go without stopping it; the socket
    /// must not outlive the process all the same.
    fn drop(&mut self) {
        Hotkey::stop(self);
    }
}

/// Make sure the socket's directory exists and only we can enter it. One
/// already there must be a real directory of ours: a link planted in its
/// place could otherwise have the socket made somewhere else.
fn private_dir(dir: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != own_uid()? {
        return Err(io::Error::other(format!("{} is not a directory of this user's", dir.display())));
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

/// This process's user id: `/proc/self` belongs to it.
fn own_uid() -> io::Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

/// Clear the way for our socket. One that still answers belongs to a Flow
/// that is running; one that does not was left by a Flow that died.
/// Anything that is not a socket is left alone.
fn clear_stale(path: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.file_type().is_socket() {
        return Err(io::Error::other("something that is not a socket is in the way; not touching it"));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(io::Error::new(ErrorKind::AddrInUse, "another Flow is already listening"));
    }
    fs::remove_file(path)
}

fn serve(listener: UnixListener, sink: Sender<Event>, stop: Arc<AtomicBool>) {
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(stream) => {
                if !serve_client(stream, &sink) {
                    break;
                }
            }
            Err(err) => log::debug!("control socket: {err}"),
        }
    }
}

/// Answer one client's lines. Returns false once the engine is gone.
fn serve_client(stream: UnixStream, sink: &Sender<Event>) -> bool {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let Ok(mut reply_to) = stream.try_clone() else { return true };
    for line in BufReader::new(stream.take(CLIENT_BYTES)).lines() {
        // A timeout or bytes that are not text end this client only.
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match parse(&line) {
            Some(event) => {
                if sink.send(Event::Hotkey(event)).is_err() {
                    return false;
                }
                HEARD.store(true, Ordering::SeqCst);
                "ok".to_string()
            }
            None => format!("error: unknown command {:?}; expected down, up, toggle or cancel", line.trim()),
        };
        if writeln!(reply_to, "{reply}").is_err() {
            break;
        }
    }
    true
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error(
        "Flow is not running: nothing listens on {0}. Start it from the app menu, or with `flow --headless`."
    )]
    NotRunning(PathBuf),
    #[error("$XDG_RUNTIME_DIR is not set, so there is no control socket to reach Flow on")]
    NoRuntimeDir,
    #[error("Flow said: {0}")]
    Refused(String),
    #[error("control socket {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

/// Send one command to the running Flow and wait for its answer.
pub fn send(command: &str) -> Result<(), SendError> {
    send_to(&socket_path().ok_or(SendError::NoRuntimeDir)?, command)
}

pub fn send_to(path: &Path, command: &str) -> Result<(), SendError> {
    let io = |source: io::Error| SendError::Io { path: path.to_path_buf(), source };
    let mut stream = UnixStream::connect(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused => SendError::NotRunning(path.to_path_buf()),
        _ => io(e),
    })?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT)).map_err(io)?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT)).map_err(io)?;
    writeln!(stream, "{command}").map_err(io)?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(io)?;
    match reply.trim() {
        "ok" => Ok(()),
        "" => Err(SendError::Refused("nothing; it closed the connection".into())),
        other => Err(SendError::Refused(other.trim_start_matches("error: ").to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn events(rx: &mpsc::Receiver<Event>) -> Vec<HotkeyEvent> {
        rx.try_iter()
            .filter_map(|e| match e {
                Event::Hotkey(h) => Some(h),
                _ => None,
            })
            .collect()
    }

    fn started(runtime: &Path) -> (ControlSocket, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let mut socket = ControlSocket::at(socket_path_in(runtime));
        socket.start(tx).expect("start");
        (socket, rx)
    }

    #[test]
    fn commands_become_hotkey_events() {
        let runtime = tempfile::tempdir().unwrap();
        let (mut socket, rx) = started(runtime.path());
        for command in ["down", "up", "toggle", "cancel"] {
            send_to(socket.path(), command).unwrap();
        }
        socket.stop();
        use HotkeyEvent::*;
        assert_eq!(events(&rx), vec![Down, Up, Toggle, Cancel]);
        assert!(heard());
    }

    #[test]
    fn an_unknown_command_is_refused_and_sends_nothing() {
        let runtime = tempfile::tempdir().unwrap();
        let (mut socket, rx) = started(runtime.path());
        let err = send_to(socket.path(), "explode").unwrap_err();
        assert!(matches!(&err, SendError::Refused(why) if why.contains("explode")), "{err}");
        socket.stop();
        assert!(events(&rx).is_empty());
    }

    #[test]
    fn the_socket_and_its_directory_are_private() {
        let runtime = tempfile::tempdir().unwrap();
        let (mut socket, _rx) = started(runtime.path());
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(socket.path().parent().unwrap()), 0o700);
        assert_eq!(mode(socket.path()), 0o600);
        socket.stop();
        assert!(!socket.path().exists(), "stopping removes the socket");
    }

    #[test]
    fn a_stale_socket_is_replaced() {
        let runtime = tempfile::tempdir().unwrap();
        let path = socket_path_in(runtime.path());
        fs::create_dir(path.parent().unwrap()).unwrap();
        // Bound and dropped: the file stays, nobody listens. A Flow that died.
        drop(UnixListener::bind(&path).unwrap());
        let (mut socket, rx) = started(runtime.path());
        send_to(&path, "toggle").unwrap();
        socket.stop();
        assert_eq!(events(&rx), vec![HotkeyEvent::Toggle]);
    }

    #[test]
    fn a_live_socket_is_not_stolen() {
        let runtime = tempfile::tempdir().unwrap();
        let (mut first, _rx) = started(runtime.path());
        let (tx, _rx2) = mpsc::channel();
        let mut second = ControlSocket::at(socket_path_in(runtime.path()));
        assert!(second.start(tx).is_err());
        send_to(first.path(), "down").expect("the first still answers");
        first.stop();
    }

    #[test]
    fn something_else_in_the_way_is_left_alone() {
        let runtime = tempfile::tempdir().unwrap();
        let path = socket_path_in(runtime.path());
        fs::create_dir(path.parent().unwrap()).unwrap();
        fs::write(&path, "not a socket").unwrap();
        let (tx, _rx) = mpsc::channel();
        assert!(ControlSocket::at(path.clone()).start(tx).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "not a socket");
    }

    #[test]
    fn a_linked_directory_is_refused() {
        let runtime = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), runtime.path().join("flow")).unwrap();
        let (tx, _rx) = mpsc::channel();
        assert!(ControlSocket::at(socket_path_in(runtime.path())).start(tx).is_err());
        assert_eq!(fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn nobody_listening_says_flow_is_not_running() {
        let runtime = tempfile::tempdir().unwrap();
        let err = send_to(&socket_path_in(runtime.path()), "down").unwrap_err();
        assert!(matches!(err, SendError::NotRunning(_)), "{err}");
        assert!(err.to_string().contains("Flow is not running"));
    }

    #[test]
    fn a_gone_engine_ends_the_listener() {
        let runtime = tempfile::tempdir().unwrap();
        let (mut socket, rx) = started(runtime.path());
        drop(rx);
        // The command cannot be delivered, so no `ok` comes back.
        assert!(send_to(socket.path(), "down").is_err());
        socket.stop();
    }
}
