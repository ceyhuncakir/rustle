//! Fetching a model's files, resumably and verified.
//!
//! Each file in [`model_files`] comes from
//! `https://huggingface.co/{repo}/resolve/main/{file}`, streamed to a `.part`
//! file that a later call resumes with a `Range` request. Hugging Face keeps
//! large files in git LFS, and `raw/main/{file}` then returns the pointer
//! text (`oid sha256:<hex>` and `size <n>`) we verify against; small files
//! are plain git blobs with no published hash, so those are checked by size.
//! `nemo128.onnx` is the exception: it is extracted from the pinned onnx-asr
//! wheel (see [`crate::files`]).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use flow_core::models::{model_dir, model_files, stt_model, Precision, NEMO128_FILE};
use log::{info, warn};
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::files::{
    part_path, sha256_file, verify_nemo128, write_atomic, zip_member, NEMO128_SHA256, NEMO128_WHEEL_MEMBER,
    ONNX_ASR_WHEEL_SHA256, ONNX_ASR_WHEEL_URL,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub file: String,
    pub received: u64,
    /// Total bytes, or 0 when the server did not say.
    pub total: u64,
}

/// The download was stopped through its cancel flag; the `.part` file stays
/// for a later resume.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("download cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Fetch every file of `precision` for model `id` into
/// [`model_dir`], skipping files already present and verified.
pub fn download(
    id: &str,
    precision: Precision,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> anyhow::Result<()> {
    let model = stt_model(id).with_context(|| format!("unknown stt model {id:?}"))?;
    let source = Source {
        hf_base: "https://huggingface.co".into(),
        repo: model.repo.into(),
        wheel_url: ONNX_ASR_WHEEL_URL.into(),
        wheel_sha256: ONNX_ASR_WHEEL_SHA256.into(),
        nemo128_sha256: NEMO128_SHA256.into(),
    };
    download_from(&source, &model_dir(id), precision, cancel, progress)
}

struct Source {
    hf_base: String,
    repo: String,
    wheel_url: String,
    wheel_sha256: String,
    nemo128_sha256: String,
}

/// What we know about a file before fetching it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Expected {
    sha256: Option<String>,
    size: Option<u64>,
}

