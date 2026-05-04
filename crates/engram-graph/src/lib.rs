//! Knowledge graph helpers.
//!
//! - `extract`: deterministic entity extraction (no LLM, regex on capitalization)
//! - `facts`: LLM-based (subject, predicate, object) extraction for
//!   contradiction detection at write time
//! - `expand`: bounded query-time graph traversal

pub mod expand;
pub mod extract;
pub mod facts;

pub use extract::extract_entities;
pub use facts::{extract_facts, normalize, ExtractedFact, FactExtractionError};
