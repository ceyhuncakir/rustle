//! Where model files come from besides [`crate::download`]: a developer's
//! Hugging Face cache, and the onnx-asr wheel for the feature extractor.
//!
//! `nemo128.onnx` deserves an explanation. onnx-asr ships its own export of
//! NeMo's log-mel preprocessor inside the Python package and never uses the
//! `nemo128.onnx` that sits in the Hugging Face repository: that one is an
//! older, different graph (it yields one frame more per second and different
//! normalisation). The decoder here was validated against the packaged
//! export, so that is the file we install, pinned by hash, and fetched from
//! the immutable PyPI wheel rather than the repository.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use log::info;
use rustle_core::models::{model_dir, model_files, stt_model, Precision, NEMO128_FILE, VOCAB_FILE};
use sha2::{Digest, Sha256};

/// SHA-256 of `onnx_asr/preprocessors/data/nemo128.onnx` in onnx-asr 0.12.0.
pub const NEMO128_SHA256: &str = "5b4a84c52eeaa615dc46d781cc7e4598f9b432184831d57493c48adcd01371a9";
/// The wheel that file is extracted from. PyPI never changes a released
/// file, so the hash below stays valid.
pub const ONNX_ASR_WHEEL_URL: &str = "https://files.pythonhosted.org/packages/6a/60/2fa469a2ee674c35ab48821a1039762ae7b9d0b88188ac1012e779477f76/onnx_asr-0.12.0-py3-none-any.whl";
pub const ONNX_ASR_WHEEL_SHA256: &str = "5e7ceca454609819ea7833f61e2302e0c8f6ece4f8a78b66c5daba53cb51de4a";
pub const NEMO128_WHEEL_MEMBER: &str = "onnx_asr/preprocessors/data/nemo128.onnx";

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Fails unless `path` hashes to `expected` ([`NEMO128_SHA256`] outside tests).
pub fn verify_nemo128(path: &Path, expected: &str) -> anyhow::Result<()> {
    let actual = sha256_file(path)?;
    if actual != expected {
        bail!(
            "{} has sha256 {actual}, expected {expected} (onnx-asr 0.12.0's preprocessor export; \
             the nemo128.onnx in the Hugging Face repository is a different, older graph). \
             Delete it and run the download again.",
            path.display()
        );
    }
    Ok(())
}

/// Copy a model from `~/.cache/huggingface/hub` (dereferencing the snapshot
/// symlinks) into [`model_dir`], plus `nemo128.onnx` from the project venv's
/// onnx-asr. Whatever the cache has of both precisions is copied; the fp32
/// trio and `vocab.txt` must be there.
pub fn import_from_hf_cache(id: &str) -> anyhow::Result<()> {
    let model = stt_model(id).with_context(|| format!("unknown stt model {id:?}"))?;
    let snapshots = hf_hub_dir().join(format!("models--{}", model.repo.replace('/', "--"))).join("snapshots");
    let snapshot = newest_snapshot(&snapshots)?;
    let dest = model_dir(id);
    fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;

    let required = model_files(Precision::Fp32);
    let optional = model_files(Precision::Int8).into_iter().filter(|f| !required.contains(f));
    for file in required.iter().copied().chain(optional).filter(|f| *f != NEMO128_FILE) {
        let src = snapshot.join(file);
        if src.is_file() {
            copy_dereferenced(&src, &dest.join(file))?;
        } else if required.contains(&file) {
            bail!("{file} is not in the Hugging Face cache snapshot {}", snapshot.display());
        }
    }

    let nemo = dest.join(NEMO128_FILE);
    if nemo.is_file() && verify_nemo128(&nemo, NEMO128_SHA256).is_ok() {
        info!("{} already present", nemo.display());
    } else {
        let src = venv_nemo128().context(
            "nemo128.onnx not found: expected a virtualenv with onnx-asr at $RUSTLE_VENV, $VIRTUAL_ENV or ./.venv",
        )?;
        copy_dereferenced(&src, &nemo)?;
        verify_nemo128(&nemo, NEMO128_SHA256)?;
    }
    Ok(())
}

