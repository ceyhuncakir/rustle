//! Configuration, read from `config.toml`.
//!
//! Every field has a working default, so the file is optional. The default
//! file is heavily commented because those comments are the documentation;
//! edits therefore go through [`set_value`], which changes one line in place
//! and leaves everything else - including the comments - untouched.
//!
//! The schema and file location are unchanged from the Python version, so an
//! existing `config.toml` keeps working.

use std::path::{Path, PathBuf};

use log::warn;
use serde::{Deserialize, Serialize};

/// Where config and data live. Linux follows XDG (`~/.config/flow`,
/// `~/.local/share/flow`), macOS uses `~/Library/Application Support/flow`
/// for both. Windows keeps the config in `%APPDATA%\flow` but the data in
/// `%LOCALAPPDATA%\flow`: models run to gigabytes and the history is
/// private, and neither should roam with a domain profile to every machine
/// the user signs in to. `FLOW_CONFIG_DIR` / `FLOW_DATA_DIR` override both,
/// which is what the tests and the nested dev sessions use.
fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLOW_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("flow")
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FLOW_DATA_DIR") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")).join("flow")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Input device name substring; empty means the system default.
    pub device: String,
    pub sample_rate: u32,
    /// Amplitude below this counts as silence when trimming the take.
    pub silence_rms: f32,
    pub trim_silence: bool,
    /// Refuse to transcribe takes shorter than this; usually a mis-press.
    pub min_seconds: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            sample_rate: 16000,
            silence_rms: 0.006,
            trim_silence: true,
            min_seconds: 0.35,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SttConfig {
    /// v3 is the multilingual sibling of v2 at the same speed, and detects
    /// the language itself.
    pub model: String,
    /// `auto` uses a graphics card when one helps; `gpu` insists (`cuda`, its
    /// old name, still works); `cpu` never tries.
    pub provider: String,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self { model: "nemo-parakeet-tdt-0.6b-v3".into(), provider: "auto".into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CleanupConfig {
    pub enabled: bool,
    /// ollama | anthropic | openai | openrouter | deepseek | custom | none
    pub backend: String,
    pub model: String,
    pub endpoint: String,
    /// Only for `backend = "custom"`: the OpenAI-compatible base URL.
    pub base_url: String,
    pub timeout: f32,
    /// How long Ollama keeps the model loaded after a request.
    pub keep_alive: String,
    /// light | balanced | tidy
    pub style: String,
    /// Delete ideas the speaker abandoned mid-dictation.
    pub resolve_intent: bool,
    /// auto | never | always
    pub think: String,
    pub languages: Vec<String>,
    /// same | en | nl
    pub output_language: String,
    pub dictionary: Vec<String>,
    /// App identifier -> instruction appended to the prompt for that app.
    pub app_rules: std::collections::BTreeMap<String, String>,
}

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: "ollama".into(),
            model: "qwen3:14b".into(),
            endpoint: "http://localhost:11434".into(),
            base_url: String::new(),
            timeout: 20.0,
            keep_alive: "1h".into(),
            style: "balanced".into(),
            resolve_intent: true,
            think: "never".into(),
            languages: vec!["en".into(), "nl".into()],
            output_language: "same".into(),
            dictionary: Vec::new(),
            app_rules: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    pub enabled: bool,
    /// Wait for enough signal before drawing conclusions about a voice.
    pub min_dictations: u32,
    /// Re-mine the profile after this many new dictations.
    pub refresh_every: u32,
    /// Cap on learned terms; every one lengthens the cleanup prompt.
    pub max_terms: usize,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self { enabled: false, min_dictations: 15, refresh_every: 25, max_terms: 40 }
    }
}

/// Platform integration knobs. New in the Rust version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopConfig {
    /// auto | window | off. Only consulted where the overlay is a window of
    /// our own (everything except GNOME, where the Shell extension draws it).
    pub overlay: String,
    /// The dictation shortcut for platforms where the app registers it
    /// itself. On GNOME the binding lives in the extension's settings.
    pub hotkey: String,
    /// Holding the shortcut stops recording on release; a tap latches it.
    pub push_to_talk: bool,
}

