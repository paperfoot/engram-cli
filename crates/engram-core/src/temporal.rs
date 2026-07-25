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
    parse_bound(s, DayBound::Start)
}

/// Which end of a bare `YYYY-MM-DD` a caller means.
///
/// A date is a whole day, not an instant, so which instant it denotes depends
/// on the side of the range it sits on. An inclusive window written
/// `--since 2026-06-01 --until 2026-06-01` must select all of June 1st, which
/// only works when the lower bound is the first instant of the day and the
/// upper bound the last. Same convention as the personal bench's
/// `window_bounds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayBound {
    /// `YYYY-MM-DD` -> 00:00:00.000000000 UTC.
    Start,
    /// `YYYY-MM-DD` -> 23:59:59.999999999 UTC.
    End,
}

/// `parse_moment`, but a bare date resolves to the requested end of the day.
///
/// RFC3339 input is an exact instant and is unaffected by `bound`.
pub fn parse_bound(s: &str, bound: DayBound) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let naive = match bound {
        DayBound::Start => date.and_hms_opt(0, 0, 0)?,
        DayBound::End => date.and_hms_nano_opt(23, 59, 59, 999_999_999)?,
    };
    Some(DateTime::from_naive_utc_and_offset(naive, Utc))
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

    #[test]
    fn a_bare_date_brackets_the_whole_utc_day() {
        let start = parse_bound("2026-06-01", DayBound::Start).unwrap();
        let end = parse_bound("2026-06-01", DayBound::End).unwrap();
        assert_eq!(start.to_rfc3339(), "2026-06-01T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-06-01T23:59:59.999999999+00:00");
        // A memory written at any instant on that day falls inside [start, end].
        let midday = parse_bound("2026-06-01T12:00:00Z", DayBound::End).unwrap();
        assert!(start <= midday && midday <= end);
        // And the next day does not.
        let next = parse_bound("2026-06-02T00:00:00Z", DayBound::Start).unwrap();
        assert!(next > end);
    }

    #[test]
    fn rfc3339_is_an_instant_regardless_of_which_bound_asked() {
        let raw = "2026-06-01T09:30:00Z";
        assert_eq!(
            parse_bound(raw, DayBound::Start),
            parse_bound(raw, DayBound::End)
        );
    }

    #[test]
    fn unparseable_bounds_are_none_at_both_ends() {
        for raw in ["", "   ", "yesterday", "2026-06-32", "06/01/2026"] {
            assert!(parse_bound(raw, DayBound::Start).is_none(), "{raw}");
            assert!(parse_bound(raw, DayBound::End).is_none(), "{raw}");
        }
    }
}
