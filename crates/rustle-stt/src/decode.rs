//! The TDT greedy loop, a faithful port of onnx-asr's
//! `_AsrWithTransducerDecoding._decoding` (asr.py) plus `NemoConformerTdt._decode`
//! (models/nemo.py).
//!
//! Per encoder frame `t` the joint network is run with the previously emitted
//! token (blank before the first) and the prediction network's LSTM states.
//! Its output is `vocab_size` token logits followed by the duration logits;
//! `argmax` of each gives the token and how many frames to skip. A non-blank
//! token is emitted and the states advance; then `t` moves by the duration,
//! or by one on a blank, or by one once ten tokens came out of a single frame.

use anyhow::{bail, Context};

/// onnx-asr's default `max_tokens_per_step` (config.json does not override it
/// for the Parakeet exports).
pub const MAX_TOKENS_PER_STEP: usize = 10;

/// The prediction network's LSTM states: `input_states_1` / `input_states_2`
/// of decoder_joint, each `[layers, 1, hidden]` flattened.
#[derive(Clone, Debug, PartialEq)]
pub struct DecoderState {
    pub h: Vec<f32>,
    pub c: Vec<f32>,
}

impl DecoderState {
    pub fn zeros(len: usize) -> DecoderState {
        DecoderState { h: vec![0.0; len], c: vec![0.0; len] }
    }
}

/// One joint evaluation: `vocab_size` token logits, then the duration logits.
pub struct JointOutput {
    pub logits: Vec<f32>,
    pub state: DecoderState,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub tokens: Vec<u32>,
    /// Encoder frame each token was emitted at (8x subsampled, 80 ms each).
    pub frames: Vec<usize>,
}

/// Run the loop over `frames` encoder frames.
///
/// `joint(t, prev_token, state)` evaluates decoder_joint for frame `t`; it is
/// a closure so the parity tests can stand in a fake for the session.
pub fn greedy_tdt<F>(
    frames: usize,
    vocab_size: usize,
    blank: usize,
    initial: DecoderState,
    mut joint: F,
) -> anyhow::Result<Decoded>
where
    F: FnMut(usize, usize, &DecoderState) -> anyhow::Result<JointOutput>,
{
    let mut decoded = Decoded::default();
    let mut state = initial;
    let mut prev = blank;
    let mut t = 0;
    let mut emitted = 0;

    while t < frames {
        let out = joint(t, prev, &state).with_context(|| format!("joint network at frame {t}"))?;
        if out.logits.len() <= vocab_size {
            bail!(
                "joint output has {} logits, expected more than the {vocab_size} vocabulary entries (TDT durations)",
                out.logits.len()
            );
        }
        let (tokens, durations) = out.logits.split_at(vocab_size);
        let token = argmax(tokens).context("empty token logits")?;
        let step = argmax(durations).context("empty duration logits")?;

        if token != blank {
            state = out.state;
            prev = token;
            decoded.tokens.push(token as u32);
            decoded.frames.push(t);
            emitted += 1;
        }

        if step > 0 {
            t += step;
            emitted = 0;
        } else if token == blank || emitted == MAX_TOKENS_PER_STEP {
            t += 1;
            emitted = 0;
        }
    }

    Ok(decoded)
}

