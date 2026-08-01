//! The append-only journal: every projection mutation's event.
//!
//! v0.3 mutated projections directly and hard-deleted derived state, so a
//! content edit erased a memory's facts, its extraction marker and — through
//! the FK cascade on `fact_conflict_queue` — 14.5k adjudication rows, with no
//! record that any of it had ever existed. The journal inverts that: the
//! tables agents read become projections of this log, and `admin rebuild`
//! reconstructs them.
//!
//! `events` carries NO foreign keys on purpose. A memory can be retracted and
//! a fact re-derived under a new uuid; the record of both must outlive them.
//! Any FK here would reintroduce exactly the cascade the journal exists to
//! survive.

use crate::error::StorageError;
use crate::sqlite::SqliteStore;
use chrono::{DateTime, Utc};
use engram_core::events::EventKind;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use uuid::Uuid;

/// The two extractor ids in use, spelled once. Three specs had three
/// spellings of "the thing that derived these facts"; a mismatch turns the
/// content-addressed memoization into a permanent cache miss.
pub const EXTRACTOR_FACT_LLM: &str = "fact_llm@1";
pub const EXTRACTOR_COMPILE: &str = "compile_llm@1";

/// Actor string for events the v9→v10 migration synthesizes.
pub const ACTOR_MIGRATION_V10: &str = "migration:v10";
/// Actor string for events synthesized while importing a v2 export.
pub const ACTOR_IMPORT_V2: &str = "import:v2";

pub const JOURNAL_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS events (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    ts         TEXT NOT NULL,
    actor      TEXT NOT NULL,
    kind       TEXT NOT NULL,
    subject_id TEXT,
    kb         TEXT,
    diary      TEXT,
    payload    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_subject ON events(subject_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_kind    ON events(kind, seq);
CREATE INDEX IF NOT EXISTS idx_events_kb_seq  ON events(kb, seq);
"#;

/// An event about to be appended. Built through [`EventDraft::new`] so
/// `actor` can never be forgotten by a projection writer.
#[derive(Debug, Clone)]
pub struct EventDraft {
    pub kind: EventKind,
    pub subject_id: Option<Uuid>,
    pub kb: Option<String>,
    pub diary: Option<String>,
    pub actor: String,
    pub payload: Value,
}

impl EventDraft {
    pub fn new(kind: EventKind, actor: impl Into<String>, payload: Value) -> Self {
        EventDraft {
            kind,
            subject_id: None,
            kb: None,
            diary: None,
            actor: actor.into(),
            payload,
        }
    }

    pub fn subject(mut self, id: Uuid) -> Self {
        self.subject_id = Some(id);
        self
    }

    pub fn scope(mut self, kb: Option<&str>, diary: Option<&str>) -> Self {
        self.kb = kb.map(|s| s.to_string());
        self.diary = diary.map(|s| s.to_string());
        self
    }
}

/// A stored event, read back.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub seq: i64,
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub actor: String,
    pub kind: EventKind,
    pub subject_id: Option<Uuid>,
    pub kb: Option<String>,
    pub diary: Option<String>,
    pub payload: Value,
}

impl EventRow {
    /// A payload field as a string, or `MalformedRow`. Replay must never
    /// invent a default for a field the writer promised: a rebuilt row with a
    /// silently-empty column is worse than a rebuild that stops.
    pub fn payload_str(&self, key: &str) -> Result<String, StorageError> {
        self.payload
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                StorageError::MalformedRow(format!(
                    "event {} ({}) has no string field `{key}`",
                    self.id, self.kind
                ))
            })
    }

    pub fn payload_opt_str(&self, key: &str) -> Option<String> {
        self.payload
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn payload_uuid(&self, key: &str) -> Result<Uuid, StorageError> {
        let raw = self.payload_str(key)?;
        Uuid::parse_str(&raw).map_err(|e| {
            StorageError::MalformedRow(format!(
                "event {} field `{key}` is not a uuid: {e}",
                self.id
            ))
        })
    }

    pub fn payload_opt_uuid(&self, key: &str) -> Option<Uuid> {
        self.payload_opt_str(key)
            .and_then(|s| Uuid::parse_str(&s).ok())
    }

    pub fn payload_bool(&self, key: &str) -> bool {
        self.payload
            .get(key)
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }
}

