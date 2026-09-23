//! Run a wav file through the recogniser and print what came out, token ids
//! and encoder frames included, for chasing a parity difference on a file
//! the fixtures do not cover. (`flow models` and `flow dictate` in the app
//! cover downloading, importing and live dictation.)
//!
//! ```text
//! cargo run -p flow-stt --example stt -- FILE.wav [auto|cuda|cpu]
//! ```

use flow_core::engine::Transcriber;
use flow_core::models::DEFAULT_STT_MODEL;
use flow_stt::Parakeet;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: stt FILE.wav [auto|cuda|cpu]");
        return Ok(());
    };
    let provider = args.get(1).map_or("auto", String::as_str);
    let id = std::env::var("FLOW_STT_MODEL").unwrap_or_else(|_| DEFAULT_STT_MODEL.to_string());

    let mut reader = hound::WavReader::open(path)?;
    let sample_rate = reader.spec().sample_rate;
    let audio: Vec<f32> =
        reader.samples::<i16>().map(|s| s.map(|s| s as f32 / 32768.0)).collect::<Result<_, _>>()?;

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
