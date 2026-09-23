//! Run a wav file through the recogniser and print what came out, token ids
//! and encoder frames included, for chasing a parity difference on a file
//! the fixtures do not cover. (`flow models` and `flow dictate` in the app
//! cover downloading, importing and live dictation.)
//!
//! ```text
//! cargo run -p flow-stt --example stt -- FILE.wav [auto|gpu|cpu]
//! ```
//!
//! Any PCM or float wav will do; more than one channel is mixed down to
//! mono, as a microphone take is.

use flow_core::engine::Transcriber;
use flow_core::models::DEFAULT_STT_MODEL;
use flow_stt::Parakeet;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: stt FILE.wav [auto|gpu|cpu]");
        return Ok(());
    };
    let provider = args.get(1).map_or("auto", String::as_str);
    let id = std::env::var("FLOW_STT_MODEL").unwrap_or_else(|_| DEFAULT_STT_MODEL.to_string());

    let (audio, sample_rate) = read_wav(path)?;

    let model = Parakeet::new(&id, provider);
    model.load()?;
    let report = model.compute_report().expect("loaded");
    eprintln!("{} -> {} ({})", report.requested, report.actual, report.reason);
    let result = model.transcribe_detailed(&audio, sample_rate)?;
    println!("{}", result.text);
    eprintln!("tokens {:?}", result.tokens);
    eprintln!("frames {:?}", result.frames);
    Ok(())
}

/// Mono samples in `-1.0..1.0` and the sample rate.
fn read_wav(path: &str) -> anyhow::Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            anyhow::ensure!(
                (1..=32).contains(&spec.bits_per_sample),
                "{path}: {} bits per sample",
                spec.bits_per_sample
            );
            let full_scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|s| s as f32 / full_scale)).collect::<Result<_, _>>()?
        }
    };
    let channels = usize::from(spec.channels.max(1));
    let mono =
        samples.chunks_exact(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32).collect();
    Ok((mono, spec.sample_rate))
}