/// First index of the largest value, as `numpy.argmax`.
fn argmax(xs: &[f32]) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &x) in xs.iter().enumerate() {
        if best.is_none_or(|(_, b)| x > b) {
            best = Some((i, x));
        }
    }
    best.map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOCAB: usize = 4; // ids 0..3, blank = 3
    const BLANK: usize = 3;

    /// Logits that pick `token` with duration `step` (5 duration bins).
    fn pick(token: usize, step: usize) -> Vec<f32> {
        let mut v = vec![0.0; VOCAB + 5];
        v[token] = 1.0;
        v[VOCAB + step] = 1.0;
        v
    }

    fn out(token: usize, step: usize, tag: f32) -> JointOutput {
        JointOutput { logits: pick(token, step), state: DecoderState { h: vec![tag], c: vec![-tag] } }
    }

    #[test]
    fn skips_by_the_predicted_duration() {
        let mut calls = Vec::new();
        let decoded = greedy_tdt(6, VOCAB, BLANK, DecoderState::zeros(1), |t, prev, _| {
            calls.push((t, prev));
            Ok(match t {
                0 => out(1, 2, 1.0),     // emit 1, jump to t = 2
                2 => out(BLANK, 0, 9.0), // blank, t = 3
                3 => out(2, 3, 2.0),     // emit 2, jump to t = 6 = end
                _ => panic!("unexpected frame {t}"),
            })
        })
        .unwrap();
        assert_eq!(decoded.tokens, vec![1, 2]);
        assert_eq!(decoded.frames, vec![0, 3]);
        assert_eq!(calls, vec![(0, BLANK), (2, 1), (3, 1)]);
    }

    #[test]
    fn blank_advances_one_frame_and_keeps_state() {
        let mut states = Vec::new();
        let decoded = greedy_tdt(3, VOCAB, BLANK, DecoderState::zeros(1), |t, _, state| {
            states.push(state.clone());
            Ok(out(BLANK, 0, t as f32 + 1.0))
        })
        .unwrap();
        assert!(decoded.tokens.is_empty());
        assert_eq!(states.len(), 3);
        // A blank never advances the prediction network.
        assert!(states.iter().all(|s| *s == DecoderState::zeros(1)));
    }

    #[test]
    fn blank_with_duration_skips_without_emitting() {
        let mut calls = 0;
        let decoded = greedy_tdt(4, VOCAB, BLANK, DecoderState::zeros(1), |_, _, _| {
            calls += 1;
            Ok(out(BLANK, 4, 0.0))
        })
        .unwrap();
        assert!(decoded.tokens.is_empty());
        assert_eq!(calls, 1);
    }

    #[test]
    fn caps_tokens_per_frame_at_ten() {
        let mut calls = Vec::new();
        let decoded = greedy_tdt(2, VOCAB, BLANK, DecoderState::zeros(1), |t, prev, state| {
            calls.push((t, prev, state.h[0]));
            Ok(out(2, 0, state.h[0] + 1.0))
        })
        .unwrap();
        assert_eq!(decoded.tokens.len(), 2 * MAX_TOKENS_PER_STEP);
        assert_eq!(&decoded.frames[..10], &[0; 10]);
        assert_eq!(&decoded.frames[10..], &[1; 10]);
        // The state threads through every emission, and the previous token
        // is the last emitted one after the first call.
        assert_eq!(calls[0], (0, BLANK, 0.0));
        assert_eq!(calls[1], (0, 2, 1.0));
        assert_eq!(calls[10], (1, 2, 10.0));
        assert_eq!(calls.len(), 20);
    }

    #[test]
    fn duration_resets_the_per_frame_count() {
        // Nine tokens with duration 0, then one with duration 1: the count
        // resets, so the next frame gets a full ten again.
        let mut n = 0;
        let decoded = greedy_tdt(2, VOCAB, BLANK, DecoderState::zeros(1), |_, _, _| {
            n += 1;
            Ok(if n % 10 == 0 { out(1, 1, 0.0) } else { out(1, 0, 0.0) })
        })
        .unwrap();
        assert_eq!(decoded.tokens.len(), 20);
        assert_eq!(decoded.frames.iter().filter(|&&f| f == 0).count(), 10);
    }

    #[test]
    fn rejects_output_without_durations() {
        let err = greedy_tdt(1, VOCAB, BLANK, DecoderState::zeros(1), |_, _, _| {
            Ok(JointOutput { logits: vec![0.0; VOCAB], state: DecoderState::zeros(1) })
        })
        .unwrap_err();
        assert!(err.to_string().contains("durations"), "{err}");
    }

    #[test]
    fn argmax_takes_the_first_maximum() {
        assert_eq!(argmax(&[1.0, 3.0, 3.0, 2.0]), Some(1));
        assert_eq!(argmax(&[-1.0]), Some(0));
        assert_eq!(argmax(&[]), None);
    }
}
