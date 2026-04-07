//! AAAK-style compression — lossless dialect readable by any LLM.
//!
//! v0 baseline: identity (passthrough). The autoresearch loop will replace
//! this implementation with a real compression dialect, measured against
//! round-trip fidelity tests in the LongMemEval harness.
//!
//! The point of starting with an identity pass-through is so the rest of the
//! system can wire through `compress`/`decompress` calls today and we can
//! optimize the implementation later without changing call sites.

use crate::error::CoreError;

pub trait Compressor: Send + Sync {
    fn compress(&self, text: &str) -> Result<String, CoreError>;
    fn decompress(&self, text: &str) -> Result<String, CoreError>;
    fn name(&self) -> &'static str;
}

/// Identity compressor — does nothing. Replaced by autoresearch.
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
}
