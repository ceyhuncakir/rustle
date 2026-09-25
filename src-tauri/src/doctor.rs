//! `rustle doctor`: check every moving part and say what is wrong.

use rustle_core::config::Config;
use rustle_core::models;
use rustle_desktop::HotkeySource;
use rustle_stt::GpuReport;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

fn check(name: &str, result: Result<String, String>) -> Check {
    match result {
        Ok(detail) => Check { name: name.into(), ok: true, detail },
        Err(detail) => Check { name: name.into(), ok: false, detail: truncate(detail, 240) },
    }
}

/// At most `n` characters; slicing by bytes would split a multi-byte one.
fn truncate(s: String, n: usize) -> String {
    if s.chars().count() <= n {
        return s;
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// The graphics card line. Running on the CPU is not a fault; a card that
/// could be used but is not, is.
fn gpu_check(provider: &str, gpu: &GpuReport) -> Result<String, String> {
    let with_fix = || match &gpu.fix {
        Some(fix) => format!("{} - {fix}", gpu.describe()),
        None => gpu.describe(),
    };
    match provider {
        "cpu" => Ok(format!("provider = cpu; {}", gpu.describe())),
        _ if gpu.usable => Ok(match &gpu.driver {
            Some(driver) => format!("{}, driver {driver}", gpu.describe()),
            None => gpu.describe(),
        }),
        // Told to use the GPU: fine while there is a card and nothing to fix,
        // such as an integrated GPU that `auto` would pass over.
        p if rustle_stt::gpu::insists_on_gpu(p) => match (&gpu.gpu, &gpu.fix) {
            (Some(card), None) if gpu.backend.is_some() => Ok(format!("{card}, used because provider = {p}")),
            _ => Err(with_fix()),
        },
        _ if gpu.gpu.is_some() && gpu.backend.is_some() && gpu.fix.is_some() => Err(with_fix()),
        _ => Ok(format!("{} - recognition runs on the CPU", gpu.describe())),
    }
}

/// Where the dictation shortcut comes from: the desktop's portal, the GNOME
/// extension, a grab of Rustle's own, or only the control socket that the
/// desktop's key bindings reach through `rustle hotkey`.
fn shortcut_check(session: rustle_desktop::Session, source: &HotkeySource) -> Result<String, String> {
    let main = match source {
        HotkeySource::AppShortcut(combo) => Ok(format!("{combo}, grabbed by Rustle")),
        HotkeySource::Builtin(_) => builtin_shortcut(session),
        HotkeySource::External(how) => Ok(format!("socket only - {how}")),
        HotkeySource::Unsupported(why) => Err(format!("none - {why}")),
    };
    // `rustle hotkey` works on every Linux desktop while Rustle runs.
    #[cfg(target_os = "linux")]
    let main = {
        let socket = if rustle_desktop::linux::control::listening() {
            "control socket listening"
        } else {
            "control socket not listening (Rustle is not running)"
        };
        let with = |m: String| format!("{}; {socket}", m.trim_end_matches('.'));
        main.map(with).map_err(with)
    };
    main
}

#[cfg(target_os = "linux")]
fn builtin_shortcut(session: rustle_desktop::Session) -> Result<String, String> {
    use rustle_desktop::linux::portal::{self, PortalState};
    if matches!(session, rustle_desktop::Session::GnomeWayland { extension: true }) {
        return Ok("GNOME Shell extension".into());
    }
    let version = portal::version().map(|v| format!(" v{v}")).unwrap_or_default();
    let state = portal::state();
    match &state {
        _ if state.is_bound() => Ok(format!(
            "portal{version}: {}",
            portal::display_trigger(state.dictate_trigger().unwrap_or_default())
        )),
        PortalState::Binding => Ok(format!("portal{version}: waiting for the desktop's dialog")),
        PortalState::Declined => Err(format!(
            "portal{version}: the desktop's dialog was closed without adding the shortcut - \
             turn dictation off and on to be asked again"
        )),
        PortalState::Failed(why) => Err(format!("portal{version}: {why}")),
        // Another process (`rustle doctor` next to a running Rustle) cannot see
        // the binding; the desktop's settings list it.
        _ => Ok(format!("portal{version}; Rustle binds it when dictation starts")),
    }
}

/// The Shell keeps running the extension it loaded at login, which after
/// an upgrade can be older than the copy this Rustle carries.
#[cfg(target_os = "linux")]
fn extension_check() -> Result<String, String> {
    use rustle_desktop::linux::gnome;
    let running = gnome::running_extension_version();
    if !gnome::extension_outdated(running.as_deref()) {
        return Ok(format!("version {}", running.as_deref().unwrap_or("?")));
    }
    Err(format!(
        "{} is running, but this Rustle carries version {}: install it under Settings > Desktop, then log out and back in",
        running.map_or("an unversioned copy".to_string(), |v| format!("version {v}")),
        gnome::bundled_extension_version().unwrap_or_default()
    ))
}

#[cfg(not(target_os = "linux"))]
fn builtin_shortcut(_session: rustle_desktop::Session) -> Result<String, String> {
    Ok("delivered by the desktop".into())
}

pub fn run(config: &Config, notes: &[String], running: bool) -> Vec<Check> {
    let mut checks = Vec::new();

    // A value that does not fit is skipped rather than failing the whole
    // file, so say which.
    checks.push(check(
        "Config",
        match Config::load_with_warnings() {
            Ok((_, warnings)) if warnings.is_empty() => {
                Ok(rustle_core::config::config_path().display().to_string())
            }
            Ok((_, warnings)) => Err(warnings.join("; ")),
            Err(err) => Err(format!("{err:#}")),
        },
    ));

    // Desktop integration.
    let session = rustle_desktop::session::detect();
    match rustle_desktop::build_for(session, &config.desktop) {
        Ok(backends) => {
            checks.push(check("Desktop", Ok(session.to_string())));
            checks.push(check("Shortcut", shortcut_check(session, &backends.hotkey)));
        }
        Err(err) => checks.push(check("Desktop", Err(format!("{session}: {err:#}")))),
    }
    // Outside GNOME's extension, Wayland pastes through a helper program,
    // and one merely on PATH may still be unable to type.
    #[cfg(target_os = "linux")]
    {
        use rustle_desktop::Session;
        if matches!(session, Session::GnomeWayland { extension: true }) {
            checks.push(check("GNOME extension", extension_check()));
        }
        if matches!(
            session,
            Session::KdeWayland
                | Session::LayerShellWayland
                | Session::OtherWayland
                | Session::GnomeWayland { extension: false }
        ) {
            let tool = rustle_desktop::linux::wayland::probe_tool();
            checks.push(check("Paste", tool.map(|name| format!("{name} sends the paste keystroke"))));
        }
    }
    for note in notes {
        checks.push(Check { name: "Desktop".into(), ok: true, detail: note.clone() });
    }

    // Microphone.
    checks.push(check("Microphone", {
        let devices = rustle_audio::list_devices();
        if devices.is_empty() {
            Err("no input devices".into())
        } else {
            let default = devices.iter().find(|d| d.is_default).or(devices.first());
            Ok(format!(
                "{} input device(s); default: {}",
                devices.len(),
                default.map(|d| d.name.as_str()).unwrap_or("?")
            ))
        }
    }));

    // Recognition model files.
    let precision = rustle_stt::gpu::precision_for(&config.stt.provider);
    checks.push(check("Recognition model", {
        let id = config.stt.model.as_str();
        if models::stt_model(id).is_none() {
            Err(format!("unknown model {id:?}"))
        } else if models::is_downloaded(id, precision) {
            Ok(format!("{id} ({precision:?}) in {}", models::model_dir(id).display()))
        } else {
            Err(format!("{id} not downloaded - open Settings or run `rustle models download`"))
        }
    }));
    checks.push(check("GPU", gpu_check(&config.stt.provider, rustle_stt::gpu::detect())));

    // Cleanup.
    let cleaner = rustle_core::cleanup::build_cleaner(&config.cleanup);
    let (ok, why) = cleaner.available();
    checks.push(check(
        &format!("Cleanup ({})", if config.cleanup.enabled { config.cleanup.model.as_str() } else { "off" }),
        if ok { Ok(why) } else { Err(why) },
    ));

    // Engine.
    checks.push(Check {
        name: "Service".into(),
        ok: true,
        detail: if running { "running".into() } else { "stopped".into() },
    });

    // Learning.
    checks.push(check("Learning", {
        if config.learning.enabled {
            match rustle_core::history::History::open_default() {
                Ok(history) => {
                    let (terms, _) = rustle_core::learning::load_profile(&history);
                    Ok(format!("on - {} stored, {} terms learned", history.count().unwrap_or(0), terms.len()))
                }
                Err(err) => Err(format!("history unavailable: {err:#}")),
            }
        } else {
            Ok("off - nothing is being stored".into())
        }
    }));

    checks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(usable: bool, problem: Option<&str>, fix: Option<&str>) -> GpuReport {
        GpuReport {
            backend: Some("cuda".into()),
            devices: vec![],
            gpu: Some("NVIDIA GeForce RTX 4090 (24 GB)".into()),
            driver: Some("580.159.03, CUDA 13.0".into()),
            usable,
            problem: problem.map(Into::into),
            fix: fix.map(Into::into),
            loaded_from: vec![],
        }
    }

    #[test]
    fn truncate_counts_characters() {
        assert_eq!(truncate("Çakır".repeat(30), 5), "Çakır…");
        assert_eq!(truncate("short".into(), 10), "short");
    }

    #[test]
    fn a_usable_gpu_is_ok() {
        assert!(gpu_check("auto", &report(true, None, None)).unwrap().contains("driver 580"));
    }

    #[test]
    fn a_fixable_gpu_fails_only_when_it_could_be_used() {
        let missing = report(false, Some("cuDNN 9 is not installed"), Some("install cuDNN"));
        assert!(gpu_check("cpu", &missing).is_ok());
        assert!(gpu_check("auto", &missing).is_err());
        assert!(gpu_check("gpu", &missing).unwrap_err().contains("install cuDNN"));
        let old = report(false, Some("its compute capability 6.1 is older"), None);
        assert!(gpu_check("auto", &old).is_ok(), "nothing to fix, so not a failure");
    }

    #[test]
    fn gpu_only_uses_an_integrated_card() {
        let igpu = report(false, Some("it is built into the processor"), None);
        assert!(gpu_check("gpu", &igpu).unwrap().contains("used because provider = gpu"));
        let mut none = report(false, Some("no graphics card found"), None);
        none.gpu = None;
        assert!(gpu_check("gpu", &none).is_err());
    }
}