impl Default for DesktopConfig {
    fn default() -> Self {
        Self { overlay: "auto".into(), hotkey: default_hotkey().into(), push_to_talk: true }
    }
}

/// Super+D is taken on both other platforms: Windows reserves Win+D for
/// Show Desktop, so registering it fails outright, and KDE and Ubuntu bind it
/// to the same thing.
fn default_hotkey() -> &'static str {
    if cfg!(target_os = "macos") {
        "Alt+D"
    } else {
        "Ctrl+Alt+Space"
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub audio: AudioConfig,
    pub stt: SttConfig,
    pub cleanup: CleanupConfig,
    pub learning: LearningConfig,
    pub desktop: DesktopConfig,
}

impl Config {
    /// Load from the default location; a missing file means all defaults.
    /// Values that do not fit their field are skipped and logged - see
    /// [`Config::load_with_warnings`].
    pub fn load() -> anyhow::Result<Config> {
        let (config, warnings) = Self::load_with_warnings()?;
        for warning in &warnings {
            warn!("config.toml: {warning}");
        }
        Ok(config)
    }

    /// Load, skipping any value that does not fit its field instead of
    /// giving up on the whole file, and say what was skipped - lines like
    /// `ignored cleanup.timeout: expected a number` - for the caller to show.
    /// A file that is not valid TOML at all is still an error.
    pub fn load_with_warnings() -> anyhow::Result<(Config, Vec<String>)> {
        Self::load_from(&config_path())
    }

    fn load_from(path: &Path) -> anyhow::Result<(Config, Vec<String>)> {
        if !path.exists() {
            return Ok((Config::default(), Vec::new()));
        }
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    /// Unknown keys are ignored, as the Python version did, so an older or
    /// newer file never refuses to load. A mistyped value costs only itself:
    /// `timeout = "30"` used to fail the whole parse, and with it every other
    /// setting in the file fell back to its default without a word.
    fn parse(text: &str) -> anyhow::Result<(Config, Vec<String>)> {
        let mut table: toml::Table = toml::from_str(text)?;
        let mut warnings = Vec::new();
        prune(&mut table, &mut Vec::new(), &mut warnings);
        // Key order depends on whether some crate in the build turned on
        // toml's preserve_order; sorting keeps the report the same either way.
        warnings.sort();
        Ok((toml::Value::Table(table).try_into()?, warnings))
    }
}

/// Whether `value`, placed at `path` in an otherwise empty file, reads as a
/// config. Every field has a default, so this judges the one value alone.
fn fits(path: &[String], value: &toml::Value) -> Result<(), toml::de::Error> {
    let mut wrapped = value.clone();
    for key in path.iter().rev() {
        wrapped = toml::Value::Table(toml::Table::from_iter([(key.clone(), wrapped)]));
    }
    wrapped.try_into::<Config>().map(drop)
}

/// Remove every value under `table` (found at `path`) that does not fit its
/// field, noting each one in `warnings`.
fn prune(table: &mut toml::Table, path: &mut Vec<String>, warnings: &mut Vec<String>) {
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        path.push(key.clone());
        if let Some(value) = table.get_mut(&key) {
            if let Err(err) = fits(path, value) {
                // A table where a table belongs has the problem further in:
                // keep its good entries rather than dropping the section.
                let empty = toml::Value::Table(toml::Table::new());
                if let (toml::Value::Table(inner), true) = (&mut *value, fits(path, &empty).is_ok()) {
                    prune(inner, path, warnings);
                }
                if fits(path, value).is_err() {
                    warnings.push(format!("ignored {}: {}", dotted(path), explain(&err, value)));
                    table.remove(&key);
                }
            }
        }
        path.pop();
    }
}

/// `cleanup.app_rules."org.gnome.Console"`, as the key would be written.
fn dotted(path: &[String]) -> String {
    let bare =
        |key: &str| !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let parts: Vec<String> =
        path.iter().map(|key| if bare(key) { key.clone() } else { format!("{key:?}") }).collect();
    parts.join(".")
}

