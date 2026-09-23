//! Speech recognition: NVIDIA Parakeet TDT on ONNX Runtime.
//!
//! A port of `flowd/stt.py` without the Python. The weights are the onnx-asr
//! export of Parakeet TDT 0.6B (v3 multilingual, v2 English) and the pipeline
//! is the one onnx-asr runs: `nemo128.onnx` (log-mel features) -> encoder ->
//! a greedy TDT loop over `decoder_joint`, here on the `ort` crate.
//!
//! Model files live in [`flow_core::models::model_dir`]; [`download`] fetches
//! them and [`import_from_hf_cache`] copies a developer's Hugging Face cache
//! there. The Hugging Face cache itself is never read at run time: its
//! snapshots are symlinks into a blob store, which onnxruntime's external-data
//! path check rejects.

// ort ships the CUDA and WebGPU providers as two separate ONNX Runtime
// builds; asked for both, it silently links the CPU-only one.
#[cfg(all(feature = "cuda", feature = "webgpu"))]
compile_error!("the `cuda` and `webgpu` features cannot be combined; pick one GPU backend");

mod decode;
mod download;
mod files;
pub mod gpu;
mod parakeet;
mod vocab;

pub use download::{download, Cancelled, Progress};
pub use files::import_from_hf_cache;
pub use gpu::{GpuDevice, GpuReport};
pub use parakeet::{free_gpu_context, release_runtime, ComputeReport, Features, Parakeet, Transcription};

/// ONNX Runtime's build string (version, commit, compiler flags), the same
/// text `onnxruntime.get_build_info()` returns in Python. Two builds with
/// different strings can differ in the last bits of their float math.
pub fn runtime_info() -> &'static str {
    ort::info()
}
