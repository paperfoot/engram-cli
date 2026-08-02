//! Procedural memory: the `lessons` projection and schema v11.
//!
//! A fact is a claim about the world. A lesson is a claim about THE WORK —
//! "when a caller resolves a takes_medication conflict with keep-both, do not
//! re-queue that pair after an edit". The store already remembered what is
//! true and had no place at all for what it had learned about answering, so
//! every agent rediscovered the same corrections from scratch.
//!
//! Two rules this file exists to hold:
//!
//! 1. **One lesson per (kb, trigger, lesson_norm), with a counter.** Without
//!    the unique index a nightly `engram reflect` re-proposes the same
//!    sentence forever and `engram lessons` becomes the 951-row backlog in a
//!    new table. `occurrences` rising is the honest record of "we learned this
//!    again".
//! 2. **Every mutation carries its timestamp in, never reads the clock here.**
//!    The projection row and its journal event must agree to the microsecond
//!    or `admin rebuild` stops being byte-identical — two `Utc::now()` calls
//!    in one write are two different instants.
//!
//! Migration v10→v11 is additive-only per D25: `CREATE TABLE IF NOT EXISTS`
//! plus two indexes, no backfill, no row rewrite, and therefore **no
//! `VACUUM INTO` backup** — the v10 backup exists because v10 rewrites
//! existing rows, and nothing here touches one.

use crate::error::StorageError;
use crate::sqlite::SqliteStore;
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use uuid::Uuid;

