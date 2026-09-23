//! Learn this speaker's vocabulary and voice from their own dictations.
//!
//! Off unless switched on. When on, Flow builds two things from the local
//! history and feeds both back into the cleanup prompt:
//!
//!   vocabulary - jargon, product and project names a general recogniser
//!                mangles
//!   style      - one instruction describing how this person actually talks
//!
//! Both are mined by the same model that does the cleanup - whichever
//! backend that is - in the background, never on the path between your key
//! release and the paste.

use std::sync::{Arc, LazyLock};

use log::{debug, info, warn};
use regex::Regex;
use serde_json::{json, Value};

use crate::backends::{Backend, BackendError};
use crate::cleanup::strip_thinking;
use crate::history::{History, DEFAULT_SAMPLE_LIMIT};

const VOCAB_KEY: &str = "vocabulary";
const STYLE_KEY: &str = "style";
/// Terms the user has rejected. Mining cannot distinguish jargon from a
/// mis-transcription the speaker later corrected, so the last word is
/// theirs.
const BLOCKED_KEY: &str = "blocked";

/// Mining is background work on a batch of transcripts, so it gets far
/// longer than a dictation would.
const MINING_TIMEOUT_SECS: f32 = 120.0;
/// How many samples go into one prompt.
const BATCH_LIMIT: usize = 120;
/// A "term" longer than this is the model summarising rather than
/// extracting.
const MAX_TERM_CHARS: usize = 40;
/// A style note longer than this is the model rambling and would only
/// dilute the cleanup prompt.
const MAX_STYLE_CHARS: usize = 400;

/// Template for vocabulary mining; `{max_terms}` is the cap.
const VOCAB_PROMPT: &str = r#"You are given transcripts of one person's dictation. Extract only the NAMES a general speech recogniser would get wrong: products, projects, companies, tools, libraries, file formats, commands and people.

Every term you return MUST be copied from the transcripts below. Do not invent terms, and do not repeat anything from these instructions.

Exclude, however often they appear:
- ordinary technical vocabulary any recogniser already knows - "model", "pipeline", "audio", "GPU", "UI", "agent", "transcript", "optimize"
- ordinary verbs and adjectives - "rephrase", "articulate", "glitch"
- anything that looks like a mis-transcription: nonsense strings, or two near-identical variants of the same thing. If you are unsure a term is spelled the way the speaker meant, leave it out.
- anything appearing only once. One mention is not a pattern.

Fewer, better terms beat a long list: every term you return is shown to another model as gospel spelling. At most {max_terms}, most distinctive first.

Return a JSON array of strings and nothing else."#;

const STYLE_PROMPT: &str = r#"You are given transcripts of one person's dictation. Write at most two sentences instructing an editor how this person's WRITTEN text should read, so their voice survives editing.

Critical: these transcripts are speech. Filler words, "uh", "um", "like", stutters, repeated phrases, false starts and fragments are artefacts of speaking and are always removed before your instruction is applied. Never mention them, and never ask for them to be kept - that would undo the cleanup.

Describe only what should survive into the written text: register (formal or casual), whether they swear, characteristic words or openers, typical sentence length, and whether they mix languages.

For example: "This speaker is casual and direct, swears freely, favours short punchy sentences, and drops English technical terms into Dutch."

Return only that instruction, with no preamble."#;

fn vocab_prompt(max_terms: usize) -> String {
    VOCAB_PROMPT.replace("{max_terms}", &max_terms.to_string())
}

static JSON_ARRAY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)\[.*\]").expect("array regex"));

/// Pull a list of terms out of the reply, tolerating stray prose. Blank and
/// over-long entries are dropped, duplicates are matched case-insensitively.
fn parse_terms(text: &str, max_terms: usize) -> Vec<String> {
    let Some(found) = JSON_ARRAY.find(text) else { return Vec::new() };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(found.as_str()) else { return Vec::new() };

    let mut terms: Vec<String> = Vec::new();
    for term in items.iter().filter_map(Value::as_str).map(str::trim) {
        if terms.len() == max_terms {
            break;
        }
        if term.is_empty() || term.chars().count() > MAX_TERM_CHARS {
            continue;
        }
        if !terms.iter().any(|t| t.to_lowercase() == term.to_lowercase()) {
            terms.push(term.to_string());
        }
    }
    terms
}

