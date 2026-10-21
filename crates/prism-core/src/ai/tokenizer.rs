//! CLIP byte-level BPE tokenizer (the scheme used by OpenAI CLIP and
//! TinyCLIP): lower-cased, regex pre-split, GPT-2 byte→unicode mapping,
//! BPE merges with an end-of-word marker `</w>`.

use crate::error::{Error, Result};
use regex::Regex;
use std::collections::HashMap;

pub const CONTEXT: usize = 77;
pub const SOT: i64 = 49406;
pub const EOT: i64 = 49407;

pub struct Tokenizer {
    vocab: HashMap<String, i64>,
    ranks: HashMap<(String, String), usize>,
    byte_map: [char; 256],
    pattern: Regex,
}

/// GPT-2's reversible byte → printable unicode mapping.
fn bytes_to_unicode() -> [char; 256] {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    let mut map = ['\0'; 256];
    for (b, c) in bs.into_iter().zip(cs) {
        map[b as usize] = char::from_u32(c).unwrap_or('?');
    }
    map
}

impl Tokenizer {
    /// `vocab_json`: token → id; `merges`: "a b" lines (first line is a header).
    pub fn new(vocab_json: &str, merges: &str) -> Result<Self> {
        let vocab: HashMap<String, i64> = serde_json::from_str(vocab_json)?;
        let ranks = merges
            .lines()
            .skip(1)
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .enumerate()
            .map(|(i, p)| (p, i))
            .collect();
        let pattern = Regex::new(
            r"(?i)<\|startoftext\|>|<\|endoftext\|>|'s|'t|'re|'ve|'m|'ll|'d|[\p{L}]+|[\p{N}]|[^\s\p{L}\p{N}]+",
        )
        .map_err(|e| Error::NotFound(format!("tokenizer regex: {e}")))?;
        Ok(Self { vocab, ranks, byte_map: bytes_to_unicode(), pattern })
    }

    fn bpe(&self, word: &str) -> Vec<String> {
        let mut parts: Vec<String> = word.chars().map(String::from).collect();
        if let Some(last) = parts.last_mut() {
            last.push_str("</w>");
        }
        loop {
            let best = parts
                .windows(2)
                .enumerate()
                .filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|r| (*r, i)))
                .min();
            let Some((_, i)) = best else { break };
            let merged = format!("{}{}", parts[i], parts[i + 1]);
            parts.splice(i..i + 2, [merged]);
        }
        parts
    }

    /// Token ids for `text` without start/end markers.
    pub fn tokens(&self, text: &str) -> Vec<i64> {
        let clean = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        let mut ids = Vec::new();
        for m in self.pattern.find_iter(&clean) {
            let mapped: String = m.as_str().bytes().map(|b| self.byte_map[b as usize]).collect();
            for piece in self.bpe(&mapped) {
                if let Some(id) = self.vocab.get(&piece) {
                    ids.push(*id);
                }
            }
        }
        ids
    }

    /// `[SOT, tokens…, EOT, EOT…]` padded/truncated to 77, plus the
    /// attention mask.
    pub fn encode(&self, text: &str) -> ([i64; CONTEXT], [i64; CONTEXT]) {
        let mut ids = [EOT; CONTEXT];
        let mut mask = [0i64; CONTEXT];
        ids[0] = SOT;
        let toks = self.tokens(text);
        let n = toks.len().min(CONTEXT - 2);
        ids[1..1 + n].copy_from_slice(&toks[..n]);
        ids[1 + n] = EOT;
        mask[..n + 2].iter_mut().for_each(|m| *m = 1);
        (ids, mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_map_is_a_bijection() {
        let m = bytes_to_unicode();
        let set: std::collections::HashSet<char> = m.iter().copied().collect();
        assert_eq!(set.len(), 256);
        assert_eq!(m[b'a' as usize], 'a');
        assert_eq!(m[b' ' as usize], 'Ġ');
    }

    #[test]
    fn bpe_merges_in_rank_order() {
        let vocab = r#"{"c</w>":1,"a":2,"t</w>":3,"at</w>":4,"cat</w>":5,"ca":6}"#;
        let merges = "#version: 0.2\na t</w>\nc at</w>\nc a\n";
        let t = Tokenizer::new(vocab, merges).unwrap();
        assert_eq!(t.tokens("CAT"), vec![5]);
        let (ids, mask) = t.encode("cat cat");
        assert_eq!(&ids[..4], &[SOT, 5, 5, EOT]);
        assert_eq!(ids[76], EOT);
        assert_eq!(mask.iter().sum::<i64>(), 4);
    }

    /// Against the real CLIP vocabulary when available
    /// (`PRISM_MODELS_DIR` pointing at a folder with vocab.json/merges.txt).
    #[test]
    fn real_vocab_matches_reference_ids() {
        let Ok(dir) = std::env::var("PRISM_MODELS_DIR") else { return };
        let dir = std::path::Path::new(&dir);
        let t = Tokenizer::new(
            &std::fs::read_to_string(dir.join("vocab.json")).unwrap(),
            &std::fs::read_to_string(dir.join("merges.txt")).unwrap(),
        )
        .unwrap();
        // Reference: openai/clip-vit-base-patch32 tokenizer.
        assert_eq!(t.tokens("a photo of a cat"), vec![320, 1125, 539, 320, 2368]);
    }
}
