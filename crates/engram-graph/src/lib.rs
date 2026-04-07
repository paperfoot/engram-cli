//! Knowledge graph helpers.
//!
//! v0 = deterministic edges only (citation, containment, co-occurrence,
//! synonym). LLM-extracted triples are optional and added in Phase 4.
//!
//! Used as query-time expansion from top-N candidates, NOT as the primary
//! retriever.

pub mod expand;
