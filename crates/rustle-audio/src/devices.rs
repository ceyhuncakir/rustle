//! Input device enumeration and lookup on cpal's default host.

use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait};

/// One capture device, as the settings UI lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub channels: u16,
    pub default_rate: u32,
    pub is_default: bool,
}

/// Every device that can be opened for input right now, one entry per name.
///
/// Probing the default config opens each device, which is the only way ALSA
/// will say whether a PCM works for capture; the ones that refuse (output-only
/// plugins, cards that are gone) are left out rather than listed and then
/// failing at record time. ALSA also exposes each card under several aliases
/// (`sysdefault`, `front`, `hw`, `plughw`) with one description; only the
/// first is kept, because that is what a name lookup would resolve to anyway.
pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_input_device();
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    devices
        .filter_map(|device| {
            let config = usable_config(&device)?;
            let name = device_name(&device);
            seen.insert(name.clone()).then_some(DeviceInfo {
                name,
                channels: config.channels(),
                default_rate: config.sample_rate(),
                is_default: default.as_ref() == Some(&device),
            })
        })
        .collect()
}

/// The device's default input configuration, if it is a device
/// [`list_devices`] shows: not ALSA's null sink, and able to capture now.
fn usable_config(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    if is_null_sink(device) {
        return None;
    }
    device.default_input_config().ok()
}

/// ALSA's `null` PCM opens fine for capture and delivers silence forever;
/// nobody wants to dictate into it.
fn is_null_sink(device: &cpal::Device) -> bool {
    device.description().is_ok_and(|d| d.driver() == Some("null"))
}

/// The human name: on ALSA the first line of the hint description
/// ("PipeWire Sound Server"), elsewhere whatever the host reports.
pub(crate) fn device_name(device: &cpal::Device) -> String {
    device.description().map(|d| d.name().to_owned()).unwrap_or_else(|_| "unknown".to_owned())
}

/// The default input device, or the one `wanted` names; see [`choose`].
pub(crate) fn find_input_device(host: &cpal::Host, wanted: Option<&str>) -> anyhow::Result<cpal::Device> {
    let Some(wanted) = wanted.map(str::trim).filter(|s| !s.is_empty()) else {
        return host.default_input_device().ok_or_else(|| anyhow!("no default input device"));
    };
    let candidates = host
        .input_devices()
        .context("could not list input devices")?
        .map(|device| {
            let description = device.description().ok();
            Candidate {
                name: device_name(&device),
                driver: description.and_then(|d| d.driver().map(str::to_owned)),
                device,
            }
        })
        .collect();
    choose(candidates, wanted, |device| usable_config(device).is_some())
        .ok_or_else(|| anyhow!("no input device matching {wanted:?}"))
}

/// A device as a lookup sees it: the name [`list_devices`] shows, and the
/// backend's own id for it (ALSA's `hw:CARD=Generic`), when there is one.
struct Candidate<D> {
    device: D,
    name: String,
    driver: Option<String>,
}

/// The device `wanted` names, by the rules [`list_devices`] follows, so a
/// name picked from the list opens the device listed under it: the first
/// usable device with exactly that name. Failing that (a name typed by hand),
/// the first usable one whose name or backend id contains `wanted`, ignoring
/// case. `usable` opens the device, so it is only asked about devices whose
/// name matches.
fn choose<D>(candidates: Vec<Candidate<D>>, wanted: &str, usable: impl Fn(&D) -> bool) -> Option<D> {
    let needle = wanted.to_lowercase();
    let loose = |c: &Candidate<D>| {
        c.name.to_lowercase().contains(&needle)
            || c.driver.as_ref().is_some_and(|driver| driver.to_lowercase().contains(&needle))
    };
    let index = candidates
        .iter()
        .position(|c| c.name == wanted && usable(&c.device))
        .or_else(|| candidates.iter().position(|c| loose(c) && usable(&c.device)))?;
    candidates.into_iter().nth(index).map(|c| c.device)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Devices as `(name, backend id, usable)`; the chosen one's index.
    fn pick(devices: &[(&str, &str, bool)], wanted: &str) -> Option<usize> {
        let candidates = devices
            .iter()
            .enumerate()
            .map(|(i, (name, driver, _))| Candidate {
                device: i,
                name: name.to_string(),
                driver: Some(driver.to_string()),
            })
            .collect();
        choose(candidates, wanted, |&i| devices[i].2)
    }

    #[test]
    fn a_listed_name_opens_the_listed_device() {
        let devices = [
            ("USB Audio Front", "front:CARD=USB", true),
            ("USB Audio", "sysdefault:CARD=USB", false),
            ("USB Audio", "hw:CARD=USB", true),
            ("USB Audio", "plughw:CARD=USB", true),
        ];
        // Not the earlier substring match, and not the alias that cannot
        // capture: the first usable one, which is what the list shows.
        assert_eq!(pick(&devices, "USB Audio"), Some(2));
    }

    #[test]
    fn a_typed_name_matches_loosely() {
        let devices = [
            ("Null Output", "null", false),
            ("HD Audio Mic", "sysdefault:CARD=PCH", true),
            ("USB Audio", "hw:CARD=Generic", true),
        ];
        assert_eq!(pick(&devices, "usb"), Some(2));
        assert_eq!(pick(&devices, "hw:card=generic"), Some(2));
        assert_eq!(pick(&devices, "null"), None, "matches, but cannot capture");
        assert_eq!(pick(&devices, "nothing like it"), None);
    }

    #[test]
    fn list_devices_does_not_panic() {
        // May be empty on a headless CI box; must not blow up.
        let devices = super::list_devices();
        for device in &devices {
            eprintln!("{device:?}");
        }
        assert!(devices.iter().all(|d| !d.name.is_empty()));
    }
}
