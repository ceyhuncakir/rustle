//! API keys, stored in the operating system's keyring rather than the config
//! file.
//!
//! config.toml is plain text that ends up in backups, dotfile repos and over
//! anyone's shoulder. Keys live in the platform credential store instead -
//! the Secret Service on Linux, the Keychain on macOS, the Credential Manager
//! on Windows - unlocked with the login session like every other credential
//! on the machine. An environment variable still wins when one is set, so CI
//! and one-off runs work without touching the keyring.

use log::warn;

/// The keyring service name; the account under it is the provider key.
const SERVICE: &str = "dev.ceyhun.Rustle.ApiKey";
/// The service keys were saved under while the app was called Flow.
const FLOW_SERVICE: &str = "ai.flow.ApiKey";

/// Env var checked before the keyring, per provider.
/// Kept in step with `backends::PROVIDERS` by a test; duplicated rather than
/// derived so this module stays free of a dependency on the backends.
pub const ENV_VARS: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("custom", "RUSTLE_API_KEY"),
];

pub fn env_var(provider: &str) -> Option<&'static str> {
    ENV_VARS.iter().find(|(p, _)| *p == provider).map(|(_, v)| *v)
}

/// The provider's variable and its exported value, if set to anything at all.
fn from_env(provider: &str) -> Option<(&'static str, String)> {
    let var = env_var(provider)?;
    std::env::var(var).ok().filter(|value| !value.is_empty()).map(|value| (var, value.trim().to_string()))
}

fn entry(provider: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, provider)
}

/// The key for a provider: environment first, then the keyring, then one
/// saved while the app was called Flow, then - on Linux, once - the entry the
/// Python version left behind.
pub fn get_key(provider: &str) -> String {
    if let Some((_, value)) = from_env(provider) {
        return value;
    }

    match entry(provider).and_then(|e| e.get_password()) {
        Ok(value) => value.trim().to_string(),
        Err(keyring::Error::NoEntry) => flow_key(provider).unwrap_or_else(|| legacy_key(provider)),
        // A locked keyring is not fatal.
        Err(err) => {
            warn!("could not read the keyring: {err}");
            String::new()
        }
    }
}

pub fn set_key(provider: &str, key: &str) -> bool {
    match entry(provider).and_then(|e| e.set_password(key.trim())) {
        Ok(()) => true,
        Err(err) => {
            warn!("could not write to the keyring: {err}");
            false
        }
    }
}

/// True when an entry was removed; false when there was none or the keyring
/// could not be reached.
pub fn clear_key(provider: &str) -> bool {
    // Older copies go too, or the next start would find one and bring the
    // key back.
    if let Ok(old) = keyring::Entry::new(FLOW_SERVICE, provider) {
        let _ = old.delete_credential();
    }
    #[cfg(target_os = "linux")]
    legacy::clear(provider);
    match entry(provider).and_then(|e| e.delete_credential()) {
        Ok(()) => true,
        Err(keyring::Error::NoEntry) => false,
        Err(err) => {
            warn!("could not clear the keyring entry: {err}");
            false
        }
    }
}

/// A key saved while the app was called Flow, moved under the new name.
fn flow_key(provider: &str) -> Option<String> {
    let old = keyring::Entry::new(FLOW_SERVICE, provider).ok()?;
    let key = old.get_password().ok()?.trim().to_string();
    if key.is_empty() {
        return None;
    }
    if set_key(provider, &key) {
        log::info!("moved the {provider} API key over from Flow");
        let _ = old.delete_credential();
    }
    Some(key)
}

