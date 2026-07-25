//! Memory-note chunking — the pipeline behind `engram remember`.
//!
//! `remember` used to call [`crate::chunker::naive_split`] directly: split on
//! blank lines, embed whatever falls out, no merge and no size cap. Fed real
//! input (a pasted WhatsApp thread, a business doc with date headers, a long
//! unbroken note) that produced two failure modes at once:
//!
//! * **Degenerate chunks.** Every stray line — `Sep 16, 2025`, `Elias:`, `ok` —
//!   became its own embedded, retrievable chunk. Short chunks are nearly pure
//!   signal for the few tokens they hold, so they score at the top for any query
//!   touching those tokens and push the paragraph that actually answers the
//!   question below the fold. One live store accumulated 986 chunks under 40
//!   characters, and a recall for a person's birthday returned two bare dates
//!   above the memory holding the birthday.
//! * **Silent truncation.** With no cap, a long note went to the embedder in one
//!   piece and lost everything past its input window with no warning.
//!
//! The fix reuses the chunkers ingest already had — merge, cap, and the new
//! hard floor — behind one policy object:
//!
//! ```text
//! paragraphs -> merge to target -> cap at max -> floor at min
//! ```
//!
//! Paragraph split, not [`crate::chunker::section_aware_split`], on purpose:
//! the section-aware chunker lifts heading lines OUT of the chunk text into
//! metadata, and `chunks_fts` indexes only `content`. For a document that is
//! right — the breadcrumb is navigation. For a personal note the heading IS the
//! retrieval key ("Noemi" above three lines about Noemi), and moving it into an
//! unindexed column would make the note unfindable by the one word that matters.
//! Merging keeps that heading glued to the lines beneath it instead.

use crate::chunker::{
    enforce_max_chunk_chars, enforce_min_chunk_chars, merge_small_chunks, naive_split, PendingChunk,
};

/// A fragment shorter than this never stands alone as a retrievable chunk.
///
/// 60 characters is roughly a full short sentence. Below it you are looking at
/// a date stamp, a speaker label, a bare name or a sign-off — text with no
/// standalone meaning that nonetheless embeds to a high-similarity vector.
/// Above it a chunk carries enough context to deserve its own retrieval slot.
/// The floor applies per FRAGMENT, never to a whole memory: a 27-character
/// memory is a legitimate memory and is stored as one chunk.
pub const DEFAULT_MIN_CHUNK_CHARS: usize = 60;

/// Merge adjacent paragraphs until a chunk reaches this size. ~125 tokens:
/// enough context for the embedder to place the chunk meaningfully, small
/// enough that a recall hit still points at a specific passage.
pub const DEFAULT_TARGET_CHUNK_CHARS: usize = 500;

/// Hard ceiling on chunk size, matching paper/general ingest. Well under the
/// embedder's ~7,800-character input window, so a chunk produced by this
/// pipeline can never be truncated.
pub const DEFAULT_MAX_CHUNK_CHARS: usize = 1800;

/// The three knobs that decide chunk shape. Configured through
/// `chunking.{min,target,max}_chars` with `ENGRAM_CHUNK_{MIN,TARGET,MAX}_CHARS`
/// env overrides, resolved CLI-side (this crate stays config-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPolicy {
    pub min_chars: usize,
    pub target_chars: usize,
    pub max_chars: usize,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            min_chars: DEFAULT_MIN_CHUNK_CHARS,
            target_chars: DEFAULT_TARGET_CHUNK_CHARS,
            max_chars: DEFAULT_MAX_CHUNK_CHARS,
        }
    }
}

impl ChunkPolicy {
    /// Clamp a user-supplied policy into a coherent ordering. A config file or
    /// env var with `max < min` would otherwise make the floor and the ceiling
    /// fight forever, so nonsense is repaired rather than trusted.
    pub fn sanitized(self) -> Self {
        let min_chars = self.min_chars;
        let target_chars = self.target_chars.max(min_chars);
        let max_chars = self.max_chars.max(target_chars).max(min_chars).max(1);
        Self {
            min_chars,
            target_chars,
            max_chars,
        }
    }
}