/// Turn serde's "invalid type: string \"30\", expected f32" into something a
/// person editing the file can act on.
fn explain(err: &toml::de::Error, value: &toml::Value) -> String {
    let message = err.message();
    let Some((_, expected)) = message.rsplit_once("expected ") else {
        return message.to_string();
    };
    let wanted = match expected.trim() {
        "f32" | "f64" => "a number",
        "u8" | "u16" | "u32" | "u64" | "usize" => "a whole number, 0 or more",
        "i8" | "i16" | "i32" | "i64" | "isize" => "a whole number",
        "a boolean" => "true or false",
        "a string" | "a borrowed string" | "a char" => "text in quotes",
        "a sequence" => "a list",
        other if other == "a map" || other.starts_with("struct ") => "a table",
        other => other,
    };
    // A list with one bad item names the item; a list where none belongs
    // is reported as "invalid type: sequence".
    if value.is_array() && !message.starts_with("invalid type: sequence") {
        format!("expected a list of items that are each {wanted}")
    } else {
        format!("expected {wanted}")
    }
}

const DEFAULT_TOML: &str = r##"# Flow configuration. Every value here is the built-in default; delete a line
# to go back to it.

[audio]
device = ""            # input device name substring, empty = system default
sample_rate = 16000
trim_silence = true
silence_rms = 0.006
min_seconds = 0.35

[stt]
# v3 is multilingual and detects the language itself, at the same speed as
# the English-only v2. Switch to "nemo-parakeet-tdt-0.6b-v2" if you only ever
# dictate in English.
model = "nemo-parakeet-tdt-0.6b-v3"
provider = "auto"      # auto | gpu | cpu

[cleanup]
enabled = true
# Where the cleanup model runs.
#   ollama     - local, offline, free, nothing leaves this machine
#   anthropic  - Claude
#   openai     - GPT
#   openrouter - one key, hundreds of models including DeepSeek
#   deepseek   - DeepSeek directly
#   custom     - any OpenAI-compatible endpoint; set base_url below
#   none       - paste the raw transcript, no cleanup
# API keys live in your operating system's keyring, never here. Set them in
# the settings window, or export the provider's environment variable.
backend = "ollama"

# Only used when backend = "custom".
base_url = ""
model = "qwen3:14b"
endpoint = "http://localhost:11434"
timeout = 20.0
keep_alive = "1h"     # "0" unloads immediately, "-1" never unloads

# How much liberty the model takes with your wording.
#   light    - punctuation and obvious "um"s only, wording untouched
#   balanced - also drops false starts and resolves changes of mind
#   tidy     - also tightens loose grammar
style = "balanced"

# Delete ideas you abandoned mid-sentence ("actually, forget that, what I
# need is..."). The only rule that removes content; set false to keep
# everything you said.
resolve_intent = true

# Let the model reason before answering. Off by default: measured on the
# cleanup eval it scored the same either way, but took 3-21s instead of
# 0.1-0.3s. "auto" reasons only on longer transcripts containing a
# retraction cue; "always" reasons on everything.
think = "never"

# The languages you dictate in. The recogniser detects the language on its
# own; this tells the cleanup model what to expect so it does not drift.
languages = ["en", "nl"]

# What language to write out.
#   same - whatever you spoke, cleaned up in that language
#   en   - always English, translating your Dutch
#   nl   - always Dutch, translating your English
output_language = "same"

# Names and jargon the recogniser keeps getting wrong.
dictionary = []

# Per-application tone. Keys are application identifiers - run
# `flow context` with the target app focused to find one.
[cleanup.app_rules]
# "org.gnome.Console" = "Output a shell command only, no prose."
# "Slack" = "Casual, no greeting or sign-off."