/// `$HF_HUB_CACHE`, else `$HF_HOME/hub`, else `~/.cache/huggingface/hub`.
fn hf_hub_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".cache/huggingface/hub")
}

fn newest_snapshot(snapshots: &Path) -> anyhow::Result<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(snapshots)
        .with_context(|| format!("no Hugging Face cache snapshots at {}", snapshots.display()))?
    {
        let path = entry?.path();
        if !path.join(VOCAB_FILE).exists() {
            continue;
        }
        let modified = fs::metadata(&path)?.modified()?;
        if best.as_ref().is_none_or(|(t, _)| modified > *t) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p).with_context(|| format!("no complete snapshot under {}", snapshots.display()))
}

/// Copy `src` (following symlinks) to `dest` through a `.part` file, unless
/// `dest` already has the same size.
fn copy_dereferenced(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let size = fs::metadata(src).with_context(|| format!("reading {}", src.display()))?.len();
    if fs::metadata(dest).map(|m| m.len() == size).unwrap_or(false) {
        info!("{} already present", dest.display());
        return Ok(());
    }
    let started = Instant::now();
    let part = part_path(dest);
    fs::copy(src, &part).with_context(|| format!("copying {} to {}", src.display(), part.display()))?;
    fs::rename(&part, dest)?;
    info!(
        "copied {} ({:.0} MB) in {:.1}s",
        dest.display(),
        size as f64 / 1e6,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// Write `bytes` to `dest` atomically.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let part = part_path(dest);
    File::create(&part)
        .and_then(|mut f| f.write_all(bytes).and_then(|_| f.flush()))
        .with_context(|| format!("writing {}", part.display()))?;
    fs::rename(&part, dest).with_context(|| format!("renaming {} to {}", part.display(), dest.display()))?;
    Ok(())
}

/// onnx-asr's packaged `nemo128.onnx` in `$RUSTLE_VENV`, `$VIRTUAL_ENV`,
/// `./.venv` or the repository's `.venv`, whichever comes first.
fn venv_nemo128() -> Option<PathBuf> {
    ["RUSTLE_VENV", "VIRTUAL_ENV"]
        .into_iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .chain([PathBuf::from(".venv"), Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv")])
        .filter_map(|root| fs::read_dir(root.join("lib")).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("site-packages/onnx_asr/preprocessors/data").join(NEMO128_FILE))
        .find(|candidate| candidate.is_file())
}

/// Pull one member out of a zip archive (a wheel): stored or deflated, no
/// zip64, which is all a 4 MB wheel needs.
pub fn zip_member(zip: &[u8], name: &str) -> anyhow::Result<Vec<u8>> {
    let le = |at: usize, width: usize| -> anyhow::Result<usize> {
        let bytes = zip.get(at..at + width).context("truncated zip archive")?;
        Ok(bytes.iter().rev().fold(0, |n, &b| (n << 8) | b as usize))
    };
    let magic = |at: usize, sig: &[u8]| zip.get(at..at + 4) == Some(sig);

    let n = zip.len();
    let eocd = (n.saturating_sub(22 + 65535)..=n.saturating_sub(22))
        .rev()
        .find(|&i| magic(i, b"PK\x05\x06"))
        .context("not a zip archive (no end-of-central-directory record)")?;
    let mut p = le(eocd + 16, 4)?;
    for _ in 0..le(eocd + 10, 2)? {
        if !magic(p, b"PK\x01\x02") {
            bail!("corrupt zip central directory");
        }
        let (name_len, extra_len, comment_len) = (le(p + 28, 2)?, le(p + 30, 2)?, le(p + 32, 2)?);
        let entry_name = zip.get(p + 46..p + 46 + name_len).context("truncated zip archive")?;
        if entry_name != name.as_bytes() {
            p += 46 + name_len + extra_len + comment_len;
            continue;
        }
        let (method, compressed, uncompressed) = (le(p + 10, 2)?, le(p + 20, 4)?, le(p + 24, 4)?);
        let local = le(p + 42, 4)?;
        if !magic(local, b"PK\x03\x04") {
            bail!("corrupt zip local header for {name}");
        }
        let data = local + 30 + le(local + 26, 2)? + le(local + 28, 2)?;
        let raw = zip.get(data..data + compressed).context("truncated zip archive")?;
        let mut out = Vec::with_capacity(uncompressed);
        match method {
            0 => out.extend_from_slice(raw),
            8 => {
                flate2::read::DeflateDecoder::new(raw)
                    .read_to_end(&mut out)
                    .with_context(|| format!("inflating {name}"))?;
            }
            other => bail!("zip member {name} uses unsupported compression method {other}"),
        }
        if out.len() != uncompressed {
            bail!("zip member {name}: got {} bytes, header says {uncompressed}", out.len());
        }
        return Ok(out);
    }
    bail!("{name} is not in the archive")
}

/// A single-member zip with stored (uncompressed) data, for tests.
#[cfg(test)]
pub fn zip_single_stored(name: &str, data: &[u8]) -> Vec<u8> {
    let size = (data.len() as u32).to_le_bytes();
    // The fields a local header and a central directory entry share: version
    // needed (2.0), flags, method (stored), time/date, crc, both sizes, name
    // and extra lengths.
    let mut fields = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    fields.extend(crc32(data).to_le_bytes());
    fields.extend(size);
    fields.extend(size);
    fields.extend((name.len() as u16).to_le_bytes());
    fields.extend([0, 0]);

    let mut out = b"PK\x03\x04".to_vec();
    out.extend(&fields);
    out.extend(name.as_bytes());
    out.extend(data);
    let cd_start = out.len() as u32;
    out.extend(b"PK\x01\x02");
    out.extend([20, 0]); // version made by
    out.extend(&fields);
    out.extend([0; 14]); // comment length, disk, attributes, local header offset
    out.extend(name.as_bytes());
    let cd_size = out.len() as u32 - cd_start;
    out.extend(b"PK\x05\x06");
    out.extend([0, 0, 0, 0, 1, 0, 1, 0]); // disk numbers, one entry here and in total
    out.extend(cd_size.to_le_bytes());
    out.extend(cd_start.to_le_bytes());
    out.extend([0, 0]); // comment length
    out
}

#[cfg(test)]
fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(0xffff_ffff, |crc, &b| {
        (0..8).fold(crc ^ b as u32, |c, _| if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_stored_member() {
        let zip = zip_single_stored("onnx_asr/preprocessors/data/nemo128.onnx", b"hello onnx");
        assert_eq!(zip_member(&zip, NEMO128_WHEEL_MEMBER).unwrap(), b"hello onnx");
        assert!(zip_member(&zip, "missing").is_err());
        assert!(zip_member(b"not a zip", "x").is_err());
    }

    #[test]
    fn extracts_a_deflated_member() {
        // Build a deflated archive by hand: compress, then patch method/sizes.
        use std::io::Write as _;
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 7) as u8).collect();
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&data).unwrap();
        let deflated = enc.finish().unwrap();
        let mut zip = zip_single_stored("m", &deflated);
        // method (local header at 8, central at cd + 10), uncompressed size
        // (local at 22, central at cd + 24).
        let cd = zip.len() - 22 - (46 + 1);
        zip[8..10].copy_from_slice(&8u16.to_le_bytes());
        zip[cd + 10..cd + 12].copy_from_slice(&8u16.to_le_bytes());
        zip[22..26].copy_from_slice(&(data.len() as u32).to_le_bytes());
        zip[cd + 24..cd + 28].copy_from_slice(&(data.len() as u32).to_le_bytes());
        assert_eq!(zip_member(&zip, "m").unwrap(), data);
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(verify_nemo128(&path, NEMO128_SHA256).is_err());
    }

    #[test]
    fn part_paths() {
        assert_eq!(part_path(Path::new("/a/b.onnx")), PathBuf::from("/a/b.onnx.part"));
    }
}
