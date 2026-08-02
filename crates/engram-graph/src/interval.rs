//! Re-export shim. The interval algebra and the interval-closing safety gate
//! live in `engram_core::interval` (interfaces.md R1): the migration applies
//! `close_at`'s clamp in SQL and `engram-storage` cannot depend on
//! `engram-graph`, so one implementation is not a preference.
//!
//! This module defines nothing. `semantics.rs` owns what `may_close` MEANS —
//! which authorities may close what, and where the gate sits in the decision
//! procedure.

pub use engram_core::interval::*;
