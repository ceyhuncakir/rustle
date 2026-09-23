//! Fetching a model's files, resumably and verified.
//!
//! Each file in [`model_files`] comes from the model's Hugging Face
//! repository at a pinned commit (`resolve/<commit>/<file>`) and must match
//! the SHA-256 and size recorded in [`PINNED`] before it is put in place. The
//! repositories belong to a third party: pinning the commit and the hashes
//! means nothing pushed there later, by its owner or by whoever gets hold of
//! the account, changes what Flow runs. `nemo128.onnx` is the exception: it
//! is extracted from the pinned onnx-asr wheel (see [`crate::files`]).
//!
//! A file is streamed to a `.part` beside it, which a later call resumes
//! with a `Range` request. Only one download of a model runs in a process,
//! and the `.part` is locked while it is written, so a second Flow process
//! cannot append to it either. The finished `.part` is hashed as it is on
//! disk, still locked, and only then renamed into place.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use flow_core::models::{model_dir, model_files, stt_model, Precision, NEMO128_FILE};
use log::{debug, info, warn};
use reqwest::blocking::{Client, Response};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};

use crate::files::{
    part_path, sha256_file, verify_nemo128, write_atomic, zip_member, NEMO128_SHA256, NEMO128_WHEEL_MEMBER,
    ONNX_ASR_WHEEL_SHA256, ONNX_ASR_WHEEL_URL,
};

/// A model repository at a fixed commit, with the SHA-256 and size of every
/// file taken from it, both precisions. From
/// `https://huggingface.co/api/models/<repo>?blobs=true` (the LFS `sha256`;
/// `vocab.txt` is a plain git blob, so hashed after fetching it at the
/// commit). The v3 hashes match the files the parity tests passed with.
/// Moving to a newer commit means taking every hash from that commit.
struct Pinned {
    repo: &'static str,
    commit: &'static str,
    /// `(file, sha256, size)`.
    files: &'static [(&'static str, &'static str, u64)],
}

const PINNED: &[Pinned] = &[
    Pinned {
        repo: "istupakov/parakeet-tdt-0.6b-v3-onnx",
        commit: "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
        files: &[
            ("vocab.txt", "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d", 93_939),
            (
                "encoder-model.int8.onnx",
                "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
                652_183_999,
            ),
            (
                "decoder_joint-model.int8.onnx",
                "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
                18_202_004,
            ),
            (
                "encoder-model.onnx",
                "98a74b21b4cc0017c1e7030319a4a96f4a9506e50f0708f3a516d02a77c96bb1",
                41_770_866,
            ),
            (
                "encoder-model.onnx.data",
                "9a22d372c51455c34f13405da2520baefb7125bd16981397561423ed32d24f36",
                2_435_420_160,
            ),
            (
                "decoder_joint-model.onnx",
                "e978ddf6688527182c10fde2eb4b83068421648985ef23f7a86be732be8706c1",
                72_520_893,
            ),
        ],
    },
    Pinned {
        repo: "istupakov/parakeet-tdt-0.6b-v2-onnx",
        commit: "0bbb45a3365852604aef28b538a8f066f4ccaa85",
        files: &[
            ("vocab.txt", "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d", 9_384),
            (
                "encoder-model.int8.onnx",
                "3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155",
                652_184_014,
            ),
            (
                "decoder_joint-model.int8.onnx",
                "a449f49acd68979d418651dd2dcb737cc0f1bf0225e009e29ee326354edbf7d3",
                8_998_286,
            ),
            (
                "encoder-model.onnx",
                "3987bcd28175d829d12888a996a84e8f62a0e374d9ffd640662c1515adc679d3",
                41_770_866,
            ),
            (
                "encoder-model.onnx.data",
                "4dab7362d4874d85965045b1e41b2d61dd2cc0fb25671a7f6b3dc47bf120cc41",
                2_435_420_160,
            ),
            (
                "decoder_joint-model.onnx",
                "cbb52a07bd70ab5b67f8439d4b3cd8704b18467b4430bcacb5adabe154b8d191",
                35_792_059,
            ),
        ],
    },
];

/// How long the server may send nothing, before the response starts or in
/// the middle of it, before the download fails as stalled. The `.part` is
/// kept, so the next attempt resumes.
const STALL: Duration = Duration::from_secs(30);