fn download_from(
    source: &Source,
    dir: &Path,
    precision: Precision,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let client = Client::builder()
        .user_agent(concat!("flow-stt/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .timeout(None)
        .build()?;

    for file in model_files(precision) {
        let dest = dir.join(file);
        if file == NEMO128_FILE {
            fetch_nemo128(&client, source, &dest, cancel, &mut progress)?;
            continue;
        }
        let url = format!("{}/{}/resolve/main/{file}", source.hf_base, source.repo);
        let expected = expected_for(&client, source, file)?;
        if let Some(size) = verified_size(&dest, |dest| matches(dest, &expected)) {
            progress(Progress { file: file.to_string(), received: size, total: size });
            continue;
        }
        let part = part_path(&dest);
        fetch_to(&client, &url, &part, &expected, cancel, |received, total| {
            progress(Progress { file: file.to_string(), received, total })
        })?;
        fs::rename(&part, &dest)
            .with_context(|| format!("renaming {} to {}", part.display(), dest.display()))?;
    }
    Ok(())
}

fn fetch_nemo128(
    client: &Client,
    source: &Source,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
) -> anyhow::Result<()> {
    let report = |received, total| Progress { file: NEMO128_FILE.to_string(), received, total };
    if let Some(size) = verified_size(dest, |dest| verify_nemo128(dest, &source.nemo128_sha256)) {
        progress(report(size, size));
        return Ok(());
    }
    let wheel = dest.with_file_name("onnx_asr.whl.part");
    let expected = Expected { sha256: Some(source.wheel_sha256.clone()), size: None };
    fetch_to(client, &source.wheel_url, &wheel, &expected, cancel, |received, total| {
        progress(report(received, total))
    })?;
    let bytes = fs::read(&wheel)?;
    let member = zip_member(&bytes, NEMO128_WHEEL_MEMBER)
        .context("extracting nemo128.onnx from the onnx-asr wheel")?;
    let actual = format!("{:x}", Sha256::digest(&member));
    if actual != source.nemo128_sha256 {
        bail!("nemo128.onnx from the onnx-asr wheel has sha256 {actual}, expected {}", source.nemo128_sha256);
    }
    write_atomic(dest, &member)?;
    let _ = fs::remove_file(&wheel);
    info!("extracted {} from the onnx-asr wheel", dest.display());
    Ok(())
}

/// The size of `dest` when it is on disk and `verify` accepts it. A file
/// that fails verification is reported and fetched again.
fn verified_size(dest: &Path, verify: impl FnOnce(&Path) -> anyhow::Result<()>) -> Option<u64> {
    if !dest.is_file() {
        return None;
    }
    match verify(dest).and_then(|()| Ok(fs::metadata(dest)?.len())) {
        Ok(size) => {
            info!("{} already present", dest.display());
            Some(size)
        }
        Err(e) => {
            warn!("{} does not verify, fetching again: {e:#}", dest.display());
            None
        }
    }
}

/// Hash and size from the LFS pointer, or the size of a plain blob.
fn expected_for(client: &Client, source: &Source, file: &str) -> anyhow::Result<Expected> {
    let url = format!("{}/{}/raw/main/{file}", source.hf_base, source.repo);
    let response =
        client.get(&url).send().and_then(Response::error_for_status).with_context(|| format!("GET {url}"))?;
    let body = response.bytes().with_context(|| format!("reading {url}"))?;
    Ok(parse_pointer(&body).unwrap_or(Expected { sha256: None, size: Some(body.len() as u64) }))
}

/// `version https://git-lfs.github.com/spec/v1\noid sha256:<hex>\nsize <n>`.
fn parse_pointer(body: &[u8]) -> Option<Expected> {
    let text = std::str::from_utf8(body).ok()?;
    if !text.starts_with("version https://git-lfs.github.com/spec/v1") {
        return None;
    }
    let mut expected = Expected::default();
    for line in text.lines() {
        if let Some(oid) = line.strip_prefix("oid sha256:") {
            expected.sha256 = Some(oid.trim().to_ascii_lowercase());
        } else if let Some(size) = line.strip_prefix("size ") {
            expected.size = size.trim().parse().ok();
        }
    }
    expected.sha256.as_ref()?;
    Some(expected)
}

fn matches(path: &Path, expected: &Expected) -> anyhow::Result<()> {
    if let Some(sha) = &expected.sha256 {
        let actual = sha256_file(path)?;
        if &actual != sha {
            bail!("sha256 {actual} != {sha}");
        }
    }
    if let Some(size) = expected.size {
        let actual = fs::metadata(path)?.len();
        if actual != size {
            bail!("size {actual} != {size}");
        }
    }
    Ok(())
}

/// Stream `url` into `part`, resuming whatever is already there, then verify.
/// Mismatches delete the part so the next attempt starts clean.
fn fetch_to(
    client: &Client,
    url: &str,
    part: &Path,
    expected: &Expected,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> anyhow::Result<()> {
    let started = Instant::now();
    let mut existing = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    if let Some(size) = expected.size {
        if existing > size {
            warn!("{} is larger than the file it is a part of, starting over", part.display());
            fs::remove_file(part)?;
            existing = 0;
        }
    }

    let (mut response, mut received, total) = if existing > 0 && expected.size == Some(existing) {
        // Complete but never verified (say, cancelled between the last byte
        // and the rename).
        info!("{} is complete, verifying", part.display());
        (None, existing, existing)
    } else {
        let mut request = client.get(url);
        if existing > 0 {
            request = request.header(RANGE, format!("bytes={existing}-"));
        }
        let response = request.send().with_context(|| format!("GET {url}"))?;
        match response.status() {
            StatusCode::PARTIAL_CONTENT => {
                let total = content_range_total(&response).unwrap_or(0);
                info!("resuming {url} at byte {existing}");
                (Some(response), existing, total)
            }
            StatusCode::OK => {
                if existing > 0 {
                    info!("{url} does not support resuming, starting over");
                    fs::remove_file(part)?;
                    existing = 0;
                }
                let total = content_length(&response).unwrap_or(0);
                (Some(response), 0, total)
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                // The part is as big as the file; verification below decides.
                (None, existing, existing)
            }
            status => bail!("GET {url}: HTTP {status}"),
        }
    };
    let total = if total == 0 { expected.size.unwrap_or(0) } else { total };

    let mut hasher = Sha256::new();
    if existing > 0 && expected.sha256.is_some() {
        std::io::copy(&mut File::open(part)?, &mut hasher)?;
    }

    if let Some(response) = response.as_mut() {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(part)
            .with_context(|| format!("opening {}", part.display()))?;
        let mut buf = vec![0u8; 256 << 10];
        progress(received, total);
        loop {
            if cancel.load(Ordering::Relaxed) {
                file.flush()?;
                info!("cancelled {url} at {received} bytes; the part file is kept for resuming");
                return Err(Cancelled.into());
            }
            let n = response.read(&mut buf).with_context(|| format!("reading {url}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).with_context(|| format!("writing {}", part.display()))?;
            hasher.update(&buf[..n]);
            received += n as u64;
            progress(received, total);
        }
        file.flush()?;
    }

    let verified = (|| -> anyhow::Result<()> {
        if let Some(sha) = &expected.sha256 {
            let actual = format!("{:x}", hasher.finalize());
            if &actual != sha {
                bail!("sha256 mismatch for {url}: got {actual}, expected {sha}");
            }
        }
        if let Some(size) = expected.size {
            if received != size {
                bail!("size mismatch for {url}: got {received} bytes, expected {size}");
            }
        } else if total > 0 && received != total {
            bail!("size mismatch for {url}: got {received} bytes, Content-Length said {total}");
        }
        Ok(())
    })();
    if let Err(e) = verified {
        let _ = fs::remove_file(part);
        return Err(e);
    }
    let secs = started.elapsed().as_secs_f64();
    info!(
        "fetched {url}: {:.1} MB in {secs:.1}s ({:.1} MB/s)",
        received as f64 / 1e6,
        if secs > 0.0 { received as f64 / 1e6 / secs } else { 0.0 }
    );
    Ok(())
}

fn content_length(response: &Response) -> Option<u64> {
    response.headers().get(CONTENT_LENGTH)?.to_str().ok()?.trim().parse().ok()
}

/// `Content-Range: bytes <from>-<to>/<total>`.
fn content_range_total(response: &Response) -> Option<u64> {
    let value = response.headers().get(CONTENT_RANGE)?.to_str().ok()?;
    value.rsplit_once('/')?.1.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::zip_single_stored;
    use httpmock::prelude::*;
    use std::sync::Mutex;

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn pointer(bytes: &[u8]) -> String {
        format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
            sha(bytes),
            bytes.len()
        )
    }

    fn blob(seed: u32, len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| (i.wrapping_mul(seed) >> 3) as u8).collect()
    }

    /// `acme/model` on the mock server, with no wheel.
    fn hf_source(server: &MockServer) -> Source {
        Source {
            hf_base: server.base_url(),
            repo: "acme/model".into(),
            wheel_url: String::new(),
            wheel_sha256: String::new(),
            nemo128_sha256: String::new(),
        }
    }

    /// Serve `file` as an LFS object: the pointer for `bytes` at `raw/main`
    /// and `served` (normally the same bytes) at `resolve/main`.
    fn mock_lfs(server: &MockServer, file: &str, bytes: &[u8], served: Vec<u8>) {
        let pointer = pointer(bytes);
        server.mock(|when, then| {
            when.method(GET).path(format!("/acme/model/raw/main/{file}"));
            then.status(200).body(pointer);
        });
        server.mock(|when, then| {
            when.method(GET).path(format!("/acme/model/resolve/main/{file}"));
            then.status(200).body(served);
        });
    }

    #[test]
    fn parses_lfs_pointers() {
        let p =
            parse_pointer(b"version https://git-lfs.github.com/spec/v1\noid sha256:ABCD\nsize 42\n").unwrap();
        assert_eq!(p.sha256.as_deref(), Some("abcd"));
        assert_eq!(p.size, Some(42));
        assert!(parse_pointer(b"<unk> 0\n").is_none());
    }

    #[test]
    fn downloads_resumes_and_verifies() {
        let server = MockServer::start();
        let encoder = blob(7, 300_000);
        let decoder = blob(11, 50_000);
        let vocab = b"<unk> 0\n<blk> 1\n".to_vec();
        let nemo = b"pretend this is an onnx graph".to_vec();
        let wheel = zip_single_stored(NEMO128_WHEEL_MEMBER, &nemo);
        let resume_at = 123_456usize;

        server.mock(|when, then| {
            when.method(GET).path("/acme/model/raw/main/encoder-model.int8.onnx");
            then.status(200).body(pointer(&encoder));
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/acme/model/resolve/main/encoder-model.int8.onnx")
                .header("range", format!("bytes={resume_at}-"));
            then.status(206)
                .header("content-range", format!("bytes {resume_at}-{}/{}", encoder.len() - 1, encoder.len()))
                .body(&encoder[resume_at..]);
        });
        mock_lfs(&server, "decoder_joint-model.int8.onnx", &decoder, decoder.clone());
        // A small file is a plain blob: raw/main serves the bytes themselves.
        for route in ["raw", "resolve"] {
            server.mock(|when, then| {
                when.method(GET).path(format!("/acme/model/{route}/main/vocab.txt"));
                then.status(200).body(vocab.clone());
            });
        }
        server.mock(|when, then| {
            when.method(GET).path("/wheel");
            then.status(200).body(wheel.clone());
        });

        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("encoder-model.int8.onnx.part"), &encoder[..resume_at]).unwrap();
        let source = Source {
            wheel_url: format!("{}/wheel", server.base_url()),
            wheel_sha256: sha(&wheel),
            nemo128_sha256: sha(&nemo),
            ..hf_source(&server)
        };
        let seen = Mutex::new(Vec::new());
        download_from(&source, dir.path(), Precision::Int8, &AtomicBool::new(false), |p| {
            seen.lock().unwrap().push(p)
        })
        .unwrap();

        assert_eq!(fs::read(dir.path().join("encoder-model.int8.onnx")).unwrap(), encoder);
        assert_eq!(fs::read(dir.path().join("decoder_joint-model.int8.onnx")).unwrap(), decoder);
        assert_eq!(fs::read(dir.path().join("vocab.txt")).unwrap(), vocab);
        assert_eq!(fs::read(dir.path().join("nemo128.onnx")).unwrap(), nemo);
        assert!(!dir.path().join("encoder-model.int8.onnx.part").exists());
        assert!(!dir.path().join("onnx_asr.whl.part").exists());

        let seen = seen.lock().unwrap();
        let last = |file: &str| seen.iter().rfind(|p| p.file == file).cloned().unwrap();
        let first = |file: &str| seen.iter().find(|p| p.file == file).cloned().unwrap();
        assert_eq!(first("encoder-model.int8.onnx").received, resume_at as u64);
        assert_eq!(last("encoder-model.int8.onnx").received, encoder.len() as u64);
        assert_eq!(last("encoder-model.int8.onnx").total, encoder.len() as u64);
        assert_eq!(last("vocab.txt").received, vocab.len() as u64);
        assert_eq!(last("nemo128.onnx").received, wheel.len() as u64);

        // A second run finds everything present and fetches nothing new.
        let again = Mutex::new(Vec::new());
        download_from(&source, dir.path(), Precision::Int8, &AtomicBool::new(false), |p| {
            again.lock().unwrap().push(p)
        })
        .unwrap();
        assert_eq!(again.lock().unwrap().len(), 4);
    }

    #[test]
    fn rejects_a_bad_hash_and_keeps_nothing() {
        let server = MockServer::start();
        let decoder = blob(3, 10_000);
        let mut wrong = decoder.clone();
        wrong[5] ^= 0xff;
        mock_lfs(&server, "decoder_joint-model.int8.onnx", &decoder, wrong);
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new();
        let expected = expected_for(&client, &hf_source(&server), "decoder_joint-model.int8.onnx").unwrap();
        let part = dir.path().join("decoder_joint-model.int8.onnx.part");
        let url = format!("{}/acme/model/resolve/main/decoder_joint-model.int8.onnx", server.base_url());
        let err = fetch_to(&client, &url, &part, &expected, &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"), "{err}");
        assert!(!part.exists());
    }

    #[test]
    fn cancel_keeps_the_part_file() {
        let server = MockServer::start();
        let encoder = blob(5, 2_000_000);
        mock_lfs(&server, "encoder-model.int8.onnx", &encoder, encoder.clone());
        let dir = tempfile::tempdir().unwrap();
        let client = Client::new();
        let expected = expected_for(&client, &hf_source(&server), "encoder-model.int8.onnx").unwrap();
        let part = dir.path().join("encoder-model.int8.onnx.part");
        let url = format!("{}/acme/model/resolve/main/encoder-model.int8.onnx", server.base_url());
        let cancel = AtomicBool::new(false);
        let err = fetch_to(&client, &url, &part, &expected, &cancel, |received, _| {
            if received > 0 {
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some(), "{err}");
        let kept = fs::metadata(&part).unwrap().len();
        assert!(kept > 0 && kept < encoder.len() as u64, "kept {kept}");
    }
}