/// How many times `needle` occurs in `corpus` as a whole word, both already
/// lower-cased. Non-overlapping, like a substring count, but an occurrence
/// only counts where the term's own edges are not glued to more letters:
/// "flow" inside "workflow" is not a use of "flow". Edges that are not
/// alphanumeric themselves ("C++", ".NET") need no boundary on that side.
fn count_word_uses(corpus: &str, needle: &str) -> usize {
    let first_is_word = needle.chars().next().is_some_and(char::is_alphanumeric);
    let last_is_word = needle.chars().next_back().is_some_and(char::is_alphanumeric);
    corpus
        .match_indices(needle)
        .filter(|(start, _)| {
            let before_ok =
                !first_is_word || !corpus[..*start].chars().next_back().is_some_and(char::is_alphanumeric);
            let after_ok = !last_is_word
                || !corpus[start + needle.len()..].chars().next().is_some_and(char::is_alphanumeric);
            before_ok && after_ok
        })
        .count()
}

/// Keep only terms the speaker demonstrably used.
///
/// Models parrot their own instructions: given example terms in the prompt,
/// the first version returned five names that appear nowhere in this user's
/// history. Learned vocabulary is handed to another model as correct
/// spelling, so a fabricated term becomes a word Flow will happily insert
/// into text the user never said. Checking against the source closes that
/// off regardless of what the prompt says.
///
/// Uses are counted as whole words, case-insensitively. The Python counted
/// substrings, which let a short term be vouched for by unrelated longer
/// words it happened to sit inside.
fn verify_terms(terms: &[String], samples: &[String], min_uses: usize) -> Vec<String> {
    let corpus = samples.join("\n").to_lowercase();
    terms
        .iter()
        .filter(|term| {
            let needle = term.trim().to_lowercase();
            if needle.is_empty() {
                return false;
            }
            let uses = count_word_uses(&corpus, &needle);
            if uses < min_uses {
                debug!("dropping {term:?} - appears {uses} time(s) in history");
            }
            uses >= min_uses
        })
        .cloned()
        .collect()
}

fn batch(samples: &[String], limit: usize) -> String {
    samples.iter().take(limit).map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n")
}

