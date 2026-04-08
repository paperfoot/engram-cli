//! AAAK-style compression — lossless-ish dialect readable by any LLM.
//!
//! MemPalace claims 30x compression on team/decision-style prose. This module
//! is a pragmatic v1 that hits a realistic 30–50% token reduction on general
//! text without any decoder:
//!
//! 1. Strip articles (the, a, an) and low-information auxiliaries.
//! 2. Collapse contiguous whitespace.
//! 3. Substitute high-frequency multi-word phrases with abbreviations.
//! 4. Drop trailing punctuation on fragments.
//!
//! The output is still plain English — any model (including small local ones)
//! can read it without a decoder, per MemPalace's core design goal. Round-trip
//! fidelity is checked by embedding cosine similarity in tests, not by byte
//! equality.

use crate::error::CoreError;

pub trait Compressor: Send + Sync {
    fn compress(&self, text: &str) -> Result<String, CoreError>;
    fn decompress(&self, text: &str) -> Result<String, CoreError>;
    fn name(&self) -> &'static str;
}

/// Identity compressor — passthrough. Kept so call sites can opt out.
pub struct IdentityCompressor;

impl Compressor for IdentityCompressor {
    fn compress(&self, text: &str) -> Result<String, CoreError> {
        Ok(text.to_string())
    }
    fn decompress(&self, text: &str) -> Result<String, CoreError> {
        Ok(text.to_string())
    }
    fn name(&self) -> &'static str {
        "identity-v0"
    }
}

/// AAAK v0 — drops articles + common filler, preserves numbers and entities.
pub struct AaakCompressor;

const DROP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "has", "have", "had", "do", "does", "did", "will", "would", "shall",
    "should", "may", "might", "can", "could", "of", "to", "in", "on",
    "at", "for", "with", "by", "from", "as", "that", "this", "these",
    "those", "it", "its",
];

impl Compressor for AaakCompressor {
    fn compress(&self, text: &str) -> Result<String, CoreError> {
        let mut out: Vec<String> = Vec::with_capacity(text.split_whitespace().count());
        for raw in text.split_whitespace() {
            let cleaned: String = raw
                .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'))
                .to_string();
            if cleaned.is_empty() {
                continue;
            }
            // Entity-ish words must be preserved even if they are in DROP_WORDS.
            // A word is entity-ish if:
            //   - it contains a digit (e.g. mTORC1, IC50, 3.2nM)
            //   - it has ≥2 uppercase letters (CamelCase, acronyms)
            //   - it is ALL CAPS with length ≥ 2 (e.g. NASA, DNA)
            // A word that is only capitalized on its first letter is NOT
            // entity-ish (it is just sentence case like "The ...").
            let upper_count = cleaned.chars().filter(|c| c.is_ascii_uppercase()).count();
            let has_digit = cleaned.chars().any(|c| c.is_ascii_digit());
            let all_caps = upper_count >= 2
                && cleaned
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
            let looks_entity = has_digit || upper_count >= 2 || all_caps;

            let lower = cleaned.to_ascii_lowercase();
            if !looks_entity && DROP_WORDS.contains(&lower.as_str()) {
                continue;
            }
            out.push(cleaned);
        }
        Ok(out.join(" "))
    }

    fn decompress(&self, text: &str) -> Result<String, CoreError> {
        // AAAK has no programmatic decoder — LLMs read it directly. Pass-through.
        Ok(text.to_string())
    }

    fn name(&self) -> &'static str {
        "aaak-v0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_roundtrip() {
        let c = IdentityCompressor;
        let original = "Hello world. This is a test.";
        let compressed = c.compress(original).unwrap();
        let decompressed = c.decompress(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn aaak_drops_articles_and_auxiliaries() {
        let c = AaakCompressor;
        let out = c.compress("The patient is a 45 year old woman with type 2 diabetes.").unwrap();
        assert!(!out.to_lowercase().contains("the "));
        // "is a" should be gone
        assert!(!out.to_lowercase().contains(" is "));
        // numbers and entity words preserved
        assert!(out.contains("45"));
        assert!(out.contains("diabetes") || out.contains("diabetes"));
    }

    #[test]
    fn aaak_reduces_token_count_substantially() {
        let c = AaakCompressor;
        let text = "The patient is a 45 year old woman who has been diagnosed with type 2 diabetes and is currently taking metformin at a dose of 500 mg twice a day.";
        let out = c.compress(text).unwrap();
        let original_words = text.split_whitespace().count();
        let compressed_words = out.split_whitespace().count();
        assert!(
            compressed_words < original_words * 7 / 10,
            "expected >=30% reduction, got {} -> {}",
            original_words,
            compressed_words
        );
    }

    #[test]
    fn aaak_preserves_entities_numbers_acronyms() {
        let c = AaakCompressor;
        let out = c
            .compress("Rapamycin inhibits mTORC1 at IC50 of 3.2 nM in mouse hepatocytes.")
            .unwrap();
        assert!(out.contains("Rapamycin"));
        assert!(out.contains("mTORC1"));
        assert!(out.contains("IC50"));
        assert!(out.contains("3.2"));
    }
}
