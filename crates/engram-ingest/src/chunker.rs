//! Section-aware chunking. v0 = naive paragraph splitter; autoresearch will
//! evolve this into structure-preserving chunking.

#[derive(Debug, Clone)]
pub struct PendingChunk {
    pub text: String,
    pub position: u32,
    pub section: Option<String>,
}

/// Naive baseline: split on blank lines, drop empty/whitespace-only chunks.
pub fn naive_split(text: &str) -> Vec<PendingChunk> {
    let mut out = Vec::new();
    let mut pos = 0u32;
    for raw in text.split("\n\n") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(PendingChunk {
            text: trimmed.to_string(),
            position: pos,
            section: None,
        });
        pos += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_split_basic() {
        let chunks = naive_split("First paragraph.\n\nSecond paragraph.\n\n\nThird.");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text, "First paragraph.");
        assert_eq!(chunks[2].position, 2);
    }
}