/// Schema v11. `memory_id` is deliberately NOT a foreign key: a lesson must
/// survive the retraction of the memory twin that made it recallable.
pub const SCHEMA_V11: &str = r#"
CREATE TABLE IF NOT EXISTS lessons (
    id            TEXT PRIMARY KEY,
    kb            TEXT NOT NULL DEFAULT 'default',
    diary         TEXT NOT NULL DEFAULT 'default',
    trigger       TEXT NOT NULL,
    lesson        TEXT NOT NULL,
    lesson_norm   TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'proposed',
    source_kind   TEXT NOT NULL DEFAULT 'agent_extracted',
    confidence    REAL NOT NULL DEFAULT 0.5,
    occurrences   INTEGER NOT NULL DEFAULT 1,
    evidence_json TEXT NOT NULL DEFAULT '[]',
    memory_id     TEXT,
    rule          TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    FOREIGN KEY(kb) REFERENCES knowledge_bases(name) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_lessons_dedup ON lessons(kb, trigger, lesson_norm);
CREATE INDEX IF NOT EXISTS idx_lessons_status ON lessons(kb, status, updated_at);
"#;

/// The lesson statuses, spelled once. `retired` is what `lessons retract`
/// writes: nothing is ever deleted, so "gone" is a status, not an absence.
pub const STATUS_PROPOSED: &str = "proposed";
pub const STATUS_CONFIRMED: &str = "confirmed";
pub const STATUS_RETIRED: &str = "retired";

#[derive(Debug, Clone, PartialEq)]
pub struct LessonRow {
    pub id: Uuid,
    pub kb: String,
    pub diary: String,
    pub trigger: String,
    pub lesson: String,
    /// Lowercased, whitespace-collapsed. The dedup key, so two agents phrasing
    /// the same lesson with different spacing get one row.
    pub lesson_norm: String,
    pub status: String,
    pub source_kind: String,
    pub confidence: f32,
    pub occurrences: i64,
    pub evidence: Vec<Uuid>,
    pub memory_id: Option<Uuid>,
    /// The deterministic proposer that made it, if a machine did.
    pub rule: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The dedup key's normalizer. One definition, because a second one would
/// silently split the index.
pub fn normalize_lesson(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What an upsert did. `Merged` is not a failure — it is the counter rising.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LessonUpsert {
    Inserted,
    Merged,
}

const COLUMNS: &str = "id, kb, diary, trigger, lesson, lesson_norm, status, source_kind, \
                       confidence, occurrences, evidence_json, memory_id, rule, created_at, \
                       updated_at";

fn parse_row(r: &rusqlite::Row) -> rusqlite::Result<LessonRow> {
    let id: String = r.get(0)?;
    let evidence_json: String = r.get(10)?;
    let memory_id: Option<String> = r.get(11)?;
    let created_at: String = r.get(13)?;
    let updated_at: String = r.get(14)?;
    let parse_ts = |s: &str| {
        DateTime::parse_from_rfc3339(s)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or(DateTime::UNIX_EPOCH)
    };
    Ok(LessonRow {
        id: Uuid::parse_str(&id).unwrap_or(Uuid::nil()),
        kb: r.get(1)?,
        diary: r.get(2)?,
        trigger: r.get(3)?,
        lesson: r.get(4)?,
        lesson_norm: r.get(5)?,
        status: r.get(6)?,
        source_kind: r.get(7)?,
        confidence: r.get::<_, f64>(8)? as f32,
        occurrences: r.get(9)?,
        evidence: serde_json::from_str::<Vec<String>>(&evidence_json)
            .unwrap_or_default()
            .iter()
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
        memory_id: memory_id.and_then(|s| Uuid::parse_str(&s).ok()),
        rule: r.get(12)?,
        created_at: parse_ts(&created_at),
        updated_at: parse_ts(&updated_at),
    })
}

fn evidence_json(ids: &[Uuid]) -> String {
    json!(ids.iter().map(|i| i.to_string()).collect::<Vec<_>>()).to_string()
}

impl SqliteStore {
    /// v10 → v11. Additive-only (D25), idempotent, and safe on
    /// `open_in_memory()` for the same reason every other `CREATE TABLE` step
    /// is: it creates, it does not rewrite.
    pub(crate) fn migrate_v11(&self) -> Result<(), StorageError> {
        self.conn.execute_batch(SCHEMA_V11)?;
        Ok(())
    }

    /// Insert the lesson, or merge into the one that already says this.
    ///
    /// On merge: `occurrences` rises by one, `evidence` is the union, and
    /// `status`/`lesson` are LEFT ALONE — a lesson a human confirmed is never
    /// silently re-worded by a later machine pass. The returned row is the
    /// post-write state, which is what the event payload carries so replay can
    /// SET the counter rather than guess at it.
    pub fn upsert_lesson(
        &self,
        row: &LessonRow,
    ) -> Result<(LessonRow, LessonUpsert), StorageError> {
        self.with_tx(|s| {
            let existing: Option<LessonRow> = s
                .conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM lessons \
                         WHERE kb = ?1 AND trigger = ?2 AND lesson_norm = ?3"
                    ),
                    params![row.kb, row.trigger, row.lesson_norm],
                    parse_row,
                )
                .optional()?;
            match existing {
                Some(mut prior) => {
                    let mut new_evidence = false;
                    for id in &row.evidence {
                        if !prior.evidence.contains(id) {
                            prior.evidence.push(*id);
                            new_evidence = true;
                        }
                    }
                    // THE COUNTER MEANS "learned again", not "written again".
                    // A reflection pass re-reading a window it already read
                    // brings no new evidence, and bumping the count there
                    // would inflate every lesson on a schedule — the counter
                    // would measure how often the job ran. Evidence-free
                    // lessons (a human typing the same sentence twice) always
                    // count: the person IS the new occurrence.
                    if new_evidence || row.evidence.is_empty() {
                        prior.occurrences += 1;
                    }
                    prior.updated_at = row.updated_at;
                    // A twin written on a later pass is still this lesson's
                    // twin; a twin already recorded is never overwritten.
                    if prior.memory_id.is_none() {
                        prior.memory_id = row.memory_id;
                    }
                    s.conn.execute(
                        "UPDATE lessons SET occurrences = ?1, evidence_json = ?2,
                                memory_id = ?3, updated_at = ?4
                          WHERE id = ?5",
                        params![
                            prior.occurrences,
                            evidence_json(&prior.evidence),
                            prior.memory_id.map(|m| m.to_string()),
                            prior.updated_at.to_rfc3339(),
                            prior.id.to_string(),
                        ],
                    )?;
                    Ok((prior, LessonUpsert::Merged))
                }
                None => {
                    s.conn.execute(
                        &format!(
                            "INSERT INTO lessons({COLUMNS}) \
                             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"
                        ),
                        params![
                            row.id.to_string(),
                            row.kb,
                            row.diary,
                            row.trigger,
                            row.lesson,
                            row.lesson_norm,
                            row.status,
                            row.source_kind,
                            row.confidence as f64,
                            row.occurrences,
                            evidence_json(&row.evidence),
                            row.memory_id.map(|m| m.to_string()),
                            row.rule,
                            row.created_at.to_rfc3339(),
                            row.updated_at.to_rfc3339(),
                        ],
                    )?;
                    Ok((row.clone(), LessonUpsert::Inserted))
                }
            }
        })
    }

    pub fn get_lesson(&self, id: Uuid) -> Result<Option<LessonRow>, StorageError> {
        self.conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM lessons WHERE id = ?1"),
                params![id.to_string()],
                parse_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// `None` on a filter means every value of it, matching every other
    /// scoped listing in this file.
    pub fn list_lessons(
        &self,
        kb: Option<&str>,
        trigger: Option<&str>,
        status: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<LessonRow>, StorageError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM lessons
              WHERE (?1 IS NULL OR kb = ?1)
                AND (?2 IS NULL OR trigger = ?2)
                AND (?3 IS NULL OR status = ?3)
              ORDER BY updated_at DESC, id ASC
              LIMIT ?4 OFFSET ?5"
        ))?;
        let capped = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![kb, trigger, status, capped, offset as i64],
            parse_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    /// The real total behind a page. `returned` is what fits; this is what
    /// exists.
    pub fn count_lessons(
        &self,
        kb: Option<&str>,
        trigger: Option<&str>,
        status: Option<&str>,
    ) -> Result<i64, StorageError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM lessons
                  WHERE (?1 IS NULL OR kb = ?1)
                    AND (?2 IS NULL OR trigger = ?2)
                    AND (?3 IS NULL OR status = ?3)",
                params![kb, trigger, status],
                |r| r.get(0),
            )
            .map_err(StorageError::from)
    }

    /// `at` is passed in, not read from the clock: the row and its event share
    /// one instant or the rebuild diverges.
    pub fn set_lesson_status(
        &self,
        id: Uuid,
        status: &str,
        at: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "UPDATE lessons SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, at.to_rfc3339(), id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn set_lesson_memory(
        &self,
        id: Uuid,
        memory_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<bool, StorageError> {
        let n = self.conn.execute(
            "UPDATE lessons SET memory_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![memory_id.to_string(), at.to_rfc3339(), id.to_string()],
        )?;
        Ok(n > 0)
    }
}

