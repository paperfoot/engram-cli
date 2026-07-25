//! Temporal validity windows — facts can be valid for a date range.
//!

use crate::types::Memory;
use chrono::{DateTime, NaiveDate, Utc};

/// Parse a moment supplied by a human, an agent, or an extractor: RFC3339
/// (`2026-03-01T09:30:00Z`) or a bare ISO date (`2026-03-01`), which is read
/// as midnight UTC. Returns `None` for anything else — the caller decides
/// whether that is an error (`--event-time`) or a shrug (an LLM guessing
/// "next spring").
///
/// One parser for the whole workspace so `--event-time`, `valid_from`, and
/// date-valued fact objects can never disagree about what a date means.
pub fn parse_moment(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|ndt| DateTime::from_naive_utc_and_offset(ndt, Utc))
}

/// Returns true if the memory is valid as of the given moment.
pub fn is_valid_at(memory: &Memory, at: DateTime<Utc>) -> bool {
    if let Some(start) = memory.valid_from {
        if at < start {
            return false;
        }
    }
    if let Some(end) = memory.valid_until {
        if at > end {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemorySource};
    use chrono::TimeZone;
    use uuid::Uuid;

    fn fact() -> Memory {
        Memory {
            id: Uuid::new_v4(),
            content: "x".into(),
            created_at: Utc::now(),
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Manual,
            diary: "default".into(),
            valid_from: None,
            valid_until: None,
            tags: vec![],
        }
    }

    #[test]
    fn unconstrained_fact_is_always_valid() {
        let m = fact();
        assert!(is_valid_at(&m, Utc::now()));
    }

    #[test]
    fn fact_outside_window_is_invalid() {
        let mut m = fact();
        m.valid_from = Some(Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());
        m.valid_until = Some(Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap());
        let probe = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        assert!(!is_valid_at(&m, probe));
    }

    #[test]
    fn iso_date_is_midnight_utc() {
        let d = parse_moment("1990-04-12").unwrap();
        assert_eq!(d.to_rfc3339(), "1990-04-12T00:00:00+00:00");
    }

    #[test]
    fn rfc3339_keeps_its_instant_across_offsets() {
        let d = parse_moment("2026-03-01T09:30:00+02:00").unwrap();
        assert_eq!(d.to_rfc3339(), "2026-03-01T07:30:00+00:00");
    }

    #[test]
    fn unparseable_moments_are_none() {
        for raw in ["", "   ", "next spring", "01/03/2026", "2026-13-01"] {
            assert!(parse_moment(raw).is_none(), "{raw} should not parse");
        }
    }
}