#[cfg(target_os = "linux")]
fn legacy_key(provider: &str) -> String {
    legacy::migrate(provider, legacy::lookup, set_key).unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn legacy_key(_provider: &str) -> String {
    String::new()
}

/// Keys saved by the Python version.
///
/// It stored them through libsecret under a schema of its own - `xdg:schema`
/// `ai.flow.ApiKey` with a single `provider` attribute - which the keyring
/// crate's service/username lookup never matches, so without this an upgrade
/// would quietly lose every key. `secret-tool` reads them; where it is not
/// installed there is simply nothing to migrate.
#[cfg(target_os = "linux")]
mod legacy {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use log::{info, warn};

    const SCHEMA: &str = "ai.flow.ApiKey";
    /// A locked keyring can put up an unlock prompt, and whoever asked for
    /// the key - a dictation in flight, the settings window - must not hang
    /// on it.
    const LOOKUP_LIMIT: Duration = Duration::from_secs(10);

    /// Providers already looked up this run. Spawning `secret-tool` on every
    /// read of a key that does not exist would be wasteful, and once a key
    /// is copied across the keyring answers first anyway.
    static LOOKED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Find the old entry for `provider` and copy it into the keyring with
    /// `store`, once per run. The old entry is left alone: the Python daemon
    /// may still be installed and using it.
    pub(super) fn migrate(
        provider: &str,
        lookup: impl Fn(&str) -> Option<String>,
        store: impl Fn(&str, &str) -> bool,
    ) -> Option<String> {
        {
            let mut looked = LOOKED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if looked.iter().any(|p| p == provider) {
                return None;
            }
            looked.push(provider.to_string());
        }
        let key = lookup(provider)?;
        if store(provider, &key) {
            info!("moved the {provider} API key over from the Python version");
        }
        Some(key)
    }

    pub(super) fn lookup(provider: &str) -> Option<String> {
        let mut command = Command::new("secret-tool");
        command.args(["lookup", "xdg:schema", SCHEMA, "provider", provider]);
        run(&mut command, LOOKUP_LIMIT)
    }

    pub(super) fn clear(provider: &str) {
        let mut command = Command::new("secret-tool");
        command.args(["clear", "xdg:schema", SCHEMA, "provider", provider]);
        run(&mut command, LOOKUP_LIMIT);
    }

    /// What the command printed, trimmed, when it exits successfully within
    /// `limit` and printed anything; `None` in every other case, including a
    /// command that does not exist.
    pub(super) fn run(command: &mut Command, limit: Duration) -> Option<String> {
        let mut child =
            command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let deadline = Instant::now() + limit;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
                _ => {
                    warn!("gave up waiting for secret-tool");
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        if !status.success() {
            return None;
        }
        let mut out = String::new();
        child.stdout.take()?.read_to_string(&mut out).ok()?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.to_string())
    }
}

/// Where the key is coming from - shown in the GUI so it is never a mystery
/// which of two possible keys is actually in use.
pub fn key_source(provider: &str) -> String {
    if let Some((var, _)) = from_env(provider) {
        return format!("${var}");
    }
    if get_key(provider).is_empty() {
        "not set".into()
    } else {
        "keyring".into()
    }
}

#[cfg(test)]
mod tests {
    //! Only the environment half is exercised here: the keyring half talks to
    //! the desktop's secret service, which a test run must neither prompt
    //! for nor hang on. The migration from the Python version runs against
    //! stand-ins for the same reason. That the mapping matches the provider
    //! catalogue is checked in
    //! `backends::tests::env_vars_match_the_provider_catalogue`.
    use super::*;

    #[test]
    fn every_hosted_provider_has_a_distinct_variable() {
        let mut vars: Vec<&str> = ENV_VARS.iter().map(|(_, v)| *v).collect();
        vars.sort_unstable();
        vars.dedup();
        assert_eq!(vars.len(), ENV_VARS.len());
        assert_eq!(env_var("openai"), Some("OPENAI_API_KEY"));
        assert_eq!(env_var("ollama"), None);
        assert_eq!(env_var("none"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_python_versions_key_is_copied_across_once() {
        use std::cell::RefCell;
        // A provider name nothing else in the test binary asks for, since
        // "once" is per process.
        let provider = "legacy-test-provider";
        let stored = RefCell::new(Vec::new());
        let looked = RefCell::new(0);
        let lookup = |_: &str| {
            *looked.borrow_mut() += 1;
            Some("sk-old".to_string())
        };
        let store = |p: &str, k: &str| {
            stored.borrow_mut().push((p.to_string(), k.to_string()));
            true
        };
        assert_eq!(legacy::migrate(provider, lookup, store), Some("sk-old".into()));
        assert_eq!(*stored.borrow(), vec![(provider.to_string(), "sk-old".to_string())]);
        // Not asked again this run.
        assert_eq!(legacy::migrate(provider, lookup, store), None);
        assert_eq!(*looked.borrow(), 1);

        // Nothing stored under the old schema: nothing written.
        stored.borrow_mut().clear();
        assert_eq!(legacy::migrate("legacy-test-absent", |_| None, store), None);
        assert!(stored.borrow().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_missing_or_failing_secret_tool_is_not_an_error() {
        use std::process::Command;
        use std::time::{Duration, Instant};
        let limit = Duration::from_secs(5);
        let sh = |script: &str| {
            let mut command = Command::new("sh");
            command.args(["-c", script]);
            command
        };
        assert_eq!(legacy::run(&mut sh("printf '  sk-old\\n'"), limit), Some("sk-old".into()));
        assert_eq!(legacy::run(&mut sh("exit 1"), limit), None);
        assert_eq!(legacy::run(&mut sh("printf '   '"), limit), None);
        assert_eq!(legacy::run(&mut Command::new("rustle-no-such-program-anywhere"), limit), None);
        // A prompt nobody answers is given up on.
        let started = Instant::now();
        assert_eq!(legacy::run(&mut sh("sleep 5"), Duration::from_millis(100)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn env_var_wins_and_is_reported_as_the_source() {
        // "custom" maps to RUSTLE_API_KEY, which nothing else in the test
        // binary reads.
        std::env::set_var("RUSTLE_API_KEY", "  sk-from-env  ");
        assert_eq!(get_key("custom"), "sk-from-env");
        assert_eq!(key_source("custom"), "$RUSTLE_API_KEY");
        std::env::remove_var("RUSTLE_API_KEY");
    }
}