# Learning is OFF unless you switch it on. While it is off Flow keeps no
# record of anything you dictate.
#
# Switched on, Flow stores your dictations locally and periodically mines two
# things from them with the cleanup model: the jargon and project names a
# general recogniser gets wrong, and a short note describing how you talk.
# Both are fed back into the cleanup prompt, so the more you use it the more
# it sounds like you.
#
#   flow learning on      start learning and using what it learns
#   flow learning off     stop both; the profile is kept for next time
#   flow vocab            see what it has picked up
#   flow history --clear  delete everything it has stored
[learning]
enabled = false
min_dictations = 15
refresh_every = 25
max_terms = 40

[desktop]
# The floating island. auto = show it wherever the desktop supports an
# overlay that cannot steal focus; window = force a plain always-on-top
# window; off = never show it. On GNOME the Shell extension draws it and
# this setting is ignored.
overlay = "auto"
# The dictation shortcut. On GNOME the binding lives in the Shell
# extension's settings instead.
hotkey = "Ctrl+Alt+Space"
# Hold the shortcut to talk and release to stop; a short tap keeps
# recording until the next tap.
push_to_talk = true
"##;

/// Write the commented default file if it does not exist yet. Returns the
/// path either way.
pub fn write_default_config() -> anyhow::Result<PathBuf> {
    let path = config_path();
    write_default_config_to(&path)?;
    Ok(path)
}

pub fn write_default_config_to(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        std::fs::write(path, default_config_text())?;
    }
    Ok(())
}

/// The default file as this platform writes it, with macOS's Alt+D hotkey
/// to match [`DesktopConfig::default`].
fn default_config_text() -> String {
    if cfg!(target_os = "macos") {
        DEFAULT_TOML.replace("hotkey = \"Ctrl+Alt+Space\"", "hotkey = \"Alt+D\"")
    } else {
        DEFAULT_TOML.to_string()
    }
}

/// A value to write with [`set_value`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<String>),
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}

impl From<Value> for toml_edit::Value {
    fn from(v: Value) -> Self {
        match v {
            Value::Bool(b) => b.into(),
            Value::Int(i) => i.into(),
            Value::Float(f) => f.into(),
            Value::Str(s) => s.into(),
            Value::List(items) => toml_edit::Array::from_iter(items).into(),
        }
    }
}

/// Set one key inside one section of the default config file, creating
/// either if missing, and keeping every comment.
pub fn set_value(section: &str, key: &str, value: impl Into<Value>) -> anyhow::Result<()> {
    let path = write_default_config()?;
    set_value_in(&path, section, key, value)
}

