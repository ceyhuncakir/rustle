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
const SERVICE: &str = "ai.flow.ApiKey";

/// Env var checked before the keyring, per provider.
/// Kept in step with `backends::PROVIDERS` by a test; duplicated rather than
/// derived so this module stays free of a dependency on the backends.
pub const ENV_VARS: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("custom", "FLOW_API_KEY"),
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

/// The key for a provider: environment first, then the keyring.
pub fn get_key(provider: &str) -> String {
    if let Some((_, value)) = from_env(provider) {
        return value;
    }

    match entry(provider).and_then(|e| e.get_password()) {
        Ok(value) => value.trim().to_string(),
        Err(keyring::Error::NoEntry) => String::new(),
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
    match entry(provider).and_then(|e| e.delete_credential()) {
        Ok(()) => true,
        Err(keyring::Error::NoEntry) => false,
        Err(err) => {
            warn!("could not clear the keyring entry: {err}");
            false
        }
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
    //! for nor hang on. That the mapping matches the provider catalogue is
    //! checked in `backends::tests::env_vars_match_the_provider_catalogue`.
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

    #[test]
    fn env_var_wins_and_is_reported_as_the_source() {
        // "custom" maps to FLOW_API_KEY, which nothing else in the test
        // binary reads.
        std::env::set_var("FLOW_API_KEY", "  sk-from-env  ");
        assert_eq!(get_key("custom"), "sk-from-env");
        assert_eq!(key_source("custom"), "$FLOW_API_KEY");
        std::env::remove_var("FLOW_API_KEY");
    }
}
