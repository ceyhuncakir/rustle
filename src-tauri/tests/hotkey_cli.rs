//! `flow hotkey` end to end: the real binary reaches a control socket in a
//! temporary runtime directory, as a compositor's key binding would.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Output};
use std::sync::mpsc;

use flow_core::engine::{Event, Hotkey, HotkeyEvent};
use flow_desktop::linux::control::{socket_path_in, ControlSocket};

fn flow(runtime: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_flow"))
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .expect("run flow")
}

#[test]
fn every_command_reaches_a_running_flow() {
    let runtime = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let mut socket = ControlSocket::at(socket_path_in(runtime.path()));
    socket.start(tx).unwrap();
    for action in ["down", "up", "toggle", "cancel"] {
        let out = flow(runtime.path(), &["hotkey", action]);
        assert!(out.status.success(), "{action}: {}", String::from_utf8_lossy(&out.stderr));
    }
    socket.stop();
    let got: Vec<HotkeyEvent> = rx
        .try_iter()
        .filter_map(|e| match e {
            Event::Hotkey(h) => Some(h),
            _ => None,
        })
        .collect();
    use HotkeyEvent::*;
    assert_eq!(got, vec![Down, Up, Toggle, Cancel]);
}

#[test]
fn without_a_running_flow_it_says_so() {
    let runtime = tempfile::tempdir().unwrap();
    let out = flow(runtime.path(), &["hotkey", "toggle"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Flow is not running"), "{stderr}");
}

#[test]
fn an_unknown_action_is_refused_before_anything_is_sent() {
    let runtime = tempfile::tempdir().unwrap();
    let out = flow(runtime.path(), &["hotkey", "explode"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("down"));
}