/// Deterministic serialization: object keys sorted recursively, no
/// whitespace. `admin rebuild` must reproduce byte-identical payloads, and
/// that guarantee cannot rest on a `serde_json` feature flag another crate
/// in the workspace might switch on.
pub fn canonical_json(v: &Value) -> String {
    fn write(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort_unstable();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String((*k).clone()).to_string());
                    out.push(':');
                    write(&map[*k], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write(v, &mut out);
    out
}

fn parse_event_row(r: &rusqlite::Row) -> Result<EventRow, StorageError> {
    let id_str: String = r.get(1)?;
    let ts_str: String = r.get(2)?;
    let kind_str: String = r.get(4)?;
    let subject: Option<String> = r.get(5)?;
    let payload_str: String = r.get(8)?;
    // An unparseable kind is an error, never a silent skip: a skipped event
    // is a projection that quietly diverges from its own journal.
    let kind = EventKind::parse(&kind_str)
        .ok_or_else(|| StorageError::MalformedRow(format!("unknown event kind `{kind_str}`")))?;
    Ok(EventRow {
        seq: r.get(0)?,
        id: Uuid::parse_str(&id_str)
            .map_err(|e| StorageError::MalformedRow(format!("event id `{id_str}`: {e}")))?,
        ts: DateTime::parse_from_rfc3339(&ts_str)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| StorageError::MalformedRow(format!("event ts `{ts_str}`: {e}")))?,
        actor: r.get(3)?,
        kind,
        subject_id: subject.as_deref().and_then(|s| Uuid::parse_str(s).ok()),
        kb: r.get(6)?,
        diary: r.get(7)?,
        payload: serde_json::from_str(&payload_str)?,
    })
}

const EVENT_COLUMNS: &str = "seq, id, ts, actor, kind, subject_id, kb, diary, payload";

impl SqliteStore {
    /// Append inside the caller's OPEN transaction. Never opens one itself:
    /// invariant 1 (projection mutation and event append are one transaction)
    /// is a property of the caller's `with_tx`, and a self-opening append
    /// would commit an event whose effect later rolled back.
    pub fn append_event(&self, draft: &EventDraft) -> Result<(i64, Uuid), StorageError> {
        debug_assert!(
            !self.conn.is_autocommit(),
            "append_event outside a transaction violates journal invariant 1"
        );
        let id = Uuid::new_v4();
        let ts = Utc::now();
        let seq = self
            .insert_event(id, ts, draft)?
            .expect("a fresh v4 uuid cannot collide with a stored event id");
        Ok((seq, id))
    }

    /// The identity-preserving sibling, and the only way genesis synthesis and
    /// v3 import can work: `append_event` mints a fresh uuid and stamps
    /// `now()`, which would make the migration non-idempotent (no
    /// deterministic id to `INSERT OR IGNORE` against) and would erase an
    /// imported journal's original timestamps. `Ok(None)` = already present.
    pub fn append_event_with_identity(
        &self,
        id: Uuid,
        ts: DateTime<Utc>,
        draft: &EventDraft,
    ) -> Result<Option<i64>, StorageError> {
        debug_assert!(
            !self.conn.is_autocommit(),
            "append_event_with_identity outside a transaction violates journal invariant 1"
        );
        self.insert_event(id, ts, draft)
    }

    fn insert_event(
        &self,
        id: Uuid,
        ts: DateTime<Utc>,
        draft: &EventDraft,
    ) -> Result<Option<i64>, StorageError> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO events(id, ts, actor, kind, subject_id, kb, diary, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id.to_string(),
                ts.to_rfc3339(),
                draft.actor,
                draft.kind.as_str(),
                draft.subject_id.map(|u| u.to_string()),
                draft.kb,
                draft.diary,
                canonical_json(&draft.payload),
            ],
        )?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(self.conn.last_insert_rowid()))
    }

    pub fn events_for_subject(&self, id: Uuid) -> Result<Vec<EventRow>, StorageError> {
        let sql =
            format!("SELECT {EVENT_COLUMNS} FROM events WHERE subject_id = ?1 ORDER BY seq ASC");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.to_string()])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(parse_event_row(r)?);
        }
        Ok(out)
    }

    /// Ordered journal slice for rebuild, export and `admin rebuild --kb`.
    pub fn events_slice(
        &self,
        after_seq: i64,
        kb: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<EventRow>, StorageError> {
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events
              WHERE seq > ?1 AND (?2 IS NULL OR kb = ?2)
              ORDER BY seq ASC LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let capped = limit
            .map(|l| i64::try_from(l).unwrap_or(i64::MAX))
            .unwrap_or(-1);
        let mut rows = stmt.query(params![after_seq, kb, capped])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            out.push(parse_event_row(r)?);
        }
        Ok(out)
    }

    pub fn event_by_id(&self, id: Uuid) -> Result<Option<EventRow>, StorageError> {
        let sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![id.to_string()])?;
        match rows.next()? {
            Some(r) => Ok(Some(parse_event_row(r)?)),
            None => Ok(None),
        }
    }

    pub fn max_event_seq(&self) -> Result<i64, StorageError> {
        let seq: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM events", [], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(seq.unwrap_or(0))
    }

    pub fn count_events(&self) -> Result<i64, StorageError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?)
    }
}

