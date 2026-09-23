//! Parity with the Python daemon's onnx-asr pipeline.
//!
//! `scripts/stt-golden.py` synthesises the fixtures under `tests/fixtures/stt`
//! and records what onnx-asr 0.12 produces for each: token ids, encoder frame
//! indices and text, on the CPU (int8, exactly the graphs this crate runs)
//! and, when a GPU was around, on CUDA (fp32). The CPU result must match
//! token for token; CUDA is compared by character error rate since the
//! encoder's floating point differs across providers and ONNX Runtime
//! versions.
//!
//! Exactness needs the same onnxruntime *build* on both sides: builds differ
//! in the last bits of their float math (pyke's binaries versus Microsoft's
//! wheel give features 3e-6 apart on the identical graph), and int8 dynamic
//! quantisation turns that into the odd token or a duration argmax one frame
//! off. The golden records `onnxruntime.get_build_info()`; when `ort::info()`
//! is the same string the CPU test demands identity, otherwise a tolerance.
//! The script's docstring says how to run the strict variant.
//!
//! The tests skip, printing why, when the model files or fixtures are not
//! on disk, so a checkout without a downloaded model still passes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use flow_core::engine::Transcriber;
use flow_core::models::{is_downloaded, model_dir, Precision, DEFAULT_STT_MODEL};
use flow_stt::{Parakeet, Transcription};
use serde::Deserialize;

/// The GPU: fp32, compared with the golden's CUDA results by text since the
/// encoder's float math differs by provider. WebGPU is held to the same.
#[cfg(any(feature = "cuda", feature = "webgpu"))]
const GPU_CER_LIMIT: f64 = 0.005;
#[cfg(any(feature = "cuda", feature = "webgpu"))]
const GPU_TOKEN_DIFF_LIMIT: usize = 3;
/// CPU on a different onnxruntime build than the golden's: int8 noise. The
/// measured deviation of pyke's 1.24.2 from the 1.24.4 wheel is 1.2 % CER
/// over the set and at most 4 token edits in a file.
const CPU_CER_LIMIT: f64 = 0.02;
const CPU_TOKEN_DIFF_LIMIT: usize = 6;
const FEATURE_TOLERANCE: f32 = 1e-5;

#[derive(Deserialize)]
struct Golden {
    model: String,
    onnxruntime: String,
    build_info: String,
    features_file: String,
    files: HashMap<String, GoldenFile>,
}

#[derive(Deserialize)]
#[cfg_attr(not(any(feature = "cuda", feature = "webgpu")), allow(dead_code))]
struct GoldenFile {
    sample_rate: u32,
    seconds: f64,
    cpu: Result_,
    cuda: Option<Result_>,
    /// What `flowd/stt.py` itself produced (auto provider, fp32); informative.
    #[serde(default)]
    daemon_text: Option<String>,
}

#[derive(Deserialize, Clone)]
struct Result_ {
    text: String,
    tokens: Vec<u32>,
    frames: Vec<usize>,
}

#[derive(Deserialize)]
struct GoldenFeatures {
    file: String,
    shape: Vec<usize>,
    valid: usize,
    data: Vec<f32>,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/stt")
}

/// The golden set, or a printed reason to skip.
fn setup(precision: Precision) -> Option<Golden> {
    let _ = env_logger::builder().is_test(true).try_init();
    let path = fixtures_dir().join("golden.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("skipping: no fixtures at {} (run scripts/stt-golden.py)", path.display());
        return None;
    };
    let golden: Golden = serde_json::from_str(&text).expect("golden.json parses");
    let missing: Vec<&String> = golden.files.keys().filter(|f| !fixtures_dir().join(f).is_file()).collect();
    if !missing.is_empty() {
        // The wav files are git-ignored; the script regenerates them.
        println!(
            "skipping: {} fixture wav(s) missing under {} (run scripts/stt-golden.py --synth-only)",
            missing.len(),
            fixtures_dir().display()
        );
        return None;
    }
    println!("golden from onnxruntime {} ({})", golden.onnxruntime, golden.build_info);
    println!("running on {}", flow_stt::runtime_info());
    if !is_downloaded(&golden.model, precision) {
        println!(
            "skipping: model {} ({precision:?}) is not downloaded to {}",
            golden.model,
            model_dir(&golden.model).display()
        );
        return None;
    }
    Some(golden)
}