/// Chunk a memory's content for storage and embedding.
///
/// Guarantees, for any input:
/// * no chunk exceeds `max_chars + min_chars` (the floor may reattach a small
///   tail to a full chunk — bounded, and far below the embedder's window);
/// * no chunk is shorter than `min_chars` unless it is the ONLY chunk;
/// * every character of the input survives into exactly one chunk.
pub fn chunk_memory(text: &str, policy: ChunkPolicy) -> Vec<PendingChunk> {
    let policy = policy.sanitized();
    let paragraphs = naive_split(text);
    let merged = merge_small_chunks(paragraphs, policy.target_chars);
    let capped = enforce_max_chunk_chars(merged, policy.max_chars);
    enforce_min_chunk_chars(capped, policy.min_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(|w| w.to_string()).collect()
    }

    /// Every word of the input must appear, in order, across the chunks.
    fn assert_lossless(input: &str, chunks: &[PendingChunk]) {
        let got: Vec<String> = chunks.iter().flat_map(|c| words(&c.text)).collect();
        assert_eq!(words(input), got, "chunking lost or reordered content");
    }

    #[test]
    fn date_fragment_never_becomes_a_standalone_chunk() {
        // The exact shape that poisoned the live store: a document whose
        // paragraphs are date headers followed by a line of prose.
        let mut doc = String::new();
        for day in 16..24 {
            doc.push_str(&format!("Sep {day}, 2025\n\n"));
            doc.push_str(&format!(
                "Met Noemi at the Belgrade summit on day {day} and she mentioned her birthday \
                 is on the 3rd of April, which is worth remembering because she throws a party \
                 every year at the family house outside the city.\n\n"
            ));
        }
        doc.push_str("Oct 2, 2025");
        let doc = doc.as_str();
        let chunks = chunk_memory(doc, ChunkPolicy::default());
        assert!(chunks.len() > 1, "a multi-entry doc should not collapse");
        for chunk in &chunks {
            assert!(
                chunk.text.len() >= DEFAULT_MIN_CHUNK_CHARS,
                "degenerate chunk survived: {:?}",
                chunk.text
            );
        }
        assert!(
            !chunks.iter().any(|c| c.text.trim() == "Sep 16, 2025"),
            "bare date became its own chunk"
        );
        // The leading date header rides along with the entry it introduces.
        assert!(chunks[0].text.contains("Sep 16, 2025"));
        assert!(chunks[0].text.contains("Noemi"));
        assert_lossless(doc, &chunks);
    }

    #[test]
    fn whatsapp_style_lines_merge_instead_of_fragmenting() {
        let thread = (0..40)
            .map(|i| format!("[12/03/2026, 09:0{}] Elias: ok\n\n", i % 10))
            .collect::<String>();
        let naive = naive_split(&thread);
        let chunks = chunk_memory(&thread, ChunkPolicy::default());
        assert_eq!(naive.len(), 40, "baseline: one degenerate chunk per line");
        assert!(chunks.len() < naive.len());
        assert!(chunks
            .iter()
            .all(|c| c.text.len() >= DEFAULT_MIN_CHUNK_CHARS));
        assert_lossless(&thread, &chunks);
    }

    #[test]
    fn short_memory_still_stores_as_one_chunk() {
        // 27 characters. Legitimate, and must not be merged away or dropped.
        let short = "Lira's birthday is 12 March";
        assert!(short.len() < DEFAULT_MIN_CHUNK_CHARS);
        let chunks = chunk_memory(short, ChunkPolicy::default());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, short);
        assert_eq!(chunks[0].position, 0);
    }

    #[test]
    fn multi_fact_blob_chunks_into_bounded_units() {
        let blob = (0..30)
            .map(|i| {
                format!(
                    "Contact {i}: works at Acme {i} in Berlin, introduced by Milos at the \
                     conference, interested in the biotech round.\n\n"
                )
            })
            .collect::<String>();
        let chunks = chunk_memory(&blob, ChunkPolicy::default());
        assert!(chunks.len() > 1, "a 3.5k-char blob should split");
        for chunk in &chunks {
            assert!(chunk.text.len() >= DEFAULT_MIN_CHUNK_CHARS);
            assert!(chunk.text.len() <= DEFAULT_MAX_CHUNK_CHARS + DEFAULT_MIN_CHUNK_CHARS);
        }
        assert_lossless(&blob, &chunks);
    }

    #[test]
    fn long_unbroken_note_is_chunked_not_truncated() {
        // No blank lines, no sentence boundaries: the exact input the sentence
        // packer refuses to split, and the one the embedder would truncate.
        let note = "word ".repeat(4_000);
        assert!(
            note.len() > 7_800,
            "fixture must exceed the embedder window"
        );
        let chunks = chunk_memory(&note, ChunkPolicy::default());
        assert!(chunks.len() > 10);
        for chunk in &chunks {
            assert!(
                chunk.text.len() <= DEFAULT_MAX_CHUNK_CHARS + DEFAULT_MIN_CHUNK_CHARS,
                "chunk over cap: {}",
                chunk.text.len()
            );
        }
        assert_lossless(&note, &chunks);
    }

    #[test]
    fn positions_are_contiguous_from_zero() {
        let text = "Sep 1\n\n".to_string() + &"Something meaningful happened today. ".repeat(200);
        let chunks = chunk_memory(&text, ChunkPolicy::default());
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.position, i as u32);
        }
    }

    #[test]
    fn policy_with_inverted_bounds_is_repaired() {
        let policy = ChunkPolicy {
            min_chars: 500,
            target_chars: 10,
            max_chars: 5,
        }
        .sanitized();
        assert_eq!(policy.min_chars, 500);
        assert!(policy.target_chars >= policy.min_chars);
        assert!(policy.max_chars >= policy.target_chars);
        // And it still terminates on real input.
        let chunks = chunk_memory(&"a b c ".repeat(500), policy);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn empty_and_whitespace_input_produce_no_chunks() {
        assert!(chunk_memory("", ChunkPolicy::default()).is_empty());
        assert!(chunk_memory("   \n\n  \n", ChunkPolicy::default()).is_empty());
    }
}
