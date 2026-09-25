//! Local history of dictations, and the profile learned from it.
//!
//! Nothing is written unless learning is switched on. With it off, Rustle keeps
//! no record of anything you say - the transcript exists only long enough to
//! be pasted. Everything here is a local SQLite file; nothing leaves the
//! machine.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::engine::FocusContext;

/// How many recent dictations the learner reads, unless told otherwise.
pub const DEFAULT_SAMPLE_LIMIT: usize = 200;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS dictation (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    at     TEXT    NOT NULL,
    app    TEXT    NOT NULL DEFAULT '',
    title  TEXT    NOT NULL DEFAULT '',
    raw    TEXT    NOT NULL,
    clean  TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS dictation_at ON dictation(at);

CREATE TABLE IF NOT EXISTS profile (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    samples    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
";

/// One stored dictation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dictation {
    /// UTC, ISO-8601 to the second.
    pub at: String,
    pub app: String,
    pub title: String,
    pub raw: String,
    pub clean: String,
}

pub struct History {
    path: PathBuf,
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn read_generation(db: &Connection) -> rusqlite::Result<u64> {
    let value: Option<i64> =
        db.query_row("SELECT value FROM meta WHERE key = 'generation'", [], |row| row.get(0)).optional()?;
    Ok(value.unwrap_or(0).max(0) as u64)
}

fn upsert_profile(
    db: &Connection,
    key: &str,
    value: &serde_json::Value,
    samples: u64,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO profile (key, value, updated_at, samples) VALUES (?,?,?,?) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value, \
         updated_at=excluded.updated_at, samples=excluded.samples",
        params![key, value.to_string(), now_iso(), samples as i64],
    )?;
    Ok(())
}

impl History {
    pub fn open_default() -> anyhow::Result<History> {
        Self::open(crate::config::data_dir().join("history.db"))
    }

    /// Create the file and its directory if needed, and apply the schema.
    pub fn open(path: PathBuf) -> anyhow::Result<History> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let history = History { path };
        history.connect()?.execute_batch(SCHEMA)?;
        Ok(history)
    }

    /// A new connection per operation: the engine writes from its worker and
    /// reads from the learner's, and a connection is not shareable between
    /// threads.
    fn connect(&self) -> rusqlite::Result<Connection> {
        let db = Connection::open(&self.path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        // SQLite normally only marks deleted rows free, leaving the words in
        // the file until something happens to overwrite them. This zeroes
        // them instead - for a clear, and for a profile entry rewritten when
        // a term is forgotten.
        db.pragma_update(None, "secure_delete", true)?;
        Ok(db)
    }

    // -- dictations ---------------------------------------------------------

    pub fn record(&self, raw: &str, clean: &str, context: &FocusContext) -> anyhow::Result<()> {
        self.connect()?.execute(
            "INSERT INTO dictation (at, app, title, raw, clean) VALUES (?,?,?,?,?)",
            params![now_iso(), context.app, context.title, raw, clean],
        )?;
        Ok(())
    }

    pub fn count(&self) -> anyhow::Result<u64> {
        let count: u64 = self.connect()?.query_row("SELECT COUNT(*) FROM dictation", [], |row| row.get(0))?;
        Ok(count)
    }

    /// The newest dictations first.
    pub fn recent(&self, limit: usize) -> anyhow::Result<Vec<Dictation>> {
        let db = self.connect()?;
        let mut statement =
            db.prepare("SELECT at, app, title, raw, clean FROM dictation ORDER BY id DESC LIMIT ?")?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(Dictation {
                at: row.get(0)?,
                app: row.get(1)?,
                title: row.get(2)?,
                raw: row.get(3)?,
                clean: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Cleaned text of recent dictations - what the speaker actually meant,
    /// rather than what the recogniser first guessed. Newest first, blanks
    /// left out.
    pub fn samples_for_learning(&self, limit: usize) -> anyhow::Result<Vec<String>> {
        let db = self.connect()?;
        let mut statement = db.prepare("SELECT clean FROM dictation ORDER BY id DESC LIMIT ?")?;
        let samples = statement.query_map([limit as i64], |row| row.get::<_, String>(0))?;
        let samples = samples.collect::<Result<Vec<String>, _>>()?;
        Ok(samples.into_iter().filter(|s| !s.trim().is_empty()).collect())
    }

    /// Delete every dictation and the whole learned profile. Returns how many
    /// dictations were removed.
    pub fn clear(&self) -> anyhow::Result<u64> {
        let mut db = self.connect()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed: u64 = tx.query_row("SELECT COUNT(*) FROM dictation", [], |row| row.get(0))?;
        tx.execute("DELETE FROM dictation", [])?;
        tx.execute("DELETE FROM profile", [])?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('generation', 1) \
             ON CONFLICT(key) DO UPDATE SET value = value + 1",
            [],
        )?;
        tx.commit()?;
        // "Delete everything" should leave nothing to recover: secure_delete
        // zeroed the rows, and rebuilding the file hands the emptied pages
        // back rather than keeping them around.
        db.execute_batch("VACUUM")?;
        Ok(removed)
    }

    /// How many times the history has been cleared. Work that reads the
    /// history and writes back what it learned much later (a profile
    /// refresh asks a model) notes this first, so that a clear in between
    /// is not undone.
    pub fn generation(&self) -> anyhow::Result<u64> {
        Ok(read_generation(&self.connect()?)?)
    }

    // -- learned profile ----------------------------------------------------

    /// Insert or replace one profile entry; `samples` is how many dictations
    /// it was learned from.
    pub fn set_profile(&self, key: &str, value: &serde_json::Value, samples: u64) -> anyhow::Result<()> {
        upsert_profile(&self.connect()?, key, value, samples)?;
        Ok(())
    }

    /// Store `(key, value, samples)` entries together, unless the history
    /// has been cleared since `generation`. Returns whether they were stored.
    pub fn set_profile_unless_cleared(
        &self,
        generation: u64,
        entries: &[(&str, serde_json::Value, u64)],
    ) -> anyhow::Result<bool> {
        let mut db = self.connect()?;
        // Immediate, so no clear can land between the check and the writes.
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if read_generation(&tx)? != generation {
            return Ok(false);
        }
        for (key, value, samples) in entries {
            upsert_profile(&tx, key, value, *samples)?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// `None` when nothing has been stored under the key.
    pub fn get_profile(&self, key: &str) -> anyhow::Result<Option<serde_json::Value>> {
        let stored: Option<String> = self
            .connect()?
            .query_row("SELECT value FROM profile WHERE key = ?", [key], |row| row.get(0))
            .optional()?;
        Ok(stored.as_deref().map(serde_json::from_str).transpose()?)
    }

    /// How many dictations the entry was learned from; `None` when there is
    /// no entry.
    pub fn profile_samples(&self, key: &str) -> anyhow::Result<Option<u64>> {
        let samples: Option<i64> = self
            .connect()?
            .query_row("SELECT samples FROM profile WHERE key = ?", [key], |row| row.get(0))
            .optional()?;
        Ok(samples.map(|n| n.max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    //! The store half of the learning tests.
    use super::*;
    use crate::test_util::history as store;
    use serde_json::json;

    fn context(app: &str, title: &str) -> FocusContext {
        FocusContext { app: app.into(), title: title.into(), role: String::new() }
    }

    #[test]
    fn records_and_counts() {
        let (_dir, store) = store();
        assert_eq!(store.count().unwrap(), 0);
        store.record("um hello there", "Hello there.", &context("Slack", "#eng")).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        let row = store.recent(1).unwrap().remove(0);
        assert_eq!(row.raw, "um hello there");
        assert_eq!(row.clean, "Hello there.");
        assert_eq!(row.app, "Slack");
        assert_eq!(row.title, "#eng");
    }

    #[test]
    fn a_clear_turns_away_profile_writes_begun_before_it() {
        let (_dir, store) = store();
        let before = store.generation().unwrap();
        store.clear().unwrap();
        let after = store.generation().unwrap();
        assert_ne!(before, after);

        let entry = [("vocabulary", json!(["Ptyxis"]), 3)];
        assert!(!store.set_profile_unless_cleared(before, &entry).unwrap());
        assert_eq!(store.get_profile("vocabulary").unwrap(), None);
        assert!(store.set_profile_unless_cleared(after, &entry).unwrap());
        assert_eq!(store.get_profile("vocabulary").unwrap(), Some(json!(["Ptyxis"])));
    }

    #[test]
    fn timestamps_are_utc_iso_seconds() {
        let (_dir, store) = store();
        store.record("a", "b", &FocusContext::default()).unwrap();
        let at = store.recent(1).unwrap().remove(0).at;
        // 2026-09-21T10:11:12+00:00 - what Python's isoformat(timespec="seconds") wrote.
        assert_eq!(at.len(), 25, "{at}");
        assert!(at.ends_with("+00:00"), "{at}");
        assert_eq!(&at[10..11], "T");
        chrono::DateTime::parse_from_rfc3339(&at).unwrap();
    }

    #[test]
    fn recent_is_newest_first_and_limited() {
        let (_dir, store) = store();
        for word in ["one", "two", "three"] {
            store.record(word, word, &FocusContext::default()).unwrap();
        }
        let cleaned: Vec<String> = store.recent(2).unwrap().into_iter().map(|d| d.clean).collect();
        assert_eq!(cleaned, vec!["three", "two"]);
    }

    #[test]
    fn learning_reads_the_cleaned_text() {
        // Mining from raw would teach it the speaker's disfluencies.
        let (_dir, store) = store();
        store.record("um so like the scanner", "The scanner.", &FocusContext::default()).unwrap();
        assert_eq!(store.samples_for_learning(DEFAULT_SAMPLE_LIMIT).unwrap(), vec!["The scanner."]);
    }

    #[test]
    fn blank_cleanups_are_not_samples() {
        let (_dir, store) = store();
        store.record("first", "First.", &FocusContext::default()).unwrap();
        store.record("um", "   ", &FocusContext::default()).unwrap();
        store.record("second", "Second.", &FocusContext::default()).unwrap();
        assert_eq!(store.samples_for_learning(DEFAULT_SAMPLE_LIMIT).unwrap(), vec!["Second.", "First."]);
        assert_eq!(store.samples_for_learning(1).unwrap(), vec!["Second."]);
    }

    #[test]
    fn clear_removes_dictations_and_profile() {
        let (_dir, store) = store();
        store.record("a", "b", &FocusContext::default()).unwrap();
        store.set_profile("vocabulary", &json!(["thing"]), 0).unwrap();
        assert_eq!(store.clear().unwrap(), 1);
        assert_eq!(store.count().unwrap(), 0);
        assert_eq!(store.get_profile("vocabulary").unwrap(), None);
    }

    /// The raw bytes of the database file.
    fn file_bytes(store: &History) -> Vec<u8> {
        std::fs::read(&store.path).unwrap()
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn clearing_leaves_no_text_in_the_file() {
        // A plain DELETE only marks rows free, and the words stay readable in
        // the file until something happens to overwrite them.
        let (_dir, store) = store();
        for i in 0..50 {
            store
                .record(
                    &format!("raw secret {i}"),
                    &format!("Marmalade-{i} ledger."),
                    &context("Slack", "#dm"),
                )
                .unwrap();
        }
        store.set_profile("vocabulary", &json!(["Marmalade"]), 50).unwrap();
        assert!(contains(&file_bytes(&store), "Marmalade-7 ledger."));
        store.clear().unwrap();
        let bytes = file_bytes(&store);
        assert!(!contains(&bytes, "Marmalade"));
        assert!(!contains(&bytes, "raw secret"));
        // Still a working store afterwards.
        store.record("a", "b", &FocusContext::default()).unwrap();
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn rewriting_a_profile_entry_leaves_no_old_value_behind() {
        // Forgetting a term rewrites the vocabulary; the old list must not
        // linger in a free page.
        // The old list is much longer than the new one, so the new row cannot
        // happen to land on top of it.
        let (_dir, store) = store();
        let mut terms = vec!["Zanzibar-Quokka".to_string()];
        terms.extend((0..30).map(|i| format!("Filler-term-{i}")));
        store.set_profile("vocabulary", &json!(terms), 3).unwrap();
        store.set_profile("vocabulary", &json!(["Ptyxis"]), 3).unwrap();
        assert!(!contains(&file_bytes(&store), "Zanzibar-Quokka"));
    }

    #[test]
    fn profile_round_trips() {
        let (_dir, store) = store();
        store.set_profile("vocabulary", &json!(["Ptyxis", "interopt"]), 42).unwrap();
        assert_eq!(store.get_profile("vocabulary").unwrap(), Some(json!(["Ptyxis", "interopt"])));
        assert_eq!(store.profile_samples("vocabulary").unwrap(), Some(42));
        // Upsert: the second write replaces the first.
        store.set_profile("vocabulary", &json!(["Ptyxis"]), 7).unwrap();
        assert_eq!(store.get_profile("vocabulary").unwrap(), Some(json!(["Ptyxis"])));
        assert_eq!(store.profile_samples("vocabulary").unwrap(), Some(7));
    }

    #[test]
    fn missing_profile_returns_the_default() {
        let (_dir, store) = store();
        assert_eq!(store.get_profile("nope").unwrap(), None);
        assert_eq!(store.profile_samples("nope").unwrap(), None);
    }

    #[test]
    fn reopening_keeps_the_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.db");
        History::open(path.clone()).unwrap().record("a", "b", &FocusContext::default()).unwrap();
        assert_eq!(History::open(path).unwrap().count().unwrap(), 1);
    }
}
