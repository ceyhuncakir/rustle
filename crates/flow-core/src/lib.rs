//! Flow's portable logic.
//!
//! Nothing in this crate touches a microphone, a model runtime, a window or
//! a platform API. Those live in `flow-audio`, `flow-stt`, `flow-desktop`
//! and the Tauri app, and they implement the traits defined here in
//! [`engine`]. That split is what lets the whole dictation state machine be
//! tested with fakes, exactly as the Python daemon was.

pub mod backends;
pub mod cleanup;
pub mod config;
pub mod dsp;
pub mod engine;
pub mod history;
pub mod hold_or_tap;
pub mod learning;
pub mod models;
pub mod secrets;

#[cfg(test)]
pub(crate) mod test_util {
    //! Fixtures shared by the test modules.
    use crate::history::History;

    pub fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A history in a fresh temp dir, nested one level down so `open` has to
    /// create the directory. Keep the `TempDir` alive for the store's lifetime.
    pub fn history() -> (tempfile::TempDir, History) {
        let dir = tempfile::tempdir().unwrap();
        let history = History::open(dir.path().join("nested").join("h.db")).unwrap();
        (dir, history)
    }

    /// FNV-1a over the UTF-8 bytes; the pinned values were computed from the
    /// strings the Python module assembled, so the prompts stay byte-identical.
    pub fn fnv1a(text: &str) -> u64 {
        text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
    }
}