fn read_wav(path: &Path) -> (Vec<f32>, u32) {
    let mut reader = hound::WavReader::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "{} is not mono", path.display());
    assert_eq!(spec.bits_per_sample, 16, "{} is not 16-bit", path.display());
    let samples = reader.samples::<i16>().map(|s| s.unwrap() as f32 / 32768.0).collect();
    (samples, spec.sample_rate)
}

fn sorted_files(golden: &Golden) -> Vec<(&String, &GoldenFile)> {
    let mut files: Vec<_> = golden.files.iter().collect();
    files.sort_by(|a, b| a.0.cmp(b.0));
    files
}

/// Levenshtein distance.
fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, x) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let cost = usize::from(x != y);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

/// Character and token edit distances from the golden result.
fn edits(got: &Transcription, want: &Result_) -> (usize, usize) {
    (edit_distance(&chars(&got.text), &chars(&want.text)), edit_distance(&got.tokens, &want.tokens))
}

fn cer(edits: usize, total_chars: usize) -> f64 {
    edits as f64 / total_chars.max(1) as f64
}

#[test]
fn cpu_matches_python_golden() {
    let Some(golden) = setup(Precision::Int8) else { return };
    let strict = flow_stt::runtime_info() == golden.build_info;
    println!(
        "{}",
        if strict {
            "same onnxruntime build as the golden: every token and frame must match"
        } else {
            "different onnxruntime build than the golden: int8 results are held to a tolerance \
             (run with ORT_DYLIB_PATH pointing at the golden's libonnxruntime and --features ort/load-dynamic for the strict check)"
        }
    );
    let model = Parakeet::new(&golden.model, "cpu");
    model.load().expect("load on cpu");
    let report = model.compute_report().unwrap();
    println!("compute: requested={} actual={} ({})", report.requested, report.actual, report.reason);
    assert_eq!(report.actual, "cpu");
    assert_eq!(model.transcribe(&[], 16_000).unwrap(), "");

    let (mut total_edits, mut total_chars, mut exact) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    let files = sorted_files(&golden);
    for (name, expected) in &files {
        let (audio, rate) = read_wav(&fixtures_dir().join(name));
        assert_eq!(rate, expected.sample_rate, "{name}");
        let started = Instant::now();
        let got = model.transcribe_detailed(&audio, rate).unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        let want = &expected.cpu;
        let identical = got.tokens == want.tokens && got.frames == want.frames && got.text == want.text;
        let (char_edits, token_edits) = edits(&got, want);
        total_edits += char_edits;
        total_chars += want.text.chars().count();
        exact += usize::from(identical);
        println!(
            "{name}: {:.1}s audio in {elapsed:.2}s ({:.0}x realtime), {} -> {:?}",
            expected.seconds,
            expected.seconds / elapsed,
            if identical {
                "identical".to_string()
            } else {
                format!("{char_edits} char / {token_edits} token edits")
            },
            got.text
        );
        if let Some(daemon) = &expected.daemon_text {
            let daemon_cer = cer(edit_distance(&chars(&got.text), &chars(daemon)), daemon.chars().count());
            println!("    vs the Python daemon's own text: CER {:.2}%", daemon_cer * 100.0);
        }
        if (strict && !identical) || token_edits > CPU_TOKEN_DIFF_LIMIT {
            failures.push(format!(
                "{name}:\n  got  {:?}\n       tokens {:?}\n       frames {:?}\n  want {:?}\n       tokens {:?}\n       frames {:?}",
                got.text, got.tokens, got.frames, want.text, want.tokens, want.frames
            ));
        }
    }
    let cer = cer(total_edits, total_chars);
    println!(
        "CPU: {exact}/{} files identical, CER {:.3}% ({total_edits} edits / {total_chars} chars)",
        files.len(),
        cer * 100.0
    );
    assert!(failures.is_empty(), "CPU parity failures:\n{}", failures.join("\n"));
    if !strict {
        assert!(cer <= CPU_CER_LIMIT, "CER {:.3}% > {:.1}%", cer * 100.0, CPU_CER_LIMIT * 100.0);
    }
}

