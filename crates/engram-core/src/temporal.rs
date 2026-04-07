//! Temporal validity windows — facts can be valid for a date range.
//!
//! Inspired by MemPalace's temporal knowledge graph.

use crate::types::Memory;
use chrono::{DateTime, Utc};

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
}