/// How often a download waiting on the network looks at its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(200);

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
    let pinned = PINNED
        .iter()
        .find(|p| p.repo == model.repo)
        .with_context(|| format!("no pinned commit and hashes for {}", model.repo))?;
    let source = Source {
        base_url: format!("https://huggingface.co/{}/resolve/{}", pinned.repo, pinned.commit),
        files: pinned
            .files
            .iter()
            .map(|&(file, sha256, size)| {
                (file.to_string(), Expected { sha256: sha256.into(), size: Some(size) })
            })
            .collect(),
        wheel_url: ONNX_ASR_WHEEL_URL.into(),
        wheel_sha256: ONNX_ASR_WHEEL_SHA256.into(),
        nemo128_sha256: NEMO128_SHA256.into(),
        stall: STALL,
    };
    download_from(&source, &model_dir(id), precision, cancel, progress)
}

struct Source {
    /// Where the files are, e.g. `https://huggingface.co/<repo>/resolve/<commit>`.
    base_url: String,
    /// What each file there must be.
    files: Vec<(String, Expected)>,
    wheel_url: String,
    wheel_sha256: String,
    nemo128_sha256: String,
    stall: Duration,
}

impl Source {
    fn expected(&self, file: &str) -> anyhow::Result<&Expected> {
        self.files
            .iter()
            .find(|(f, _)| f == file)
            .map(|(_, expected)| expected)
            .with_context(|| format!("no pinned hash for {file}; refusing to fetch it unverified"))
    }
}

/// What a file must be.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Expected {
    sha256: String,
    /// Unknown for the wheel, which is checked by hash alone.
    size: Option<u64>,
}

/// Model folders being downloaded in this process.
static DOWNLOADING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// This process's claim on a model folder, given up on drop.
struct Busy(PathBuf);

impl Busy {
    fn claim(dir: &Path) -> anyhow::Result<Busy> {
        let mut busy = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner());
        if busy.iter().any(|d| d == dir) {
            bail!("this model is already downloading (into {})", dir.display());
        }
        busy.push(dir.to_path_buf());
        Ok(Busy(dir.to_path_buf()))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).retain(|d| *d != self.0);
    }
}

