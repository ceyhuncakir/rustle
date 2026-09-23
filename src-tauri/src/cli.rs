//! `flow <subcommand>`: the diagnostic and headless surface, no windows.

use std::sync::Arc;

use clap::{Parser, Subcommand};
use flow_core::config::{self, Config};
use flow_core::history::History;
use flow_core::learning::{self, Learner};
use flow_core::models::{self, Precision};

use crate::host::Shared;

#[derive(Parser, Debug)]
#[command(
    name = "flow",
    version,
    about = "Local offline dictation: speak, and cleaned-up text lands in the focused app."
)]
pub struct Cli {
    /// Run the engine without any windows (the systemd path on GNOME).
    #[arg(long)]
    pub headless: bool,
    /// Start minimised (used by launch-at-login).
    #[arg(long, hide = true)]
    pub minimized: bool,
    #[arg(short, long, global = true)]
    pub verbose: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Check every moving part and say what is wrong.
    Doctor,
    /// Record for a few seconds, recognise, clean up and paste.
    Dictate {
        #[arg(short, long, default_value_t = 5.0)]
        seconds: f32,
    },
    /// Print what the desktop reports as the focused app.
    Context,
    /// List microphones.
    Devices,
    /// Show the graphics cards and whether recognition can run on one.
    Gpu,
    /// Create the config file if missing and print it.
    Config {
        /// Open it in $EDITOR.
        #[arg(short, long)]
        edit: bool,
    },
    /// Turn learning on or off, or show its status.
    Learning {
        #[arg(value_parser = ["on", "off", "status"], default_value = "status")]
        action: String,
    },
    /// Show the learned vocabulary and style note.
    Vocab {
        /// Drop a term and never learn it again.
        #[arg(long)]
        forget: Option<String>,
    },
    /// Re-mine the profile now instead of waiting.
    Learn,
    /// Stored dictations.
    History {
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        /// Delete all of it.
        #[arg(long)]
        clear: bool,
    },
    /// Manage recognition model files.
    Models {
        #[command(subcommand)]
        action: ModelsAction,
    },
    /// Started by Flow itself: unload the Ollama model if Flow dies.
    #[command(hide = true)]
    WatchOllama {
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        model: String,
    },
    /// Run the cleanup evaluation cases against the configured model.
    Eval {
        /// Only cases whose name contains this.
        #[arg(long)]
        filter: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ModelsAction {
    /// Download the configured model's files.
    Download {
        #[arg(long, value_parser = ["int8", "fp32"])]
        precision: Option<String>,
    },
    /// Copy an export from the Hugging Face cache (developer convenience).
    ImportHf,
    /// Show what is on disk.
    Status,
}

pub fn run(command: Command) -> i32 {
    // Needs nothing from the config, so a broken one cannot stop it.
    if let Command::WatchOllama { endpoint, model } = &command {
        return match crate::watchdog::run(endpoint, model) {
            Ok(()) => 0,
            Err(err) => {
                eprintln!("error: {err:#}");
                1
            }
        };
    }
    match dispatch(command) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    }
}

fn dispatch(command: Command) -> anyhow::Result<()> {
    let config = Config::load()?;
    match command {
        Command::Doctor => doctor(&config),
        Command::Dictate { seconds } => dictate(seconds),
        Command::Context => {
            let backends = flow_desktop::build(&config.desktop)?;
            let ctx = backends.focus.context()?;
            println!("   app: {}\n title: {}\n  role: {}", ctx.app, ctx.title, ctx.role);
            Ok(())
        }
        Command::Devices => {
            for d in flow_audio::list_devices() {
                println!(
                    "{}{} ({} ch, {} Hz)",
                    if d.is_default { "* " } else { "  " },
                    d.name,
                    d.channels,
                    d.default_rate
                );
            }
            Ok(())
        }
        Command::Gpu => {
            gpu(&config);
            Ok(())
        }
        Command::Config { edit } => {
            let path = config::write_default_config()?;
            println!("{}", path.display());
            if edit {
                let editor = std::env::var("VISUAL")
                    .or_else(|_| std::env::var("EDITOR"))
                    .unwrap_or_else(|_| "vi".into());
                std::process::Command::new(editor).arg(&path).status()?;
            } else {
                print!("{}", std::fs::read_to_string(&path)?);
            }
            Ok(())
        }
        Command::Learning { action } => match action.as_str() {
            "on" | "off" => {
                config::set_value("learning", "enabled", action == "on")?;
                println!("learning {action}; restart Flow to apply");
                Ok(())
            }
            _ => {
                let history = History::open_default()?;
                let (terms, style) = learning::load_profile(&history);
                println!(
                    "learning is {}; {} dictations stored, {} terms learned",
                    if config.learning.enabled { "on" } else { "off" },
                    history.count()?,
                    terms.len()
                );
                if !style.is_empty() {
                    println!("style: {style}");
                }
                Ok(())
            }
        },
        Command::Vocab { forget } => {
            let history = History::open_default()?;
            if let Some(term) = forget {
                let removed = learning::forget_term(&history, &term)?;
                println!(
                    "{}",
                    if removed { "forgotten and blocked" } else { "blocked (was not in the vocabulary)" }
                );
                return Ok(());
            }
            let (terms, style) = learning::load_profile(&history);
            if !style.is_empty() {
                println!("style: {style}\n");
            }
            if terms.is_empty() {
                println!("no vocabulary learned yet");
            } else {
                println!("vocabulary ({}):", terms.len());
                for t in &terms {
                    println!("  {t}");
                }
            }
            let blocked = learning::blocked_terms(&history)?;
            if !blocked.is_empty() {
                println!("\nblocked: {}", blocked.join(", "));
            }
            Ok(())
        }
        Command::Learn => {
            if !config.learning.enabled {
                anyhow::bail!("learning is off; `flow learning on` first");
            }
            let history = History::open_default()?;
            if history.count()? == 0 {
                anyhow::bail!("nothing stored yet");
            }
            let backend: Arc<dyn flow_core::backends::Backend> =
                Arc::from(flow_core::backends::build_backend(&config.cleanup));
            let (terms, style) = Learner::new(backend).refresh(&history, config.learning.max_terms)?;
            println!("learned {} terms; style: {}", terms.len(), if style.is_empty() { "-" } else { &style });
            Ok(())
        }
        Command::History { limit, clear } => {
            let history = History::open_default()?;
            if clear {
                println!("deleted {} dictation(s)", history.clear()?);
                return Ok(());
            }
            let rows = history.recent(limit)?;
            for row in rows.iter().rev() {
                println!(
                    "{}  {:<18}  {}",
                    &row.at.get(11..19).unwrap_or(&row.at),
                    short(&row.app, 18),
                    short(&row.clean, 60)
                );
            }
            if rows.is_empty() {
                println!("nothing stored");
            }
            Ok(())
        }
        Command::Models { action } => match action {
            ModelsAction::Download { precision } => {
                let precision = match precision.as_deref() {
                    Some("fp32") => Precision::Fp32,
                    Some("int8") => Precision::Int8,
                    _ => flow_stt::gpu::precision_for(&config.stt.provider),
                };
                let mut last = String::new();
                let cancel = std::sync::atomic::AtomicBool::new(false);
                flow_stt::download(&config.stt.model, precision, &cancel, |p: flow_stt::Progress| {
                    if p.file != last {
                        last = p.file.clone();
                        eprintln!("{}", p.file);
                    }
                    if p.total > 0 {
                        eprint!("\r  {:>3}%", p.received * 100 / p.total);
                    }
                })?;
                eprintln!("\ndone: {}", models::model_dir(&config.stt.model).display());
                Ok(())
            }
            ModelsAction::ImportHf => {
                flow_stt::import_from_hf_cache(&config.stt.model)?;
                println!("imported into {}", models::model_dir(&config.stt.model).display());
                Ok(())
            }
            ModelsAction::Status => {
                for m in models::STT_MODELS {
                    println!(
                        "{:<28} int8: {}  fp32: {}",
                        m.id,
                        if models::is_downloaded(m.id, Precision::Int8) { "yes" } else { "no " },
                        if models::is_downloaded(m.id, Precision::Fp32) { "yes" } else { "no " },
                    );
                }
                Ok(())
            }
        },
        Command::Eval { filter } => crate::eval::run(&config, filter.as_deref()),
        Command::WatchOllama { .. } => unreachable!("handled in run"),
    }
}

fn short(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

fn gpu(config: &Config) {
    let report = flow_stt::gpu::detect();
    if report.devices.is_empty() {
        println!("no graphics card found");
    }
    for d in &report.devices {
        let mut facts = Vec::new();
        if let Some(kind) = &d.kind {
            facts.push(kind.clone());
        }
        if let Some(mb) = d.memory_mb {
            facts.push(format!("{:.1} GB", mb as f64 / 1024.0));
        }
        if let Some(c) = &d.compute {
            facts.push(format!("compute {c}"));
        }
        if let Some(driver) = &d.driver {
            facts.push(format!("driver {driver}"));
        }
        println!("{:<32} {}", d.name, facts.join(", "));
    }
    if let Some(driver) = &report.driver {
        println!("\nNVIDIA driver {driver}");
    }
    for path in &report.loaded_from {
        println!("using {path}");
    }
    println!();
    println!(
        "this build: {}",
        match report.backend.as_deref() {
            Some("webgpu") => "WebGPU (any graphics card)",
            Some("cuda") => "CUDA (NVIDIA cards)",
            _ => "CPU only",
        }
    );
    if report.usable {
        println!("GPU recognition: yes, on the {}", report.describe());
    } else {
        println!("GPU recognition: no - {}", report.describe());
        if let Some(fix) = &report.fix {
            println!("to fix: {fix}");
        }
    }
    let precision = flow_stt::gpu::precision_for(&config.stt.provider);
    println!(
        "provider = {} loads the {} model ({})",
        config.stt.provider,
        if precision == Precision::Fp32 { "fp32" } else { "int8" },
        if models::is_downloaded(&config.stt.model, precision) { "downloaded" } else { "not downloaded yet" }
    );
}

fn doctor(config: &Config) -> anyhow::Result<()> {
    let checks = crate::doctor::run(config, &[], false);
    let mut failed = 0;
    for c in &checks {
        println!("{} {:<22} {}", if c.ok { "ok  " } else { "FAIL" }, c.name, c.detail);
        if !c.ok {
            failed += 1;
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} check(s) failing");
    }
    Ok(())
}

fn dictate(seconds: f32) -> anyhow::Result<()> {
    let shared = Shared::load();
    let config = shared.config();
    let backends = flow_desktop::build(&config.desktop)?;
    let overlay: Arc<dyn flow_core::engine::Overlay> = match backends.overlay {
        flow_desktop::OverlayChoice::Native(island) => island,
        _ => Arc::new(crate::host::NullOverlay),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let mut engine =
        crate::host::assemble(&shared, &config, overlay, backends.focus, backends.injector, tx, rx)?;
    engine.prepare()?;
    eprintln!("Speak now ({seconds:.0}s)...");
    println!("{}", engine.dictate_once(seconds)?);
    Ok(())
}