#[test]
fn features_match_python_preprocessor() {
    let Some(golden) = setup(Precision::Int8) else { return };
    let path = fixtures_dir().join(&golden.features_file);
    let features: GoldenFeatures = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (audio, rate) = read_wav(&fixtures_dir().join(&features.file));
    assert_eq!(rate, 16_000);

    let model = Parakeet::new(&golden.model, "cpu");
    let got = model.features(&audio).unwrap();
    assert_eq!(features.shape, vec![1, got.mel, got.frames], "shape");
    assert_eq!(features.valid, got.valid, "features_lens");
    assert_eq!(features.data.len(), got.data.len());
    let (at, worst) = got
        .data
        .iter()
        .zip(&features.data)
        .map(|(a, b)| (a - b).abs())
        .enumerate()
        .fold((0, 0.0f32), |best, (i, d)| if d > best.1 { (i, d) } else { best });
    println!(
        "{}: {} x {} features, max abs diff {worst:.2e} at index {at}",
        features.file, got.mel, got.frames
    );
    assert!(worst <= FEATURE_TOLERANCE, "max abs diff {worst} > {FEATURE_TOLERANCE}");
}

#[cfg(any(feature = "cuda", feature = "webgpu"))]
#[test]
fn gpu_matches_python_golden_within_tolerance() {
    let Some(golden) = setup(Precision::Fp32) else { return };
    if golden.files.values().all(|f| f.cuda.is_none()) {
        println!("skipping: golden.json has no CUDA results");
        return;
    }
    let backend = flow_stt::gpu::backend().expect("a GPU build");
    let model = Parakeet::new(&golden.model, "gpu");
    model.load().expect("load on the GPU (flow gpu says what is missing)");
    let report = model.compute_report().unwrap();
    println!("compute: requested={} actual={} ({})", report.requested, report.actual, report.reason);
    assert_eq!(report.actual, backend);

    let (mut total_edits, mut total_chars) = (0usize, 0usize);
    let mut failures = Vec::new();
    for (name, expected) in sorted_files(&golden) {
        let Some(want) = &expected.cuda else { continue };
        let (audio, rate) = read_wav(&fixtures_dir().join(name));
        let started = Instant::now();
        let got = model.transcribe_detailed(&audio, rate).unwrap();
        let elapsed = started.elapsed().as_secs_f64();
        let (char_edits, token_edits) = edits(&got, want);
        total_edits += char_edits;
        total_chars += want.text.chars().count();
        println!(
            "{name}: {:.1}s audio in {elapsed:.2}s ({:.0}x realtime), {char_edits} char / {token_edits} token edits -> {:?}",
            expected.seconds,
            expected.seconds / elapsed,
            got.text
        );
        if token_edits > GPU_TOKEN_DIFF_LIMIT {
            failures.push(format!(
                "{name}: {token_edits} token edits\n  got  {:?}\n  want {:?}",
                got.text, want.text
            ));
        }
    }
    let cer = cer(total_edits, total_chars);
    println!("{backend} CER over all files: {:.3}% ({total_edits} edits / {total_chars} chars)", cer * 100.0);
    assert!(failures.is_empty(), "{backend} parity failures:\n{}", failures.join("\n"));
    assert!(cer <= GPU_CER_LIMIT, "CER {:.3}% > {:.1}%", cer * 100.0, GPU_CER_LIMIT * 100.0);
}

/// Switching dictation off gives the GPU back (sessions, and CUDA's
/// context once nothing else is loaded); switching on loads again and must
/// recognise exactly as before.
#[cfg(any(feature = "cuda", feature = "webgpu"))]
#[test]
fn gpu_can_be_switched_off_and_on() {
    let Some(golden) = setup(Precision::Fp32) else { return };
    let (name, _) = sorted_files(&golden).into_iter().next().expect("a fixture");
    let (audio, rate) = read_wav(&fixtures_dir().join(name));
    let model = Parakeet::new(&golden.model, "gpu");
    let first = model.transcribe_detailed(&audio, rate).unwrap();
    for _ in 0..2 {
        assert!(model.unload());
        assert!(!model.loaded());
        // False while another test holds a recogniser; freeing is then skipped.
        flow_stt::free_gpu_context();
        let again = model.transcribe_detailed(&audio, rate).unwrap();
        assert_eq!(again.tokens, first.tokens, "{name}");
    }
}

#[test]
fn auto_provider_loads_something() {
    let Some(golden) = setup(Precision::Int8) else { return };
    assert_eq!(golden.model, DEFAULT_STT_MODEL);
    let model = Parakeet::new(&golden.model, "auto");
    model.load().expect("auto never fails when the CPU files are there");
    let report = model.compute_report().unwrap();
    println!("auto -> {} ({})", report.actual, report.reason);
    assert_eq!(report.requested, "auto");
    assert!(["cpu", "cuda", "webgpu"].contains(&report.actual.as_str()), "{}", report.actual);
}