fn download_from(
    source: &Source,
    dir: &Path,
    precision: Precision,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> anyhow::Result<()> {
    let _busy = Busy::claim(dir)?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let net = Net::new(source.stall)?;

    for file in model_files(precision) {
        let dest = dir.join(file);
        if file == NEMO128_FILE {
            fetch_nemo128(&net, source, &dest, cancel, &mut progress)?;
            continue;
        }
        let expected = source.expected(file)?;
        if let Some(size) = verified_size(&dest, |dest| matches(dest, expected)) {
            progress(Progress { file: file.to_string(), received: size, total: size });
            continue;
        }
        let url = format!("{}/{file}", source.base_url);
        fetch_to(&net, &url, &dest, expected, cancel, |received, total| {
            progress(Progress { file: file.to_string(), received, total })
        })?;
    }
    Ok(())
}

/// The HTTP client, and the stall limit it was built with.
struct Net {
    client: Client,
    stall: Duration,
}

impl Net {
    fn new(stall: Duration) -> anyhow::Result<Net> {
        // reqwest's blocking client applies `timeout` to waiting for the
        // response and to each read of the body on its own, not to the
        // whole transfer, so it is the stall limit and a large file can
        // take as long as it needs.
        let client = Client::builder()
            .user_agent(concat!("flow-stt/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(stall)
            .timeout(stall)
            .build()?;
        Ok(Net { client, stall })
    }
}

fn fetch_nemo128(
    net: &Net,
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
    let wheel = dest.with_file_name("onnx_asr.whl");
    let expected = Expected { sha256: source.wheel_sha256.clone(), size: None };
    fetch_to(net, &source.wheel_url, &wheel, &expected, cancel, |received, total| {
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

fn matches(path: &Path, expected: &Expected) -> anyhow::Result<()> {
    if let Some(size) = expected.size {
        let actual = fs::metadata(path)?.len();
        if actual != size {
            bail!("size {actual} != {size}");
        }
    }
    let actual = sha256_file(path)?;
    if actual != expected.sha256 {
        bail!("sha256 {actual} != {}", expected.sha256);
    }
    Ok(())
}

/// Stream `url` into `dest`'s `.part`, resuming whatever is already there,
/// verify the file on disk, and rename it to `dest`. A mismatch deletes the
/// part so the next attempt starts clean; any other failure keeps it for
/// resuming.
fn fetch_to(
    net: &Net,
    url: &str,
    dest: &Path,
    expected: &Expected,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, u64),
) -> anyhow::Result<()> {
    let started = Instant::now();
    let part = part_path(dest);
    // Read and written through this one handle only: on Windows the lock
    // is mandatory and would refuse any other handle, even our own.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&part)
        .with_context(|| format!("opening {}", part.display()))?;
    lock_part(&file, &part)?;

    let mut received = file.metadata()?.len();
    if expected.size.is_some_and(|size| received > size) {
        warn!("{} is larger than the file it is a part of, starting over", part.display());
        file.set_len(0)?;
        received = 0;
    }
    let mut total = expected.size.unwrap_or(0);

    // Complete but never verified (say, cancelled between the last byte and
    // the rename): no request needed.
    if received == 0 || expected.size != Some(received) {
        let stream = Stream::start(net, url, received)?;
        let Some(Chunk::Head { status, total: length }) = stream.next(cancel)? else {
            bail!("GET {url}: no response")
        };
        match status {
            StatusCode::PARTIAL_CONTENT => info!("resuming {url} at byte {received}"),
            StatusCode::OK => {
                if received > 0 {
                    info!("{url} does not support resuming, starting over");
                    file.set_len(0)?;
                    received = 0;
                }
            }
            // The part is as big as the file; verification below decides.
            StatusCode::RANGE_NOT_SATISFIABLE => {}
            status => bail!("GET {url}: HTTP {status}"),
        }
        if status != StatusCode::RANGE_NOT_SATISFIABLE {
            if total == 0 {
                total = length.unwrap_or(0);
            }
            file.seek(SeekFrom::Start(received))?;
            progress(received, total);
            loop {
                let bytes = match stream.next(cancel) {
                    Ok(Some(Chunk::Data(bytes))) => bytes,
                    Ok(Some(_)) => continue,
                    Ok(None) => break,
                    Err(e) => {
                        if e.is::<Cancelled>() {
                            info!("cancelled {url} at {received} bytes; the part file is kept for resuming");
                        }
                        return Err(e);
                    }
                };
                file.write_all(&bytes).with_context(|| format!("writing {}", part.display()))?;
                received += bytes.len() as u64;
                progress(received, total);
            }
            // A connection that ended early is not a bad file: keep what came.
            if expected.size.is_some_and(|size| received < size) {
                bail!("{url} ended after {received} of {total} bytes; the next attempt resumes");
            }
        }
    }

    if let Err(e) = verify_open(&mut file, expected) {
        // Emptied first, while still ours, so no other process resumes
        // from bytes known to be wrong.
        let _ = file.set_len(0);
        drop(file);
        let _ = fs::remove_file(&part);
        return Err(e.context(format!("{url} does not match its pinned hash")));
    }
    // Renamed while still locked, so nobody appends between the check and
    // the rename. (std opens files with FILE_SHARE_DELETE on Windows, which
    // is what renaming an open file there takes.)
    fs::rename(&part, dest).with_context(|| format!("renaming {} to {}", part.display(), dest.display()))?;
    drop(file);
    let secs = started.elapsed().as_secs_f64();
    info!(
        "fetched {url}: {:.1} MB in {secs:.1}s ({:.1} MB/s)",
        received as f64 / 1e6,
        if secs > 0.0 { received as f64 / 1e6 / secs } else { 0.0 }
    );
    Ok(())
}

/// Take the `.part` for this process, so two Flow processes never append to
/// one file. Where the file system has no locks the download goes ahead
/// unlocked; [`Busy`] still covers this process.
fn lock_part(file: &File, part: &Path) -> anyhow::Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(fs::TryLockError::WouldBlock) => {
            bail!("{} is being downloaded by another Flow process", part.display())
        }
        Err(fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
            debug!("{} cannot be locked here ({e}); going on without", part.display());
            Ok(())
        }
        Err(fs::TryLockError::Error(e)) => Err(anyhow!(e).context(format!("locking {}", part.display()))),
    }
}

/// Hash `file` as it is on disk, through the handle that holds the lock.
fn verify_open(file: &mut File, expected: &Expected) -> anyhow::Result<()> {
    let size = file.metadata()?.len();
    if let Some(want) = expected.size {
        if size != want {
            bail!("got {size} bytes, expected {want}");
        }
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    std::io::copy(file, &mut hasher)?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected.sha256 {
        bail!("sha256 is {actual}, expected {}", expected.sha256);
    }
    Ok(())
}

enum Chunk {
    /// The status, and the whole file's size when the headers say.
    Head {
        status: StatusCode,
        total: Option<u64>,
    },
    Data(Vec<u8>),
    Failed(anyhow::Error),
}

/// A GET running on a thread of its own, so that waiting on the network
/// never holds up cancelling: [`Stream::next`] waits on a channel and looks
/// at the cancel flag a few times a second. A stall still ends the download,
/// through the client's timeout. A cancelled request's thread notices the
/// channel is gone at its next read and ends.
struct Stream {
    rx: Receiver<Chunk>,
}

impl Stream {
    fn start(net: &Net, url: &str, from: u64) -> anyhow::Result<Stream> {
        // A few MB in flight at most while the disk catches up.
        let (tx, rx) = mpsc::sync_channel(64);
        let stall = net.stall;
        let mut request = net.client.get(url);
        if from > 0 {
            request = request.header(RANGE, format!("bytes={from}-"));
        }
        let url = url.to_string();
        std::thread::Builder::new().name("flow-download-net".into()).spawn(move || {
            let mut response = match request.send() {
                Ok(response) => response,
                Err(e) => {
                    let _ = tx.send(Chunk::Failed(network_error(&url, e, stall)));
                    return;
                }
            };
            let status = response.status();
            let total = match status {
                StatusCode::PARTIAL_CONTENT => content_range_total(&response),
                _ => content_length(&response),
            };
            if tx.send(Chunk::Head { status, total }).is_err() || !status.is_success() {
                return;
            }
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match response.read(&mut buf) {
                    Ok(0) => return,
                    Ok(n) => {
                        if tx.send(Chunk::Data(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let timed_out = e
                            .get_ref()
                            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
                            .is_some_and(reqwest::Error::is_timeout);
                        let error = if timed_out {
                            anyhow!("{url} sent nothing for {stall:?}; the connection stalled")
                        } else {
                            anyhow!(e).context(format!("reading {url}"))
                        };
                        let _ = tx.send(Chunk::Failed(error));
                        return;
                    }
                }
            }
        })?;
        Ok(Stream { rx })
    }

    /// The next message; `None` once the body is complete.
    fn next(&self, cancel: &AtomicBool) -> anyhow::Result<Option<Chunk>> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            match self.rx.recv_timeout(CANCEL_POLL) {
                Ok(Chunk::Failed(e)) => return Err(e),
                Ok(chunk) => return Ok(Some(chunk)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }
}

fn network_error(url: &str, e: reqwest::Error, stall: Duration) -> anyhow::Error {
    if e.is_timeout() {
        anyhow!("no answer from {url} in {stall:?}")
    } else {
        anyhow!(e).context(format!("GET {url}"))
    }
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
    use flow_core::models::STT_MODELS;
    use httpmock::prelude::*;

    fn sha(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn expect(bytes: &[u8]) -> Expected {
        Expected { sha256: sha(bytes), size: Some(bytes.len() as u64) }
    }

    fn blob(seed: u32, len: usize) -> Vec<u8> {
        (0..len as u32).map(|i| (i.wrapping_mul(seed) >> 3) as u8).collect()
    }

    /// `acme/model` at commit `abc` on the mock server, pinned to `files`,
    /// with no wheel.
    fn source(server: &MockServer, files: &[(&str, &[u8])]) -> Source {
        Source {
            base_url: format!("{}/acme/model/resolve/abc", server.base_url()),
            files: files.iter().map(|(f, bytes)| (f.to_string(), expect(bytes))).collect(),
            wheel_url: String::new(),
            wheel_sha256: String::new(),
            nemo128_sha256: String::new(),
            stall: Duration::from_secs(10),
        }
    }

    fn serve(server: &MockServer, file: &str, bytes: Vec<u8>) {
        server.mock(|when, then| {
            when.method(GET).path(format!("/acme/model/resolve/abc/{file}"));
            then.status(200).body(bytes);
        });
    }

    fn url(server: &MockServer, file: &str) -> String {
        format!("{}/acme/model/resolve/abc/{file}", server.base_url())
    }

    #[test]
    fn every_model_is_pinned_with_hashes_for_both_precisions() {
        let hex = |s: &str, len| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        for model in STT_MODELS {
            let pinned = PINNED.iter().find(|p| p.repo == model.repo).expect(model.repo);
            assert!(hex(pinned.commit, 40), "{}: {}", model.repo, pinned.commit);
            for precision in [Precision::Int8, Precision::Fp32] {
                for file in model_files(precision).into_iter().filter(|f| *f != NEMO128_FILE) {
                    let (_, sha, size) =
                        pinned.files.iter().find(|(f, _, _)| *f == file).unwrap_or_else(|| panic!("{file}"));
                    assert!(hex(sha, 64) && *size > 0, "{}: {file}", model.repo);
                }
            }
        }
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
            when.method(GET)
                .path("/acme/model/resolve/abc/encoder-model.int8.onnx")
                .header("range", format!("bytes={resume_at}-"));
            then.status(206)
                .header("content-range", format!("bytes {resume_at}-{}/{}", encoder.len() - 1, encoder.len()))
                .body(&encoder[resume_at..]);
        });
        serve(&server, "decoder_joint-model.int8.onnx", decoder.clone());
        serve(&server, "vocab.txt", vocab.clone());
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
            ..source(
                &server,
                &[
                    ("encoder-model.int8.onnx", &encoder),
                    ("decoder_joint-model.int8.onnx", &decoder),
                    ("vocab.txt", &vocab),
                ],
            )
        };
        let mut seen = Vec::new();
        download_from(&source, dir.path(), Precision::Int8, &AtomicBool::new(false), |p| seen.push(p))
            .unwrap();

        assert_eq!(fs::read(dir.path().join("encoder-model.int8.onnx")).unwrap(), encoder);
        assert_eq!(fs::read(dir.path().join("decoder_joint-model.int8.onnx")).unwrap(), decoder);
        assert_eq!(fs::read(dir.path().join("vocab.txt")).unwrap(), vocab);
        assert_eq!(fs::read(dir.path().join("nemo128.onnx")).unwrap(), nemo);
        assert!(!dir.path().join("encoder-model.int8.onnx.part").exists());
        assert!(!dir.path().join("onnx_asr.whl").exists());
        assert!(!dir.path().join("onnx_asr.whl.part").exists());

        let last = |file: &str| seen.iter().rfind(|p| p.file == file).cloned().unwrap();
        let first = |file: &str| seen.iter().find(|p| p.file == file).cloned().unwrap();
        assert_eq!(first("encoder-model.int8.onnx").received, resume_at as u64);
        assert_eq!(last("encoder-model.int8.onnx").received, encoder.len() as u64);
        assert_eq!(last("encoder-model.int8.onnx").total, encoder.len() as u64);
        assert_eq!(last("vocab.txt").received, vocab.len() as u64);
        assert_eq!(last("nemo128.onnx").received, wheel.len() as u64);

        // A second run finds everything present and fetches nothing new.
        let mut again = Vec::new();
        download_from(&source, dir.path(), Precision::Int8, &AtomicBool::new(false), |p| again.push(p))
            .unwrap();
        assert_eq!(again.len(), 4);
    }

    #[test]
    fn a_file_without_a_pinned_hash_is_not_fetched() {
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let source = Source { nemo128_sha256: sha(b"x"), ..source(&server, &[]) };
        fs::write(dir.path().join(NEMO128_FILE), b"x").unwrap();
        let err =
            download_from(&source, dir.path(), Precision::Int8, &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(err.to_string().contains("no pinned hash"), "{err}");
    }

    #[test]
    fn rejects_a_bad_hash_and_keeps_nothing() {
        let server = MockServer::start();
        let decoder = blob(3, 10_000);
        let mut wrong = decoder.clone();
        wrong[5] ^= 0xff;
        serve(&server, "decoder_joint-model.int8.onnx", wrong);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("decoder_joint-model.int8.onnx");
        let net = Net::new(Duration::from_secs(10)).unwrap();
        let url = url(&server, "decoder_joint-model.int8.onnx");
        let err =
            fetch_to(&net, &url, &dest, &expect(&decoder), &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(format!("{err:#}").contains("sha256"), "{err:#}");
        assert!(!part_path(&dest).exists());
        assert!(!dest.exists());
    }

    #[test]
    fn a_complete_part_is_hashed_on_disk_before_it_is_renamed() {
        // Right size, wrong bytes, as two writers appending to one part
        // would leave it: nothing is fetched, and nothing is put in place.
        let server = MockServer::start();
        let decoder = blob(3, 10_000);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("decoder_joint-model.int8.onnx");
        fs::write(part_path(&dest), blob(5, 10_000)).unwrap();
        let net = Net::new(Duration::from_secs(10)).unwrap();
        let url = url(&server, "decoder_joint-model.int8.onnx");
        let err =
            fetch_to(&net, &url, &dest, &expect(&decoder), &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(format!("{err:#}").contains("sha256"), "{err:#}");
        assert!(!dest.exists());
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn a_connection_that_ends_early_keeps_the_part() {
        let server = MockServer::start();
        let encoder = blob(5, 100_000);
        serve(&server, "encoder-model.int8.onnx", encoder[..40_000].to_vec());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("encoder-model.int8.onnx");
        let net = Net::new(Duration::from_secs(10)).unwrap();
        let url = url(&server, "encoder-model.int8.onnx");
        let err =
            fetch_to(&net, &url, &dest, &expect(&encoder), &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("ended after 40000"), "{err}");
        assert_eq!(fs::read(part_path(&dest)).unwrap(), encoder[..40_000]);
    }

    #[test]
    fn cancel_keeps_the_part_file() {
        let server = MockServer::start();
        let encoder = blob(5, 2_000_000);
        serve(&server, "encoder-model.int8.onnx", encoder.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("encoder-model.int8.onnx");
        let net = Net::new(Duration::from_secs(10)).unwrap();
        let cancel = AtomicBool::new(false);
        let err =
            fetch_to(&net, &url(&server, "encoder-model.int8.onnx"), &dest, &expect(&encoder), &cancel, {
                |received, _| {
                    if received > 0 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                }
            })
            .unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some(), "{err}");
        let kept = fs::metadata(part_path(&dest)).unwrap().len();
        assert!(kept > 0 && kept < encoder.len() as u64, "kept {kept}");
        assert!(!dest.exists());
    }

    #[test]
    fn cancel_is_prompt_while_the_server_says_nothing() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/acme/model/resolve/abc/slow");
            then.status(200).body("late").delay(Duration::from_secs(20));
        });
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new(Duration::from_secs(30)).unwrap();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let err = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::Relaxed);
            });
            fetch_to(
                &net,
                &url(&server, "slow"),
                &dir.path().join("slow"),
                &expect(b"late"),
                &cancel,
                |_, _| {},
            )
            .unwrap_err()
        });
        assert!(err.downcast_ref::<Cancelled>().is_some(), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    }

    #[test]
    fn a_stalled_server_is_an_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/acme/model/resolve/abc/slow");
            then.status(200).body("late").delay(Duration::from_secs(5));
        });
        let dir = tempfile::tempdir().unwrap();
        let net = Net::new(Duration::from_millis(500)).unwrap();
        let dest = dir.path().join("slow");
        let err = fetch_to(
            &net,
            &url(&server, "slow"),
            &dest,
            &expect(b"late"),
            &AtomicBool::new(false),
            |_, _| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("no answer"), "{err:#}");
        assert!(!dest.exists());
    }

    #[test]
    fn a_second_download_of_the_same_model_is_refused() {
        let server = MockServer::start();
        let dir = tempfile::tempdir().unwrap();
        let held = Busy::claim(dir.path()).unwrap();
        let err = download_from(
            &source(&server, &[]),
            dir.path(),
            Precision::Int8,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("already downloading"), "{err}");
        drop(held);
        drop(Busy::claim(dir.path()).unwrap());
    }

    #[test]
    fn a_part_locked_elsewhere_is_left_alone() {
        let server = MockServer::start();
        let decoder = blob(3, 10_000);
        serve(&server, "decoder_joint-model.int8.onnx", decoder.clone());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("decoder_joint-model.int8.onnx");
        // A separate open file, as another process would have.
        let other = File::create(part_path(&dest)).unwrap();
        other.lock().unwrap();
        let net = Net::new(Duration::from_secs(10)).unwrap();
        let url = url(&server, "decoder_joint-model.int8.onnx");
        let err =
            fetch_to(&net, &url, &dest, &expect(&decoder), &AtomicBool::new(false), |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("another Flow process"), "{err}");
        assert!(!dest.exists());
        drop(other);
        fetch_to(&net, &url, &dest, &expect(&decoder), &AtomicBool::new(false), |_, _| {}).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), decoder);
    }
}
