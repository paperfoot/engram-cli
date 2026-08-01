//! Interval algebra for bi-temporal facts, and the interval-closing safety
//! gate.
//!
//! A fact is an observation about a state interval: `[valid_from, valid_until)`,
//! open when `valid_until` is NULL. New information EXTENDS or CLOSES
//! intervals; it never deletes rows. Contradiction shrinks to its real
//! definition — temporally incompatible intervals on a single-valued
//! relation, or a same-member overlap disagreement — instead of "any two
//! values on one predicate".
//!
//! Home is engram-core (interfaces.md v2, ruling R1/CM-3): the v9→v10
//! migration applies `close_at`'s clamp in SQL and engram-storage cannot
//! depend on engram-graph; one definition, one clamp. fact-semantics owns
//! the SEMANTICS here; this crate only houses them.

use crate::relations::{Mutability, RelationSpec};
use crate::types::Fact;
use chrono::{DateTime, Utc};

/// A fact's validity window. `until: None` means the interval is still open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub from: DateTime<Utc>,
    pub until: Option<DateTime<Utc>>,
}

impl Interval {
    /// The window a stored fact asserts. `valid_from` falls back to
    /// `created_at` (post-v10 it is never NULL, but a v9 store read by a v10
    /// binary must not panic); `superseded_at` closes an interval that has
    /// `superseded_by` set but no `valid_until` (pre-v6 rows).
    pub fn of(f: &Fact) -> Interval {
        let from = f.valid_from.unwrap_or(f.created_at);
        let until = f.valid_until.or(if f.superseded_by.is_some() {
            f.superseded_at
        } else {
            None
        });
        Interval { from, until }
    }

    pub fn is_open(&self) -> bool {
        self.until.is_none()
    }

    /// `until == Some(from)`: an instant, not a span.
    pub fn is_point(&self) -> bool {
        self.until == Some(self.from)
    }

    /// Half-open `[from, until)` overlap. A point interval is EMPTY and
    /// overlaps nothing — including itself. That is the wanted answer for
    /// "does this instant collide with a state interval"; "are these two
    /// facts about the same instant" is settled by the classifier's
    /// immutable-disagreement and duplicate rules before any interval math
    /// runs (every `Instant` relation in registry v1 is also `Immutable`).
    pub fn overlaps(&self, other: &Interval) -> bool {
        if self.is_point() || other.is_point() {
            return false;
        }
        let self_ends_after_other_starts = match self.until {
            None => true,
            Some(u) => u > other.from,
        };
        let other_ends_after_self_starts = match other.until {
            None => true,
            Some(u) => u > self.from,
        };
        self_ends_after_other_starts && other_ends_after_self_starts
    }

    /// `self.until <= other.from` — self closed before other began.
    pub fn precedes(&self, other: &Interval) -> bool {
        match self.until {
            Some(u) => u <= other.from,
            None => false,
        }
    }
}

/// True when the stored row still asserts something: not superseded, not
/// closed. This predicate REPLACES the bare `superseded_by IS NULL` test at
/// every "active facts" site — a fact closed with `reason: stopped` has
/// `superseded_by` NULL and must still not read as current (the named wave-3
/// sweep, coherence CD-2).
pub fn is_open_fact(f: &Fact) -> bool {
    f.valid_until.is_none() && f.superseded_by.is_none()
}

/// Why an interval is being closed. Persisted in `facts.close_reason` and in
/// the `fact.interval_closed` event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// A newer value on a single-valued relation replaced it.
    Superseded,
    /// The source text says the state ended ("stopped taking X").
    Stopped,
    /// A person said the value was wrong (not that it ended).
    Corrected,
    /// The memory it came from was retracted.
    RetractedSource,
}

impl CloseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            CloseReason::Superseded => "superseded",
            CloseReason::Stopped => "stopped",
            CloseReason::Corrected => "corrected",
            CloseReason::RetractedSource => "retracted_source",
        }
    }
}

