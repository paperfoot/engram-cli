//! Journal event kinds — the vocabulary of the append-only source of truth.
//!
//! The v0.3 store mutated projections directly and hard-deleted derived
//! state: a content edit erased the memory's facts, its extraction marker,
//! and — via FK cascade — every open AND resolved adjudication row touching
//! them, silently. The journal inverts that: every mutation is an event,
//! events are never deleted, and the tables agents read become projections
//! rebuildable from this log. The enum below is the closed set of event
//! kinds for journal schema v1; payload schemas live in
//! docs/arch/interfaces.md §2 and are validated at append time by storage.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    MemoryWritten,
    MemoryRevised,
    MemoryRetracted,
    MemoryMetaChanged,
    DerivationRan,
    FactObserved,
    FactIntervalClosed,
    FactConfirmed,
    ConflictOpened,
    DecisionRecorded,
    DecisionReplayed,
    CompileRan,
    JobSubmitted,
    JobCompleted,
    JobFailed,
}

impl EventKind {
    /// Wire names are dotted `<entity>.<verb>` — stable, grep-able, and the
    /// journal's `kind` column value. Renaming one is a schema change.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::MemoryWritten => "memory.written",
            EventKind::MemoryRevised => "memory.revised",
            EventKind::MemoryRetracted => "memory.retracted",
            EventKind::MemoryMetaChanged => "memory.meta_changed",
            EventKind::DerivationRan => "derivation.ran",
            EventKind::FactObserved => "fact.observed",
            EventKind::FactIntervalClosed => "fact.interval_closed",
            EventKind::FactConfirmed => "fact.confirmed",
            EventKind::ConflictOpened => "conflict.opened",
            EventKind::DecisionRecorded => "decision.recorded",
            EventKind::DecisionReplayed => "decision.replayed",
            EventKind::CompileRan => "compile.ran",
            EventKind::JobSubmitted => "job.submitted",
            EventKind::JobCompleted => "job.completed",
            EventKind::JobFailed => "job.failed",
        }
    }

    pub fn parse(s: &str) -> Option<EventKind> {
        Some(match s {
            "memory.written" => EventKind::MemoryWritten,
            "memory.revised" => EventKind::MemoryRevised,
            "memory.retracted" => EventKind::MemoryRetracted,
            "memory.meta_changed" => EventKind::MemoryMetaChanged,
            "derivation.ran" => EventKind::DerivationRan,
            "fact.observed" => EventKind::FactObserved,
            "fact.interval_closed" => EventKind::FactIntervalClosed,
            "fact.confirmed" => EventKind::FactConfirmed,
            "conflict.opened" => EventKind::ConflictOpened,
            "decision.recorded" => EventKind::DecisionRecorded,
            "decision.replayed" => EventKind::DecisionReplayed,
            "compile.ran" => EventKind::CompileRan,
            "job.submitted" => EventKind::JobSubmitted,
            "job.completed" => EventKind::JobCompleted,
            "job.failed" => EventKind::JobFailed,
            _ => return None,
        })
    }

    /// Every kind, for exhaustive tests and the rebuild dispatcher.
    pub const ALL: [EventKind; 15] = [
        EventKind::MemoryWritten,
        EventKind::MemoryRevised,
        EventKind::MemoryRetracted,
        EventKind::MemoryMetaChanged,
        EventKind::DerivationRan,
        EventKind::FactObserved,
        EventKind::FactIntervalClosed,
        EventKind::FactConfirmed,
        EventKind::ConflictOpened,
        EventKind::DecisionRecorded,
        EventKind::DecisionReplayed,
        EventKind::CompileRan,
        EventKind::JobSubmitted,
        EventKind::JobCompleted,
        EventKind::JobFailed,
    ];
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_roundtrips_through_its_wire_name() {
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(k), "kind {k}");
        }
    }

    #[test]
    fn wire_names_are_unique() {
        let mut names: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), EventKind::ALL.len());
    }

    #[test]
    fn unknown_kind_is_none_not_panic() {
        assert_eq!(EventKind::parse("memory.exploded"), None);
    }
}
