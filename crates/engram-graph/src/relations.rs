//! Re-export shim. The relation registry lives in `engram_core::relations`
//! and nowhere else (interfaces.md R1): the v9→v10 migration keys ~14.5k
//! decision rows through `member_identity`, and `engram-core` cannot depend on
//! `engram-graph`, so a second table here would drift on its first edit — and
//! the arity table IS the polypharmacy fix.
//!
//! `engram_graph::relations` exists so callers in this crate and its
//! dependents keep one import path. It defines nothing. A change to the
//! registry's CONTENT is fact-semantics' to make, as a patch against
//! `engram-core`, never as a rival table here.

pub use engram_core::relations::*;