/// WHO is entitled to close an interval. The safety gate's only input beside
/// the relation spec — the gate does not care how confident a heuristic was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseAuthority {
    /// The source text itself says the state ended.
    ExplicitEnd,
    /// A human or the calling agent adjudicated this exact pair.
    Caller { actor: String },
    /// A prior CALLER decision on the same semantic key, replayed.
    ReplayedCaller { decided_at: DateTime<Utc> },
    /// A later interval on a single-valued relation implies this one ended.
    SuccessorInterval,
    /// Recency, confidence gap, ingestion order, LLM adjudication.
    Heuristic { rule: &'static str },
}

impl CloseAuthority {
    /// The value persisted in `facts.close_authority` and the event payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            CloseAuthority::ExplicitEnd => "explicit_end",
            CloseAuthority::Caller { .. } => "caller",
            CloseAuthority::ReplayedCaller { .. } => "replayed_caller",
            CloseAuthority::SuccessorInterval => "successor_interval",
            CloseAuthority::Heuristic { .. } => "heuristic",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseVerdict {
    Allow,
    Refuse { reason: &'static str },
}

/// THE interval-closing safety gate. One function, called by every path that
/// can close an interval: the write path, `facts triage`, `facts resolve`,
/// and the compiler.
///
/// A safety relation may be closed only by evidence a person supplied: an
/// explicit end in the text, a caller decision, or a replayed caller
/// decision. `SuccessorInterval` and `Heuristic` are refused — a second
/// medication is not evidence that the first one stopped, and neither is a
/// newer timestamp. An immutable relation additionally refuses
/// `SuccessorInterval`: nobody acquires a second birthday.
pub fn may_close(spec: &RelationSpec, authority: &CloseAuthority) -> CloseVerdict {
    if spec.is_safety() {
        return match authority {
            CloseAuthority::ExplicitEnd
            | CloseAuthority::Caller { .. }
            | CloseAuthority::ReplayedCaller { .. } => CloseVerdict::Allow,
            CloseAuthority::SuccessorInterval => CloseVerdict::Refuse {
                reason: "safety relation: a successor value is not evidence the prior one ended",
            },
            CloseAuthority::Heuristic { .. } => CloseVerdict::Refuse {
                reason: "safety relation: heuristics never close intervals",
            },
        };
    }
    if spec.mutability == Mutability::Immutable
        && matches!(authority, CloseAuthority::SuccessorInterval)
    {
        return CloseVerdict::Refuse {
            reason: "immutable relation: a successor value cannot end it",
        };
    }
    CloseVerdict::Allow
}

/// The instant an interval closes when superseded by a successor: the
/// successor's `valid_from`, clamped so a close can never precede its own
/// `valid_from` (a negative-length interval is unreadable by every consumer).
/// The v9→v10 migration applies the SAME clamp in SQL (`MAX`), and neither
/// copy is advisory.
pub fn close_at(old: &Interval, successor_from: DateTime<Utc>) -> DateTime<Utc> {
    old.from.max(successor_from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FactType;
    use uuid::Uuid;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    /// Only the temporal fields matter to this module; the rest is filler.
    fn fact(valid_from: Option<&str>, valid_until: Option<&str>) -> Fact {
        Fact {
            id: Uuid::new_v4(),
            source_memory_id: Uuid::new_v4(),
            subject: "Ana".into(),
            subject_norm: "ana".into(),
            predicate: "takes_medication".into(),
            object: "metformin".into(),
            object_norm: "metformin".into(),
            confidence: 1.0,
            created_at: ts("2026-01-01T00:00:00Z"),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::Assertion,
            valid_from: valid_from.map(ts),
            valid_until: valid_until.map(ts),
            resolution: None,
            source_kind: Default::default(),
            confirmations: 0,
        }
    }

    #[test]
    fn of_falls_back_to_created_at_and_superseded_at() {
        let f = fact(None, None);
        assert_eq!(Interval::of(&f).from, f.created_at);

        let mut g = fact(Some("2026-02-01T00:00:00Z"), None);
        g.superseded_by = Some(Uuid::new_v4());
        g.superseded_at = Some(ts("2026-03-01T00:00:00Z"));
        assert_eq!(Interval::of(&g).until, Some(ts("2026-03-01T00:00:00Z")));
    }

    #[test]
    fn open_intervals_overlap_each_other() {
        let a = Interval {
            from: ts("2026-01-01T00:00:00Z"),
            until: None,
        };
        let b = Interval {
            from: ts("2026-06-01T00:00:00Z"),
            until: None,
        };
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
    }

    #[test]
    fn disjoint_closed_intervals_do_not_overlap_and_precede() {
        let a = Interval {
            from: ts("2026-01-01T00:00:00Z"),
            until: Some(ts("2026-02-01T00:00:00Z")),
        };
        let b = Interval {
            from: ts("2026-02-01T00:00:00Z"),
            until: None,
        };
        assert!(
            !a.overlaps(&b),
            "half-open: [Jan,Feb) then [Feb,∞) touch, not overlap"
        );
        assert!(a.precedes(&b));
    }

    #[test]
    fn points_overlap_nothing_including_themselves() {
        let p = Interval {
            from: ts("2026-01-01T00:00:00Z"),
            until: Some(ts("2026-01-01T00:00:00Z")),
        };
        let span = Interval {
            from: ts("2025-01-01T00:00:00Z"),
            until: None,
        };
        assert!(p.is_point());
        assert!(!p.overlaps(&span));
        assert!(!span.overlaps(&p));
        assert!(!p.overlaps(&p));
    }

    #[test]
    fn stopped_fact_is_not_open_even_without_supersession() {
        let mut f = fact(Some("2026-01-01T00:00:00Z"), Some("2026-05-01T00:00:00Z"));
        f.superseded_by = None;
        assert!(
            !is_open_fact(&f),
            "a closed interval must not read as current"
        );
        assert!(is_open_fact(&fact(Some("2026-01-01T00:00:00Z"), None)));
    }

    #[test]
    fn safety_gate_refuses_machines_and_admits_people() {
        let med = crate::relations::relation_spec("takes_medication");
        assert_eq!(
            may_close(&med, &CloseAuthority::ExplicitEnd),
            CloseVerdict::Allow
        );
        assert_eq!(
            may_close(
                &med,
                &CloseAuthority::Caller {
                    actor: "cli:default".into()
                }
            ),
            CloseVerdict::Allow
        );
        assert!(matches!(
            may_close(&med, &CloseAuthority::SuccessorInterval),
            CloseVerdict::Refuse { .. }
        ));
        assert!(matches!(
            may_close(&med, &CloseAuthority::Heuristic { rule: "recency" }),
            CloseVerdict::Refuse { .. }
        ));
    }

    #[test]
    fn immutable_refuses_successor_but_not_correction() {
        let bday = crate::relations::relation_spec("birthday");
        assert!(matches!(
            may_close(&bday, &CloseAuthority::SuccessorInterval),
            CloseVerdict::Refuse { .. }
        ));
        assert_eq!(
            may_close(
                &bday,
                &CloseAuthority::Caller {
                    actor: "cli:default".into()
                }
            ),
            CloseVerdict::Allow,
            "a person may correct an immutable value"
        );
        let city = crate::relations::relation_spec("lives_in");
        assert_eq!(
            may_close(&city, &CloseAuthority::SuccessorInterval),
            CloseVerdict::Allow
        );
    }

    #[test]
    fn close_at_clamps_to_interval_start() {
        let old = Interval {
            from: ts("2026-03-01T00:00:00Z"),
            until: None,
        };
        assert_eq!(
            close_at(&old, ts("2026-02-01T00:00:00Z")),
            old.from,
            "never before from"
        );
        assert_eq!(
            close_at(&old, ts("2026-04-01T00:00:00Z")),
            ts("2026-04-01T00:00:00Z")
        );
    }
}