/// Payload construction lives here and nowhere else, so the migration's
/// synthesized genesis events and the live write path emit byte-identical
/// shapes. That is not tidiness: it is what makes a migrated store
/// replayable by the same rebuild dispatcher as a store written today.
pub mod payload {
    use super::*;
    use engram_core::types::Fact;

    fn drop_nulls(mut m: Map<String, Value>) -> Value {
        m.retain(|_, v| !v.is_null());
        Value::Object(m)
    }

    pub fn memory_written(
        content_hash: &str,
        importance: i64,
        tags: &[String],
        event_time: Option<&str>,
        source: &str,
        genesis: bool,
    ) -> Value {
        let mut m = Map::new();
        m.insert("content_hash".into(), json!(content_hash));
        m.insert("importance".into(), json!(importance));
        m.insert("tags".into(), json!(tags));
        m.insert("event_time".into(), json!(event_time));
        m.insert("source".into(), json!(source));
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    pub fn memory_revised(memory_id: Uuid, content_hash: &str, prev_content_hash: &str) -> Value {
        json!({
            "memory_id": memory_id.to_string(),
            "content_hash": content_hash,
            "prev_content_hash": prev_content_hash,
        })
    }

    pub fn memory_meta_changed(memory_id: Uuid, importance: Option<i64>) -> Value {
        let mut m = Map::new();
        m.insert("memory_id".into(), json!(memory_id.to_string()));
        m.insert("importance".into(), json!(importance));
        drop_nulls(m)
    }

    pub fn memory_retracted(memory_id: Uuid, reason: Option<&str>) -> Value {
        let mut m = Map::new();
        m.insert("memory_id".into(), json!(memory_id.to_string()));
        m.insert("reason".into(), json!(reason));
        drop_nulls(m)
    }

    pub fn derivation_ran(
        memory_id: Uuid,
        content_hash: &str,
        deriver: &str,
        model: Option<&str>,
        output_fact_ids: &[Uuid],
        genesis: bool,
    ) -> Value {
        let ids: Vec<String> = output_fact_ids.iter().map(|u| u.to_string()).collect();
        let mut m = Map::new();
        m.insert("memory_id".into(), json!(memory_id.to_string()));
        m.insert("content_hash".into(), json!(content_hash));
        m.insert("deriver".into(), json!(deriver));
        m.insert("model".into(), json!(model));
        m.insert("fact_count".into(), json!(ids.len() as i64));
        m.insert("output_fact_ids".into(), json!(ids));
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    /// `fact_type` and `diary` are additive on top of interfaces §2's list and
    /// are not garnish: without them a rebuilt fact comes back `assertion` in
    /// the default diary, and the byte-identical property fails on the first
    /// `profile_slot` row in any store.
    pub fn fact_observed(f: &Fact, genesis: bool) -> Value {
        let mut m = Map::new();
        m.insert("fact_id".into(), json!(f.id.to_string()));
        m.insert("subject".into(), json!(f.subject));
        m.insert("subject_norm".into(), json!(f.subject_norm));
        m.insert("relation".into(), json!(f.predicate));
        m.insert("object".into(), json!(f.object));
        m.insert("object_norm".into(), json!(f.object_norm));
        m.insert("confidence".into(), json!(f.confidence));
        m.insert("fact_type".into(), json!(f.fact_type.as_str()));
        m.insert("diary".into(), json!(f.diary));
        m.insert("created_at".into(), json!(f.created_at.to_rfc3339()));
        m.insert(
            "valid_from".into(),
            json!(f.valid_from.map(|t| t.to_rfc3339())),
        );
        m.insert(
            "valid_until".into(),
            json!(f.valid_until.map(|t| t.to_rfc3339())),
        );
        m.insert(
            "source_memory_id".into(),
            json!(f.source_memory_id.to_string()),
        );
        m.insert("source_kind".into(), json!(f.source_kind.as_str()));
        m.insert("resolution".into(), json!(f.resolution));
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    /// `sets_superseded_by` [R2]: the projection's `superseded_by`/
    /// `superseded_at` compat pair must be rebuildable from this one event,
    /// and its trigger is the presence of a SUCCESSOR — not `reason ==
    /// "superseded"`. A caller correction closes with `corrected` and still
    /// has a successor.
    #[allow(clippy::too_many_arguments)]
    pub fn fact_interval_closed(
        fact_id: Uuid,
        valid_until: DateTime<Utc>,
        closed_by: Option<Uuid>,
        reason: &str,
        authority: &str,
        sets_superseded_by: bool,
        genesis: bool,
    ) -> Value {
        let mut m = Map::new();
        m.insert("fact_id".into(), json!(fact_id.to_string()));
        m.insert("valid_until".into(), json!(valid_until.to_rfc3339()));
        m.insert("closed_by".into(), json!(closed_by.map(|u| u.to_string())));
        m.insert("reason".into(), json!(reason));
        m.insert("authority".into(), json!(authority));
        m.insert("sets_superseded_by".into(), json!(sets_superseded_by));
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    pub fn fact_confirmed(fact_id: Uuid) -> Value {
        json!({ "fact_id": fact_id.to_string() })
    }

    /// `suggested` and `note` are additive on top of interfaces §2's list:
    /// both are queue columns every conflicts listing reads, so a rebuild
    /// without them renders a different backlog than the one it replaced.
    #[allow(clippy::too_many_arguments)]
    pub fn conflict_opened(
        conflict_id: Uuid,
        new_fact_id: Uuid,
        old_fact_id: Uuid,
        detector: &str,
        score: Option<f32>,
        suggested: Option<&str>,
        note: Option<&str>,
        decision_key: Option<&str>,
        member_key: Option<&str>,
        registry_version: Option<&str>,
        created_at: &str,
        genesis: bool,
    ) -> Value {
        let mut m = Map::new();
        m.insert("conflict_id".into(), json!(conflict_id.to_string()));
        m.insert("new_fact_id".into(), json!(new_fact_id.to_string()));
        m.insert("old_fact_id".into(), json!(old_fact_id.to_string()));
        m.insert("detector".into(), json!(detector));
        m.insert("rule".into(), json!(detector));
        m.insert("score".into(), json!(score));
        m.insert("suggested".into(), json!(suggested));
        m.insert("note".into(), json!(note));
        m.insert("decision_key".into(), json!(decision_key));
        m.insert("member_key".into(), json!(member_key));
        m.insert("registry_version".into(), json!(registry_version));
        m.insert("created_at".into(), json!(created_at));
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    /// `collapsed_into` is additive and set only by the migration's
    /// duplicate-key collapse, so `status='superseded_question'` is
    /// rebuildable rather than inferred from note text.
    #[allow(clippy::too_many_arguments)]
    pub fn decision_recorded(
        decision_id: Uuid,
        semantic_key: &str,
        key_version: &str,
        verdict: &str,
        authority: &str,
        conflict_id: Option<Uuid>,
        note: Option<&str>,
        actor: &str,
        kb: &str,
        diary: &str,
        decided_at: &str,
        collapsed_into: Option<Uuid>,
        genesis: bool,
    ) -> Value {
        let mut m = Map::new();
        m.insert("decision_id".into(), json!(decision_id.to_string()));
        m.insert("semantic_key".into(), json!(semantic_key));
        m.insert("key_version".into(), json!(key_version));
        m.insert("verdict".into(), json!(verdict));
        m.insert("authority".into(), json!(authority));
        m.insert(
            "conflict_id".into(),
            json!(conflict_id.map(|u| u.to_string())),
        );
        m.insert("note".into(), json!(note));
        m.insert("actor".into(), json!(actor));
        m.insert("kb".into(), json!(kb));
        m.insert("diary".into(), json!(diary));
        m.insert("decided_at".into(), json!(decided_at));
        m.insert(
            "collapsed_into".into(),
            json!(collapsed_into.map(|u| u.to_string())),
        );
        if genesis {
            m.insert("genesis".into(), json!(true));
        }
        drop_nulls(m)
    }

    pub fn decision_replayed(
        decision_id: Uuid,
        new_fact_id: Uuid,
        old_fact_id: Option<Uuid>,
        applied_verdict: &str,
    ) -> Value {
        let mut m = Map::new();
        m.insert("decision_id".into(), json!(decision_id.to_string()));
        m.insert("new_fact_id".into(), json!(new_fact_id.to_string()));
        m.insert(
            "old_fact_id".into(),
            json!(old_fact_id.map(|u| u.to_string())),
        );
        m.insert("applied_verdict".into(), json!(applied_verdict));
        drop_nulls(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::temp_store;

    fn draft(kind: EventKind) -> EventDraft {
        EventDraft::new(kind, "cli:default", json!({"b": 1, "a": 2}))
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let a = json!({"z": {"y": 1, "x": [3, {"b": 1, "a": 2}]}, "m": 0});
        let b = json!({"m": 0, "z": {"x": [3, {"a": 2, "b": 1}], "y": 1}});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(
            canonical_json(&json!({"b": 1, "a": "x"})),
            r#"{"a":"x","b":1}"#
        );
    }

    #[test]
    fn append_event_assigns_ascending_seq_and_unique_ids() {
        let (_tmp, _g, store) = temp_store();
        let subject = Uuid::new_v4();
        let (s1, id1, s2, id2) = store
            .with_tx(|s| {
                let (s1, id1) =
                    s.append_event(&draft(EventKind::MemoryWritten).subject(subject))?;
                let (s2, id2) =
                    s.append_event(&draft(EventKind::MemoryRevised).subject(subject))?;
                Ok((s1, id1, s2, id2))
            })
            .unwrap();
        assert!(s2 > s1, "seq must ascend");
        assert_ne!(id1, id2);

        let rows = store.events_for_subject(subject).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, EventKind::MemoryWritten);
        assert_eq!(rows[1].kind, EventKind::MemoryRevised);
        assert_eq!(rows[0].seq, s1);
        assert_eq!(store.max_event_seq().unwrap(), s2);
    }

    #[test]
    fn a_rolled_back_transaction_leaves_no_event() {
        let (_tmp, _g, store) = temp_store();
        let subject = Uuid::new_v4();
        let err = store.with_tx(|s| {
            s.append_event(&draft(EventKind::MemoryWritten).subject(subject))?;
            Err::<(), _>(StorageError::NotFound("deliberate".into()))
        });
        assert!(err.is_err());
        assert!(
            store.events_for_subject(subject).unwrap().is_empty(),
            "invariant 1 from the failure side: no event survives its rolled-back effect"
        );
    }

    #[test]
    fn appending_a_known_identity_twice_is_a_no_op() {
        let (_tmp, _g, store) = temp_store();
        let id = Uuid::new_v4();
        let ts = Utc::now();
        let d = draft(EventKind::FactObserved);
        let (first, second) = store
            .with_tx(|s| {
                Ok((
                    s.append_event_with_identity(id, ts, &d)?,
                    s.append_event_with_identity(id, ts, &d)?,
                ))
            })
            .unwrap();
        assert!(first.is_some());
        assert_eq!(second, None, "the second append must report the collision");
        assert_eq!(store.count_events().unwrap(), 1);
        let row = store.event_by_id(id).unwrap().expect("stored");
        assert_eq!(row.ts.to_rfc3339(), ts.to_rfc3339());
    }

    #[test]
    fn an_unknown_kind_is_an_error_not_a_silent_skip() {
        let (_tmp, _g, store) = temp_store();
        store
            .with_tx(|s| {
                s.conn.execute(
                    "INSERT INTO events(id, ts, actor, kind, payload)
                     VALUES ('11111111-1111-4111-8111-111111111111',
                             '2026-01-01T00:00:00+00:00', 'cli:default', 'memory.exploded', '{}')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let err = store.events_slice(0, None, None).unwrap_err();
        assert!(
            matches!(err, StorageError::MalformedRow(_)),
            "got {err:?} — a skipped event is a projection that silently diverges"
        );
    }
}
