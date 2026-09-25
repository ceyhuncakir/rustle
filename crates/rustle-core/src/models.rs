//! The recognisers on offer, and the files each one needs.
//!
//! Kept here rather than in `rustle-stt` so the settings window can list them
//! without linking the model runtime.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SttModel {
    /// The id used in `config.toml` (unchanged from the Python version).
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Hugging Face repository holding the onnx-asr export.
    pub repo: &'static str,
}

pub const DEFAULT_STT_MODEL: &str = "nemo-parakeet-tdt-0.6b-v3";

pub const STT_MODELS: &[SttModel] = &[
    SttModel {
        id: "nemo-parakeet-tdt-0.6b-v3",
        label: "Parakeet TDT v3",
        description: "25 European languages, detected automatically",
        repo: "istupakov/parakeet-tdt-0.6b-v3-onnx",
    },
    SttModel {
        id: "nemo-parakeet-tdt-0.6b-v2",
        label: "Parakeet TDT v2",
        description: "English only, marginally better on English",
        repo: "istupakov/parakeet-tdt-0.6b-v2-onnx",
    },
];

pub fn stt_model(id: &str) -> Option<&'static SttModel> {
    STT_MODELS.iter().find(|m| m.id == id)
}

/// The log-mel preprocessor. Shared by every precision, and the one file
/// `rustle-stt` fetches from somewhere other than the model repository.
pub const NEMO128_FILE: &str = "nemo128.onnx";
pub const VOCAB_FILE: &str = "vocab.txt";

/// Which encoder precision to fetch. The int8 export is a quarter of the
/// size and what the CPU runs; the fp32 one is what CUDA runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Int8,
    Fp32,
}

/// The files a Parakeet export consists of, in download order.
pub fn model_files(precision: Precision) -> Vec<&'static str> {
    let mut files = vec![NEMO128_FILE, VOCAB_FILE];
    match precision {
        Precision::Int8 => files.extend(["encoder-model.int8.onnx", "decoder_joint-model.int8.onnx"]),
        Precision::Fp32 => {
            files.extend(["encoder-model.onnx", "encoder-model.onnx.data", "decoder_joint-model.onnx"])
        }
    }
    files
}

/// Roughly what [`model_files`] weigh, in MB: 671 and 2550 for Parakeet
/// TDT v3; v2 differs by a few MB.
pub fn download_mb(precision: Precision) -> u64 {
    match precision {
        Precision::Int8 => 671,
        Precision::Fp32 => 2550,
    }
}

/// Where a model's files live on disk.
pub fn model_dir(id: &str) -> std::path::PathBuf {
    crate::config::data_dir().join("models").join(id)
}

/// Whether every file of the given precision is present.
pub fn is_downloaded(id: &str, precision: Precision) -> bool {
    let dir = model_dir(id);
    model_files(precision).iter().all(|f| dir.join(f).is_file())
}
