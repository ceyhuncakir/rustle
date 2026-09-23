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
        .filter(|device| !is_null_sink(device))
        .filter_map(|device| {
            let config = device.default_input_config().ok()?;
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

/// The default input device, or the first input device whose name (or
/// backend id, e.g. ALSA's `hw:CARD=Generic`) contains `wanted`,
/// case-insensitively.
pub(crate) fn find_input_device(host: &cpal::Host, wanted: Option<&str>) -> anyhow::Result<cpal::Device> {
    let Some(wanted) = wanted.map(str::trim).filter(|s| !s.is_empty()) else {
        return host.default_input_device().ok_or_else(|| anyhow!("no default input device"));
    };
    let needle = wanted.to_lowercase();
    host.input_devices()
        .context("could not list input devices")?
        .find(|device| {
            device.description().is_ok_and(|d| {
                d.name().to_lowercase().contains(&needle)
                    || d.driver().is_some_and(|driver| driver.to_lowercase().contains(&needle))
            })
        })
        .ok_or_else(|| anyhow!("no input device matching {wanted:?}"))
}

#[cfg(test)]
mod tests {
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