pub fn set_value_in(path: &Path, section: &str, key: &str, value: impl Into<Value>) -> anyhow::Result<()> {
    write_default_config_to(path)?;
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text.parse()?;

    // Dotted sections ("cleanup.app_rules") walk down one table per part.
    let mut table = doc.as_table_mut();
    for part in section.split('.') {
        table = table
            .entry(part)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("[{section}] is not a table"))?;
    }

    let new: toml_edit::Value = value.into().into();
    match table.get_mut(key).and_then(toml_edit::Item::as_value_mut) {
        // Replace only the value so the trailing comment on the line stays.
        Some(old) => {
            let decor = old.decor().clone();
            *old = new;
            *old.decor_mut() = decor;
        }
        None => {
            table.insert(key, toml_edit::Item::Value(new));
        }
    }

    std::fs::write(path, doc.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::strings;

    /// Parse, insisting that nothing was skipped.
    fn parse(text: &str) -> Config {
        let (config, warnings) = Config::parse(text).unwrap();
        assert_eq!(warnings, Vec::<String>::new());
        config
    }

    fn load(path: &Path) -> Config {
        Config::load_from(path).unwrap().0
    }

    #[test]
    fn defaults_parse_from_the_default_file() {
        assert_eq!(parse(&default_config_text()), Config::default());
    }

    #[test]
    fn the_default_file_carries_this_platforms_hotkey() {
        let hotkey = format!("hotkey = \"{}\"", default_hotkey());
        assert!(default_config_text().contains(&hotkey), "{hotkey}");
        // Win+D is reserved on Windows and Show Desktop on KDE and Ubuntu.
        assert_ne!(default_hotkey(), "Super+D");
        if cfg!(target_os = "macos") {
            assert_eq!(default_hotkey(), "Alt+D");
        } else {
            assert_eq!(default_hotkey(), "Ctrl+Alt+Space");
        }
    }

    #[test]
    fn missing_file_is_all_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, warnings) = Config::load_from(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(warnings.is_empty());
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let cfg = parse("[audio]\nbogus = 1\nsample_rate = 8000\n[insert]\nrestore_clipboard = true\n");
        assert_eq!(cfg.audio.sample_rate, 8000);
    }

    #[test]
    fn partial_sections_keep_other_defaults() {
        let cfg = parse("[cleanup]\nmodel = \"qwen3:8b\"\n");
        assert_eq!(cfg.cleanup.model, "qwen3:8b");
        assert_eq!(cfg.cleanup.backend, "ollama");
        assert_eq!(cfg.cleanup.languages, vec!["en", "nl"]);
    }

    #[test]
    fn app_rules_are_read() {
        let cfg = parse("[cleanup.app_rules]\n\"Slack\" = \"Casual.\"\n");
        assert_eq!(cfg.cleanup.app_rules["Slack"], "Casual.");
    }

    #[test]
    fn a_mistyped_value_costs_only_itself() {
        let text = "[cleanup]\nbackend = \"anthropic\"\ntimeout = \"30\"\nmodel = \"claude-opus-5\"\n\
                    [learning]\nenabled = true\nmax_terms = -1\nrefresh_every = 10\n\
                    [audio]\nsample_rate = 16000.5\ntrim_silence = \"no\"\n";
        let (cfg, warnings) = Config::parse(text).unwrap();
        assert_eq!(
            warnings,
            strings(&[
                "ignored audio.sample_rate: expected a whole number, 0 or more",
                "ignored audio.trim_silence: expected true or false",
                "ignored cleanup.timeout: expected a number",
                "ignored learning.max_terms: expected a whole number, 0 or more",
            ])
        );
        // The bad values fall back to their defaults...
        assert_eq!(cfg.cleanup.timeout, CleanupConfig::default().timeout);
        assert_eq!(cfg.learning.max_terms, LearningConfig::default().max_terms);
        assert_eq!(cfg.audio.sample_rate, 16000);
        assert!(cfg.audio.trim_silence);
        // ...and everything around them is kept.
        assert_eq!(cfg.cleanup.backend, "anthropic");
        assert_eq!(cfg.cleanup.model, "claude-opus-5");
        assert!(cfg.learning.enabled);
        assert_eq!(cfg.learning.refresh_every, 10);
    }

    #[test]
    fn a_bad_entry_inside_a_table_keeps_its_neighbours() {
        let text = "[cleanup]\nlanguages = [\"en\", 5]\n\
                    [cleanup.app_rules]\n\"Slack\" = \"Casual.\"\n\"org.gnome.Console\" = 1\n";
        let (cfg, warnings) = Config::parse(text).unwrap();
        assert_eq!(
            warnings,
            strings(&[
                "ignored cleanup.app_rules.\"org.gnome.Console\": expected text in quotes",
                "ignored cleanup.languages: expected a list of items that are each text in quotes",
            ])
        );
        assert_eq!(cfg.cleanup.app_rules.len(), 1);
        assert_eq!(cfg.cleanup.app_rules["Slack"], "Casual.");
        assert_eq!(cfg.cleanup.languages, CleanupConfig::default().languages);
    }

    #[test]
    fn a_section_of_the_wrong_shape_is_skipped_whole() {
        let text = "audio = 5\n[cleanup]\ntimeout = { secs = 3 }\nmodel = \"m\"\nstyle = [\"tidy\"]\n";
        let (cfg, warnings) = Config::parse(text).unwrap();
        assert_eq!(
            warnings,
            strings(&[
                "ignored audio: expected a table",
                "ignored cleanup.style: expected text in quotes",
                "ignored cleanup.timeout: expected a number",
            ])
        );
        assert_eq!(cfg.audio, AudioConfig::default());
        assert_eq!(cfg.cleanup.model, "m");
    }

    #[test]
    fn a_file_that_is_not_toml_is_still_an_error() {
        assert!(Config::parse("[cleanup\nmodel = \"x\"\n").is_err());
        assert!(Config::parse("model = \n").is_err());
    }

    #[test]
    fn loading_a_file_reports_what_it_skipped() {
        let (_dir, path) = scratch();
        std::fs::write(&path, "[cleanup]\ntimeout = \"30\"\nmodel = \"kept\"\n").unwrap();
        let (cfg, warnings) = Config::load_from(&path).unwrap();
        assert_eq!(cfg.cleanup.model, "kept");
        assert_eq!(warnings, strings(&["ignored cleanup.timeout: expected a number"]));
    }

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    #[test]
    fn set_value_writes_the_right_section() {
        let (_dir, path) = scratch();
        std::fs::write(&path, "[audio]\nmodel = \"a\"\n\n[stt]\nmodel = \"b\"\n").unwrap();
        set_value_in(&path, "stt", "model", Value::Str("c".into())).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[audio]\nmodel = \"a\""), "{text}");
        assert!(text.contains("model = \"c\""), "{text}");
        assert!(!text.contains("model = \"b\""), "{text}");
    }

    #[test]
    fn set_value_preserves_comments() {
        let (_dir, path) = scratch();
        std::fs::write(
            &path,
            "# top comment\n[cleanup]\n# about style\nstyle = \"balanced\"   # inline\nmodel = \"x\"\n",
        )
        .unwrap();
        set_value_in(&path, "cleanup", "style", Value::Str("tidy".into())).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# top comment"), "{text}");
        assert!(text.contains("# about style"), "{text}");
        assert!(text.contains("style = \"tidy\"   # inline"), "{text}");
    }

    #[test]
    fn set_value_creates_missing_key_and_section() {
        let (_dir, path) = scratch();
        std::fs::write(&path, "[audio]\ndevice = \"\"\n").unwrap();
        set_value_in(&path, "audio", "sample_rate", Value::Int(48000)).unwrap();
        set_value_in(&path, "learning", "enabled", true).unwrap();
        let cfg = load(&path);
        assert_eq!(cfg.audio.sample_rate, 48000);
        assert!(cfg.learning.enabled);
    }

    #[test]
    fn set_value_renders_every_type_and_round_trips() {
        let (_dir, path) = scratch();
        write_default_config_to(&path).unwrap();
        set_value_in(&path, "cleanup", "enabled", false).unwrap();
        set_value_in(&path, "learning", "max_terms", Value::Int(12)).unwrap();
        set_value_in(&path, "cleanup", "timeout", Value::Float(7.5)).unwrap();
        set_value_in(&path, "cleanup", "model", "he said \"hi\"".to_string()).unwrap();
        set_value_in(&path, "cleanup", "dictionary", Value::List(strings(&["Flow", "KVK"]))).unwrap();
        let cfg = load(&path);
        assert!(!cfg.cleanup.enabled);
        assert_eq!(cfg.learning.max_terms, 12);
        assert_eq!(cfg.cleanup.timeout, 7.5);
        assert_eq!(cfg.cleanup.model, "he said \"hi\"");
        assert_eq!(cfg.cleanup.dictionary, vec!["Flow", "KVK"]);
    }

    #[test]
    fn set_value_into_dotted_section() {
        let (_dir, path) = scratch();
        write_default_config_to(&path).unwrap();
        set_value_in(&path, "cleanup.app_rules", "Slack", "Casual.".to_string()).unwrap();
        let cfg = load(&path);
        assert_eq!(cfg.cleanup.app_rules["Slack"], "Casual.");
        // The commented examples above it survive.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# \"Slack\" = \"Casual, no greeting or sign-off.\""));
    }

    #[test]
    fn write_default_does_not_overwrite() {
        let (_dir, path) = scratch();
        std::fs::write(&path, "[audio]\nsample_rate = 1\n").unwrap();
        write_default_config_to(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[audio]\nsample_rate = 1\n");
    }
}
