//! Knowledge graph helpers.
//!
//! - `gazetteer`: places, creeds and contact-label words that are never people
//! - `hygiene`: entity-surface filters (contractions, possessives, stop-words)
//! - `extract`: deterministic entity extraction (no LLM, regex on capitalization)
//! - `people`: person typing + typed person↔person relationship edges
//! - `facts`: LLM-based (subject, predicate, object) extraction for
//!   contradiction detection at write time
//! - `expand`: bounded query-time graph traversal

pub mod expand;
pub mod extract;
pub mod facts;
pub mod gate;
pub mod gazetteer;
pub mod hygiene;
pub mod people;

pub use extract::extract_entities;
pub use facts::{extract_facts, normalize, ExtractedFact, FactExtractionError};
pub use hygiene::{canonicalize_surface, Canonical};
pub use people::{analyze, Extraction, Mention, PersonEdge};