/// Everything stored under a profile key, when it is a list of strings.
fn string_list(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

pub struct Learner {
    backend: Arc<dyn Backend>,
}

impl Learner {
    pub fn new(backend: Arc<dyn Backend>) -> Learner {
        Learner { backend }
    }

    /// One mining call. Never with reasoning: a batch of transcripts is
    /// long, and the answer is a list, not an argument.
    fn ask(&self, system: &str, prompt: &str) -> Result<String, BackendError> {
        let reply = self.backend.complete(system, prompt, MINING_TIMEOUT_SECS, false)?;
        Ok(strip_thinking(&reply))
    }

    fn mine_vocabulary(&self, samples: &[String], max_terms: usize) -> Vec<String> {
        if samples.is_empty() {
            return Vec::new();
        }
        let reply = match self.ask(&vocab_prompt(max_terms), &batch(samples, BATCH_LIMIT)) {
            Ok(reply) => reply,
            // Learning must never break dictation.
            Err(err) => {
                warn!("vocabulary mining failed: {err}");
                return Vec::new();
            }
        };

        let proposed = parse_terms(&reply, max_terms);
        let terms = verify_terms(&proposed, samples, 2);
        if terms.len() < proposed.len() {
            info!("dropped {} proposed term(s) not found in the history", proposed.len() - terms.len());
        }
        terms
    }

    fn profile_style(&self, samples: &[String]) -> String {
        if samples.is_empty() {
            return String::new();
        }
        let reply = match self.ask(STYLE_PROMPT, &batch(samples, BATCH_LIMIT)) {
            Ok(reply) => reply,
            Err(err) => {
                warn!("style profiling failed: {err}");
                return String::new();
            }
        };

        // One or two sentences; anything longer is the model rambling and
        // will only dilute the cleanup prompt.
        let collapsed = reply.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.chars().take(MAX_STYLE_CHARS).collect()
    }

    /// Re-mine both halves of the profile and store them.
    ///
    /// Returns the profile now in effect, `(terms, style_note)`. A half that
    /// failed or came back empty keeps what was learned before, both in the
    /// store and in what is returned, so one flaky model call cannot wipe a
    /// profile built up over weeks. Both are empty only when nothing has
    /// ever been learned.
    pub fn refresh(&self, history: &History, max_terms: usize) -> anyhow::Result<(Vec<String>, String)> {
        let samples = history.samples_for_learning(DEFAULT_SAMPLE_LIMIT)?;
        if samples.is_empty() {
            return Ok((Vec::new(), String::new()));
        }

        let terms = self.mine_vocabulary(&samples, max_terms);
        let blocked: Vec<String> =
            string_list(history.get_profile(BLOCKED_KEY)?).iter().map(|t| t.to_lowercase()).collect();
        let terms: Vec<String> = terms.into_iter().filter(|t| !blocked.contains(&t.to_lowercase())).collect();
        let style = self.profile_style(&samples);
        info!("learned {} terms and a style note from {} dictations", terms.len(), samples.len());

        let terms = if terms.is_empty() {
            string_list(history.get_profile(VOCAB_KEY)?)
        } else {
            history.set_profile(VOCAB_KEY, &json!(terms), samples.len() as u64)?;
            terms
        };
        let style = if style.is_empty() {
            match history.get_profile(STYLE_KEY)? {
                Some(Value::String(note)) => note,
                _ => String::new(),
            }
        } else {
            history.set_profile(STYLE_KEY, &json!(style), samples.len() as u64)?;
            style
        };
        Ok((terms, style))
    }
}

/// What has been learned so far: `(terms, style_note)`, empty on a fresh
/// store or when the store cannot be read.
pub fn load_profile(history: &History) -> (Vec<String>, String) {
    let read = |key: &str| {
        history.get_profile(key).unwrap_or_else(|err| {
            warn!("could not read the learned {key}: {err}");
            None
        })
    };
    let terms = string_list(read(VOCAB_KEY));
    let style = match read(STYLE_KEY) {
        Some(Value::String(note)) => note,
        _ => String::new(),
    };
    (terms, style)
}

/// Drop a term and remember never to learn it again. Returns whether it was
/// in the vocabulary.
///
/// The remaining vocabulary keeps its `samples` count - how many dictations
/// it was mined from - which curating it does not change.
pub fn forget_term(history: &History, term: &str) -> anyhow::Result<bool> {
    let lower = term.to_lowercase();

    let mut blocked = string_list(history.get_profile(BLOCKED_KEY)?);
    if !blocked.iter().any(|t| t.to_lowercase() == lower) {
        blocked.push(term.to_string());
        history.set_profile(BLOCKED_KEY, &json!(blocked), 0)?;
    }

    let terms = string_list(history.get_profile(VOCAB_KEY)?);
    let kept: Vec<&String> = terms.iter().filter(|t| t.to_lowercase() != lower).collect();
    let samples = history.profile_samples(VOCAB_KEY)?.unwrap_or(0);
    history.set_profile(VOCAB_KEY, &json!(kept), samples)?;
    Ok(kept.len() != terms.len())
}

pub fn blocked_terms(history: &History) -> anyhow::Result<Vec<String>> {
    Ok(string_list(history.get_profile(BLOCKED_KEY)?))
}

#[cfg(test)]
mod tests {
    //! The mining guards and the toggle's blast radius.
    //!
    //! The dangerous property here is that a learned term is handed to
    //! another model as correct spelling. A fabricated one becomes a word
    //! Flow will insert into text the user never said, so the guards matter
    //! more than the mining.

    use super::*;
    use crate::backends::OllamaBackend;
    use crate::engine::FocusContext;
    use crate::test_util::{fnv1a, history as open_store, strings};
    use std::sync::Mutex;

    fn record(store: &History, raw: &str, clean: &str) {
        store.record(raw, clean, &FocusContext::default()).unwrap();
    }

    /// Answers the vocabulary prompt with one reply and the style prompt with
    /// another, recording every call.
    struct Fake {
        vocab: String,
        style: String,
        calls: Mutex<Vec<(String, String, f32, bool)>>,
    }

    impl Fake {
        fn new(vocab: &str, style: &str) -> Arc<Fake> {
            Arc::new(Fake { vocab: vocab.into(), style: style.into(), calls: Mutex::new(Vec::new()) })
        }
    }

    /// Answers one of the two prompts and fails the other.
    struct HalfBroken {
        broken: &'static str,
        reply: String,
    }

    impl Backend for HalfBroken {
        fn complete(&self, system: &str, _: &str, _: f32, _: bool) -> Result<String, BackendError> {
            let is_style = system == STYLE_PROMPT;
            if is_style == (self.broken == "style") {
                return Err(BackendError("timed out".into()));
            }
            Ok(self.reply.clone())
        }
        fn available(&self) -> (bool, String) {
            (true, "ok".into())
        }
        fn label(&self) -> String {
            "HalfBroken".into()
        }
    }

    impl Backend for Fake {
        fn complete(
            &self,
            system: &str,
            prompt: &str,
            timeout_secs: f32,
            thinking: bool,
        ) -> Result<String, BackendError> {
            self.calls.lock().unwrap().push((system.into(), prompt.into(), timeout_secs, thinking));
            Ok(if system == STYLE_PROMPT { self.style.clone() } else { self.vocab.clone() })
        }
        fn available(&self) -> (bool, String) {
            (true, "ok".into())
        }
        fn label(&self) -> String {
            "Fake".into()
        }
    }

    // -- the guard that matters -----------------------------------------------

    #[test]
    fn verify_drops_terms_never_actually_said() {
        // Models parrot their own instructions. The first version of the
        // prompt listed example terms and got five of them back, none of
        // which appeared in the user's history.
        let samples = strings(&["I use Whisper Flow every day", "Whisper Flow is great"]);
        let proposed = strings(&["Whisper Flow", "PufferLib", "Ptyxis"]);
        assert_eq!(verify_terms(&proposed, &samples, 2), strings(&["Whisper Flow"]));
    }

    #[test]
    fn verify_requires_more_than_one_use() {
        let samples = strings(&["I mentioned Figma once", "nothing else here"]);
        assert_eq!(verify_terms(&strings(&["Figma"]), &samples, 2), Vec::<String>::new());
        assert_eq!(verify_terms(&strings(&["Figma"]), &samples, 1), strings(&["Figma"]));
    }

    #[test]
    fn verify_is_case_insensitive() {
        let samples = strings(&["the SCANNER broke", "fix the scanner"]);
        assert_eq!(verify_terms(&strings(&["Scanner"]), &samples, 2), strings(&["Scanner"]));
    }

    #[test]
    fn verify_handles_an_empty_corpus() {
        assert_eq!(verify_terms(&strings(&["anything"]), &[], 2), Vec::<String>::new());
    }

    #[test]
    fn verify_counts_whole_words_only() {
        // "Flow" must not be vouched for by "workflow" and "overflow".
        let samples = strings(&["the workflow is fine", "buffer overflow again"]);
        assert_eq!(verify_terms(&strings(&["Flow"]), &samples, 2), Vec::<String>::new());
        let samples = strings(&["Flow pastes it", "I like flow, a lot"]);
        assert_eq!(verify_terms(&strings(&["Flow"]), &samples, 2), strings(&["Flow"]));
        // Terms whose edges are punctuation still count next to spaces.
        let samples = strings(&["we write C++ here", "C++ is fast", "use .NET or .NET Core"]);
        assert_eq!(verify_terms(&strings(&["C++", ".NET"]), &samples, 2), strings(&["C++", ".NET"]));
        // Adjacent repeats are each counted.
        assert_eq!(count_word_uses("cafe cafe cafe", "cafe"), 3);
        assert_eq!(count_word_uses("cafecafe", "cafe"), 0);
    }

    // -- parsing the model's reply --------------------------------------------

    #[test]
    fn parses_a_json_array() {
        assert_eq!(parse_terms("[\"a\", \"b\"]", 10), strings(&["a", "b"]));
    }

    #[test]
    fn parses_an_array_wrapped_in_prose() {
        assert_eq!(
            parse_terms("Sure! Here you go:\n[\"a\", \"b\"]\nHope that helps", 10),
            strings(&["a", "b"])
        );
    }

    #[test]
    fn parse_survives_junk() {
        assert_eq!(parse_terms("no json here", 10), Vec::<String>::new());
        assert_eq!(parse_terms("[not valid json", 10), Vec::<String>::new());
        assert_eq!(parse_terms("[1, null, {\"a\": 1}, \"  \"]", 10), Vec::<String>::new());
    }

    #[test]
    fn parse_drops_sentences_masquerading_as_terms() {
        let long = "this is clearly a summary sentence and not a vocabulary term at all";
        assert_eq!(parse_terms(&format!("[\"ok\", \"{long}\"]"), 10), strings(&["ok"]));
    }

    #[test]
    fn parse_deduplicates_and_caps() {
        assert_eq!(parse_terms("[\"a\", \"A\", \"b\", \"c\"]", 2), strings(&["a", "b"]));
    }

    // -- curation -------------------------------------------------------------

    #[test]
    fn forgetting_removes_and_blocks() {
        let (_dir, store) = open_store();
        store.set_profile(VOCAB_KEY, &json!(["Cafe", "Whisper Flow"]), 0).unwrap();
        assert!(forget_term(&store, "Cafe").unwrap());
        assert_eq!(store.get_profile(VOCAB_KEY).unwrap(), Some(json!(["Whisper Flow"])));
        assert_eq!(blocked_terms(&store).unwrap(), strings(&["Cafe"]));
        // Forgetting it twice does not block it twice.
        assert!(!forget_term(&store, "cafe").unwrap());
        assert_eq!(blocked_terms(&store).unwrap(), strings(&["Cafe"]));
    }

    #[test]
    fn blocking_something_not_learned_still_records_it() {
        let (_dir, store) = open_store();
        store.set_profile(VOCAB_KEY, &json!(["Whisper Flow"]), 0).unwrap();
        assert!(!forget_term(&store, "Nonsense").unwrap());
        assert_eq!(blocked_terms(&store).unwrap(), strings(&["Nonsense"]));
    }

    #[test]
    fn forgetting_keeps_the_sample_count() {
        // The count says how many dictations the vocabulary was mined from;
        // curating the list does not change that.
        let (_dir, store) = open_store();
        store.set_profile(VOCAB_KEY, &json!(["Cafe", "Ptyxis"]), 57).unwrap();
        forget_term(&store, "Cafe").unwrap();
        assert_eq!(store.profile_samples(VOCAB_KEY).unwrap(), Some(57));
        // With no vocabulary yet, forgetting writes an empty one with no
        // samples, and the term is still blocked.
        let (_dir, fresh) = open_store();
        assert!(!forget_term(&fresh, "Cafe").unwrap());
        assert_eq!(fresh.get_profile(VOCAB_KEY).unwrap(), Some(json!([])));
        assert_eq!(fresh.profile_samples(VOCAB_KEY).unwrap(), Some(0));
    }

    #[test]
    fn blocked_terms_are_not_relearned() {
        let (_dir, store) = open_store();
        record(&store, "the Cafe thing", "the Cafe thing and Cafe again");
        store.set_profile(BLOCKED_KEY, &json!(["Cafe"]), 0).unwrap();

        let backend = Fake::new("[\"Cafe\"]", "casual");
        let learner = Learner::new(backend.clone());
        let (terms, style) = learner.refresh(&store, 40).unwrap();
        assert_eq!(terms, Vec::<String>::new());
        assert_eq!(style, "casual");
        // Nothing learned means no vocabulary row; the style is stored.
        assert_eq!(store.get_profile(VOCAB_KEY).unwrap(), None);
        assert_eq!(store.get_profile(STYLE_KEY).unwrap(), Some(json!("casual")));
    }

    #[test]
    fn refresh_stores_both_halves_with_the_sample_count() {
        let (_dir, store) = open_store();
        for _ in 0..3 {
            record(&store, "raw", "Ptyxis is the terminal I use; Ptyxis again");
        }
        record(&store, "raw", "   ");
        let backend =
            Fake::new("<think>hmm</think>Here: [\"Ptyxis\", \"Imaginary\"]", "  Casual,\n\n direct.  ");
        let learner = Learner::new(backend.clone());
        let (terms, style) = learner.refresh(&store, 40).unwrap();
        assert_eq!(terms, strings(&["Ptyxis"]));
        assert_eq!(style, "Casual, direct.");
        assert_eq!(store.profile_samples(VOCAB_KEY).unwrap(), Some(3));
        assert_eq!(store.profile_samples(STYLE_KEY).unwrap(), Some(3));
        assert_eq!(load_profile(&store), (strings(&["Ptyxis"]), "Casual, direct.".into()));

        // Both calls went through the backend without reasoning, at the
        // mining timeout, with the samples as a bulleted batch.
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, vocab_prompt(40));
        assert_eq!(calls[1].0, STYLE_PROMPT);
        for (_, prompt, timeout, thinking) in calls.iter() {
            assert_eq!(prompt, "- Ptyxis is the terminal I use; Ptyxis again\n- Ptyxis is the terminal I use; Ptyxis again\n- Ptyxis is the terminal I use; Ptyxis again");
            assert_eq!(*timeout, MINING_TIMEOUT_SECS);
            assert!(!thinking);
        }
    }

    #[test]
    fn a_failed_half_keeps_what_was_learned_before() {
        let (_dir, store) = open_store();
        for _ in 0..3 {
            record(&store, "raw", "Ptyxis and Tauri, Ptyxis and Tauri");
        }
        store.set_profile(VOCAB_KEY, &json!(["Ptyxis"]), 3).unwrap();
        store.set_profile(STYLE_KEY, &json!("Old note."), 3).unwrap();

        // Vocabulary mining fails, the style note succeeds.
        let learner = Learner::new(Arc::new(HalfBroken { broken: "vocabulary", reply: "New note.".into() }));
        let (terms, style) = learner.refresh(&store, 40).unwrap();
        assert_eq!(terms, strings(&["Ptyxis"]));
        assert_eq!(style, "New note.");
        assert_eq!(load_profile(&store), (strings(&["Ptyxis"]), "New note.".into()));

        // And the other way round.
        let learner = Learner::new(Arc::new(HalfBroken { broken: "style", reply: "[\"Tauri\"]".into() }));
        let (terms, style) = learner.refresh(&store, 40).unwrap();
        assert_eq!(terms, strings(&["Tauri"]));
        assert_eq!(style, "New note.");
        assert_eq!(load_profile(&store), (strings(&["Tauri"]), "New note.".into()));
    }

    #[test]
    fn refresh_with_no_history_learns_nothing() {
        let (_dir, store) = open_store();
        let backend = Fake::new("[\"x\"]", "y");
        assert_eq!(Learner::new(backend.clone()).refresh(&store, 40).unwrap(), (Vec::new(), String::new()));
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn style_note_is_collapsed_and_truncated() {
        let long = "word ".repeat(200);
        let backend = Fake::new("[]", &long);
        let note = Learner::new(backend).profile_style(&strings(&["a sample"]));
        assert_eq!(note.chars().count(), MAX_STYLE_CHARS);
        assert!(!note.contains("  "));
    }

    // -- failure must never break dictation -------------------------------------

    #[test]
    fn mining_survives_an_unreachable_model() {
        let backend: Arc<dyn Backend> = Arc::new(OllamaBackend::new("model", "http://127.0.0.1:1", "1h"));
        let learner = Learner::new(backend);
        assert_eq!(learner.mine_vocabulary(&strings(&["something"]), 40), Vec::<String>::new());
        assert_eq!(learner.profile_style(&strings(&["something"])), "");
        // And with nothing to learn from, the model is not even asked.
        assert_eq!(learner.mine_vocabulary(&[], 40), Vec::<String>::new());
        assert_eq!(learner.profile_style(&[]), "");
    }

    #[test]
    fn load_profile_on_a_fresh_store() {
        let (_dir, store) = open_store();
        assert_eq!(load_profile(&store), (Vec::new(), String::new()));
    }

    // -- the prompts are the Python's, byte for byte ------------------------------

    #[test]
    fn mining_prompts_match_the_python_byte_for_byte() {
        let vocab = vocab_prompt(40);
        assert_eq!((vocab.len(), fnv1a(&vocab)), (1037, 0x8a84e756d10c6900));
        assert_eq!((STYLE_PROMPT.len(), fnv1a(STYLE_PROMPT)), (869, 0x0cb0146a91cbe618));
    }
}
