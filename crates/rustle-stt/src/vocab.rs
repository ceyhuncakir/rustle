//! The export's SentencePiece vocabulary, and turning token ids back into text.
//!
//! `vocab.txt` has one `piece id` per line. Pieces use `▁` (U+2581) as the
//! word-start marker, which onnx-asr replaces with a space before joining and
//! then tidies with `\A\s|\s\B|(\s)\b` -> `" " if group(1) else ""`: a
//! leading space goes, a space before a non-word character (punctuation,
//! another space, the end) goes, and a space before a word character stays.
//! [`Vocab::text`] does the same without a regex.

use std::path::Path;

use anyhow::{bail, Context};

pub struct Vocab {
    /// Indexed by token id, with `▁` already turned into a space.
    pieces: Vec<String>,
    blank: usize,
}

impl Vocab {
    pub fn load(path: &Path) -> anyhow::Result<Vocab> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> anyhow::Result<Vocab> {
        let mut entries = Vec::new();
        for (n, line) in text.lines().enumerate().filter(|(_, line)| !line.is_empty()) {
            let (piece, id) =
                line.rsplit_once(' ').with_context(|| format!("vocab line {} has no id: {line:?}", n + 1))?;
            let id: usize =
                id.parse().with_context(|| format!("vocab line {} has a bad id: {line:?}", n + 1))?;
            entries.push((id, piece.replace('\u{2581}', " ")));
        }
        // The joint's logits are indexed by id, so ids must be exactly 0..n.
        entries.sort_unstable_by_key(|(id, _)| *id);
        if let Some((slot, (id, _))) = entries.iter().enumerate().find(|(slot, (id, _))| id != slot) {
            bail!("vocab ids must be 0..{} without gaps or repeats: id {id} in slot {slot}", entries.len());
        }
        let pieces: Vec<String> = entries.into_iter().map(|(_, piece)| piece).collect();
        let blank = pieces.iter().position(|p| p == "<blk>").context("vocab has no <blk> token")?;
        Ok(Vocab { pieces, blank })
    }

    /// Number of tokens, which is also where the duration logits start in
    /// the joint's output.
    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn blank(&self) -> usize {
        self.blank
    }

    pub fn piece(&self, id: usize) -> Option<&str> {
        self.pieces.get(id).map(String::as_str)
    }

    /// The text for a token sequence, tidied like onnx-asr but not trimmed.
    pub fn text(&self, ids: &[u32]) -> anyhow::Result<String> {
        let mut joined = String::new();
        for &id in ids {
            joined.push_str(
                self.piece(id as usize)
                    .with_context(|| format!("token id {id} is outside the vocabulary"))?,
            );
        }
        Ok(collapse_spaces(&joined))
    }
}

/// `re.sub(r"\A\s|\s\B|(\s)\b", lambda m: " " if m.group(1) else "", s)`.
///
/// Every alternative consumes exactly one whitespace character, so each one
/// is decided on its own: dropped at the start of the string, kept as a plain
/// space when a word character follows, dropped otherwise.
fn collapse_spaces(joined: &str) -> String {
    let mut out = String::with_capacity(joined.len());
    let mut chars = joined.chars().peekable();
    let mut at_start = true;
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if !at_start && chars.peek().is_some_and(|next| is_word(*next)) {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
        at_start = false;
    }
    out
}

/// Python's `\w` for `str` patterns: alphanumerics plus underscore.
fn is_word(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pieces_and_blank() {
        let vocab = Vocab::parse("<unk> 0\n\u{2581}hello 1\n, 2\n\u{2581}world 3\n<blk> 4\n").unwrap();
        assert_eq!(vocab.len(), 5);
        assert_eq!(vocab.blank(), 4);
        assert_eq!(vocab.piece(1), Some(" hello"));
        assert_eq!(vocab.text(&[1, 2, 3]).unwrap(), "hello, world");
    }

    #[test]
    fn rejects_gaps_and_duplicates() {
        assert!(Vocab::parse("a 0\nb 3\n<blk> 1\n").is_err());
        assert!(Vocab::parse("a 0\nb 0\n<blk> 1\n").is_err());
        assert!(Vocab::parse("a 0\nb 1\n").is_err());
    }

    #[test]
    fn collapses_like_onnx_asr() {
        assert_eq!(collapse_spaces(" hello world"), "hello world");
        assert_eq!(collapse_spaces(" hello , world ."), "hello, world.");
        assert_eq!(collapse_spaces(" a  b"), "a b");
        assert_eq!(collapse_spaces(" één  ,  twee"), "één, twee");
        assert_eq!(collapse_spaces(""), "");
        assert_eq!(collapse_spaces(" "), "");
        assert_eq!(collapse_spaces(" x_y 1"), "x_y 1");
    }
}
