//! `rustle eval`: run the cleanup pass over tricky transcripts and check what
//! it does. The cases live in `eval/cases.toml`, compiled into the binary so
//! the check works from an installed copy too.

use std::time::Instant;

use rustle_core::cleanup::build_cleaner;
use rustle_core::config::Config;
use rustle_core::engine::FocusContext;
use serde::Deserialize;

const CASES_TOML: &str = include_str!("../../eval/cases.toml");

#[derive(Debug, Deserialize)]
struct File {
    profile: Profile,
    #[serde(rename = "case")]
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    terms: Vec<String>,
    style: String,
}

#[derive(Debug, Deserialize)]
pub struct Case {
    pub name: String,
    pub raw: String,
    #[serde(default)]
    pub must: Vec<String>,
    #[serde(default)]
    pub must_not: Vec<String>,
    pub max_words: Option<usize>,
    pub min_sentences: Option<usize>,
}

#[cfg(test)]
fn cases() -> anyhow::Result<Vec<Case>> {
    Ok(toml::from_str::<File>(CASES_TOML)?.cases)
}

pub fn run(config: &Config, filter: Option<&str>) -> anyhow::Result<()> {
    let file: File = toml::from_str(CASES_TOML)?;
    let cleaner = build_cleaner(&config.cleanup);

    if std::env::var("RUSTLE_EVAL_PROFILE").as_deref() == Ok("1") {
        cleaner.set_profile(file.profile.terms.clone(), file.profile.style.clone());
        println!("[profile injected]\n");
    }

    let (ok, why) = cleaner.available();
    if !ok {
        anyhow::bail!("{why}");
    }
    cleaner.warm_up();
    println!(
        "model: {}  backend: {}  think: {}\n",
        config.cleanup.model, config.cleanup.backend, config.cleanup.think
    );

    let context =
        FocusContext { app: "org.gnome.TextEditor".into(), title: "notes".into(), role: String::new() };
    let mut failures = 0;
    let mut total = 0;

    for case in file.cases.iter().filter(|c| filter.map(|f| c.name.contains(f)).unwrap_or(true)) {
        total += 1;
        let started = Instant::now();
        let out = cleaner.clean(&case.raw, &context);
        let elapsed = started.elapsed().as_secs_f32();
        let low = out.to_lowercase();

        let missing: Vec<&String> = case.must.iter().filter(|m| !low.contains(&m.to_lowercase())).collect();
        let present: Vec<&String> =
            case.must_not.iter().filter(|m| low.contains(&m.to_lowercase())).collect();
        let words = out.split_whitespace().count();
        let too_long = case.max_words.map(|cap| words > cap).unwrap_or(false);
        let sentences = out.split(['.', '!', '?']).filter(|p| !p.trim().is_empty()).count();
        let too_few = case.min_sentences.map(|want| sentences < want).unwrap_or(false);

        let passed = missing.is_empty() && present.is_empty() && !too_long && !too_few;
        if !passed {
            failures += 1;
        }
        println!("[{}] {elapsed:5.2}s  {}", if passed { "PASS" } else { "FAIL" }, case.name);
        println!("         in:  {}", case.raw.chars().take(100).collect::<String>());
        println!("         out: {out}");
        if !missing.is_empty() {
            println!("         !! missing: {missing:?}");
        }
        if !present.is_empty() {
            println!("         !! should not contain: {present:?}");
        }
        if too_few {
            println!("         !! only {sentences} sentence(s), wanted {}", case.min_sentences.unwrap());
        }
        if too_long {
            println!("         !! {words} words, capped at {}", case.max_words.unwrap());
        }
        println!();
    }

    println!("{}/{} passed", total - failures, total);
    if failures > 0 {
        anyhow::bail!("{failures} case(s) failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn cases_parse_and_are_complete() {
        let cases = super::cases().unwrap();
        assert_eq!(cases.len(), 33);
        assert!(cases.iter().all(|c| !c.raw.is_empty() && !c.name.is_empty()));
        assert!(cases.iter().filter(|c| c.name.starts_with("dutch")).count() >= 4);
        let others = ["german", "french", "spanish", "italian", "polish"];
        assert_eq!(cases.iter().filter(|c| others.iter().any(|l| c.name.starts_with(l))).count(), 5);
    }
}