/// The three `lesson.*` payloads, built in one place so the writer and the
/// rebuild dispatcher cannot drift. Schemas (interfaces §2, D24/X-3):
///
/// - `lesson.recorded` — `{ lesson_id, kb, diary, trigger, lesson, lesson_norm,
///   status, source_kind, confidence, rule?, evidence: [event_id],
///   memory_id?, occurrences, created_at, updated_at }`
/// - `lesson.confirmed` — `{ lesson_id, actor, note?, updated_at, memory_id? }`
/// - `lesson.retracted` — `{ lesson_id, actor, reason?, updated_at }`
///
/// `occurrences` is the POST-write count on purpose: replay SETS it rather
/// than incrementing, which is what makes a rebuild byte-identical when the
/// same lesson was learned five times.
pub mod payload {
    use super::*;

    pub fn lesson_recorded(row: &LessonRow) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("lesson_id".into(), json!(row.id.to_string()));
        m.insert("kb".into(), json!(row.kb));
        m.insert("diary".into(), json!(row.diary));
        m.insert("trigger".into(), json!(row.trigger));
        m.insert("lesson".into(), json!(row.lesson));
        m.insert("lesson_norm".into(), json!(row.lesson_norm));
        m.insert("status".into(), json!(row.status));
        m.insert("source_kind".into(), json!(row.source_kind));
        m.insert("confidence".into(), json!(row.confidence));
        m.insert("occurrences".into(), json!(row.occurrences));
        m.insert(
            "evidence".into(),
            json!(row
                .evidence
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()),
        );
        m.insert("created_at".into(), json!(row.created_at.to_rfc3339()));
        m.insert("updated_at".into(), json!(row.updated_at.to_rfc3339()));
        if let Some(mem) = row.memory_id {
            m.insert("memory_id".into(), json!(mem.to_string()));
        }
        if let Some(rule) = &row.rule {
            m.insert("rule".into(), json!(rule));
        }
        Value::Object(m)
    }

    pub fn lesson_confirmed(
        lesson_id: Uuid,
        actor: &str,
        note: Option<&str>,
        memory_id: Option<Uuid>,
        at: DateTime<Utc>,
    ) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("lesson_id".into(), json!(lesson_id.to_string()));
        m.insert("actor".into(), json!(actor));
        m.insert("updated_at".into(), json!(at.to_rfc3339()));
        if let Some(n) = note {
            m.insert("note".into(), json!(n));
        }
        if let Some(mem) = memory_id {
            m.insert("memory_id".into(), json!(mem.to_string()));
        }
        Value::Object(m)
    }

    pub fn lesson_retracted(
        lesson_id: Uuid,
        actor: &str,
        reason: Option<&str>,
        at: DateTime<Utc>,
    ) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("lesson_id".into(), json!(lesson_id.to_string()));
        m.insert("actor".into(), json!(actor));
        m.insert("updated_at".into(), json!(at.to_rfc3339()));
        if let Some(r) = reason {
            m.insert("reason".into(), json!(r));
        }
        Value::Object(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_store;

    fn row(kb: &str, trigger: &str, lesson: &str) -> LessonRow {
        let now = Utc::now();
        LessonRow {
            id: Uuid::new_v4(),
            kb: kb.to_string(),
            diary: "lessons".to_string(),
            trigger: trigger.to_string(),
            lesson: lesson.to_string(),
            lesson_norm: normalize_lesson(lesson),
            status: STATUS_PROPOSED.to_string(),
            source_kind: "agent_extracted".to_string(),
            confidence: 0.5,
            occurrences: 1,
            evidence: vec![Uuid::new_v4()],
            memory_id: None,
            rule: Some("caller_overruled_machine".to_string()),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn the_same_lesson_twice_is_one_row_with_a_counter() {
        let (_tmp, _g, store) = temp_store();
        store.ensure_kb("default", None).unwrap();
        let first = row(
            "default",
            "fresh_write_correction",
            "Ask   before   superseding",
        );
        let (_, outcome) = store.upsert_lesson(&first).unwrap();
        assert_eq!(outcome, LessonUpsert::Inserted);

        // Different spacing and case: the same lesson, and the norm proves it.
        let mut second = row(
            "default",
            "fresh_write_correction",
            "ask before superseding",
        );
        second.updated_at = first.updated_at + chrono::Duration::seconds(5);
        let (merged, outcome) = store.upsert_lesson(&second).unwrap();
        assert_eq!(outcome, LessonUpsert::Merged);
        assert_eq!(merged.id, first.id, "the surviving id is the first one");
        assert_eq!(merged.occurrences, 2);
        assert_eq!(merged.evidence.len(), 2, "evidence is the union");
        assert_eq!(store.count_lessons(None, None, None).unwrap(), 1);
    }

    #[test]
    fn a_merge_never_rewords_a_confirmed_lesson() {
        let (_tmp, _g, store) = temp_store();
        store.ensure_kb("default", None).unwrap();
        let first = row("default", "t", "keep both values");
        let (stored, _) = store.upsert_lesson(&first).unwrap();
        store
            .set_lesson_status(stored.id, STATUS_CONFIRMED, Utc::now())
            .unwrap();
        let (merged, _) = store
            .upsert_lesson(&row("default", "t", "Keep Both Values"))
            .unwrap();
        assert_eq!(merged.status, STATUS_CONFIRMED);
        assert_eq!(merged.lesson, "keep both values");
    }

    #[test]
    fn listing_filters_and_counts_are_the_same_predicate() {
        let (_tmp, _g, store) = temp_store();
        store.ensure_kb("default", None).unwrap();
        for i in 0..5 {
            store
                .upsert_lesson(&row("default", "t", &format!("lesson {i}")))
                .unwrap();
        }
        let page = store.list_lessons(None, None, None, 2, 0).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(store.count_lessons(None, None, None).unwrap(), 5);
        assert!(store
            .list_lessons(Some("nope"), None, None, 10, 0)
            .unwrap()
            .is_empty());
    }
}
