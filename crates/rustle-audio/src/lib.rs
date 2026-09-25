//! Microphone capture for Rustle.
//!
//! A port of `flowd/audio.py`. Push-to-talk defines the take boundaries, so
//! there is no VAD here: [`CpalRecorder`] opens the input device when the
//! hotkey goes down, converts whatever the device produces into 16 kHz mono
//! `f32`, reports a level per block for the island's waveform, and hands the
//! whole take back when the hotkey is released.
//!
//! The Python daemon leaned on PortAudio and PipeWire to resample for it and
//! fell back to capturing at the device's native rate when a named device
//! refused 16 kHz. This crate always captures native and resamples itself
//! (see `pipeline`), which makes the default and the named-device paths
//! identical.

mod devices;
mod pipeline;
mod recorder;

pub use devices::{list_devices, DeviceInfo};
pub use recorder::CpalRecorder;
