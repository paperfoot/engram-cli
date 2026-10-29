//! v13 -> v14: the compiled person graph's typed links become facts.
//!
//! `compile` was the only writer of `relations`, and it is gone; from v14 on
//! nothing reads that table. Almost all of it is `co_occurs_with` (two names
//! in one chunk), which carries no claim and is left where it is. The typed
//! links between two people — `introduced_by`, `friend_of`, `family_of`, … —
//! are things the owner wrote down, and `people` now reads facts, so each one
//! the facts have never recorded is copied into `facts` once.
//!
//! No memory or fact is deleted or rewritten: the copy only INSERTs, through the same
//! row writer and `fact.observed` event every fact uses, so a journal rebuild
//! reproduces it. Every copied fact is `source_kind = compile_llm` (the
//! compiler extracted the link), sourced to the first cited chunk that still
//! exists — an edit replaces a memory's chunks, so a link whose sentence was
//! corrected away cites nothing live and is not copied. `migration_log` maps
//! each old relation id to its fact. A store holding typed relations gets a
//! `VACUUM INTO` backup first, like v10; if links appear after that check, the
//! migration refuses to write without one.
//!
//! Compile jobs still queued, running or paused are marked `cancelled` with
//! the reason — the only rows v14 changes, and they are job bookkeeping.
//!
//! A one-value relation (`married_to`) the facts already hold for either
//! person, with anyone, is not copied: the facts are newer than the compile.
//! A link the facts have EVER recorded in that KB and diary — open, closed or
//! superseded, under any alias of the predicate, either direction for the
//! symmetric ones — is skipped: a closed fact is a correction, and copying the
//! compiled link again would undo it. The copies themselves count, so running
//! the copy twice writes nothing the second time.

use crate::error::StorageError;
use crate::sqlite::SqliteStore;
use chrono::{DateTime, Utc};
use engram_core::relations::{canonical_relation, relation_spec};
use engram_core::types::{Fact, FactType, SourceKind};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Predicates that are not links between people: co-occurrence and the
/// generic fallback the compiler used when it had no typed relation.
const UNTYPED: &[&str] = &["co_occurs_with", "related_to"];

/// A link that reads the same both ways, so `B friend_of A` already records
/// `A friend_of B`.
fn symmetric(predicate: &str) -> bool {
    matches!(
        predicate,
        "friend_of"
            | "knows"
            | "met"
            | "married_to"
            | "partner_of"
            | "works_with"
            | "colleague_of"
            | "collaborates_with"
            | "family_of"
            | "sibling_of"
    )
}

/// The compiler's `met_at` between two people meant they met; as a fact the
/// object of `met_at` is a place, so a person-to-person one is filed as `met`.
fn fact_predicate(relation_predicate: &str) -> String {
    match canonical_relation(relation_predicate).as_str() {
        "met_at" => "met".to_string(),
        other => other.to_string(),
    }
}

/// One relation copied into `facts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CopiedRelation {
    pub relation_id: String,
    pub fact_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelationCopyReport {
    /// Typed relations considered.
    pub typed_relations: usize,
    /// Already recorded as a fact (open, closed or superseded); nothing written.
    pub already_facts: usize,
    /// None of the chunks the relation cites still exists in a live memory
    /// (forgotten, or edited since compile), so there is nothing a fact could
    /// cite. Left in `relations`.
    pub skipped_no_live_source: usize,
    /// One end is not a person (an organisation or concept the LLM compiler
    /// linked). Not a person link; left in `relations`.
    #[serde(default)]
    pub skipped_not_people: usize,
    /// A one-value relation (`married_to`, `partner_of`) that the facts
    /// already hold for one of the two people, with anyone, open or closed:
    /// the facts are newer than the compile, so the compiled value is not
    /// set beside them. Left in `relations`.
    #[serde(default)]
    pub skipped_slot_recorded: usize,
    /// Compile jobs still queued, running or paused when compile was removed;
    /// marked `cancelled` so nothing waits on a runner that no longer exists.
    #[serde(default)]
    pub compile_jobs_cancelled: usize,
    pub copied: Vec<CopiedRelation>,
    #[serde(default)]
    pub backup_path: Option<PathBuf>,
}

struct TypedRelation {
    id: String,
    kb: String,
    from: String,
    to: String,
    predicate: String,
    provenance: String,
    people: bool,
}

impl SqliteStore {
    /// True when `relations` holds any typed link, i.e. when v14 will write.
    /// Decides whether opening the store takes the pre-migration backup.
    pub(crate) fn has_typed_relations(&self) -> Result<bool, StorageError> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'relations'",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Ok(false);
        }
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM relations WHERE predicate NOT IN ('co_occurs_with', 'related_to')
                 LIMIT 1",
                [],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub(crate) fn migrate_v14(
        &self,
        backup_path: Option<PathBuf>,
    ) -> Result<RelationCopyReport, StorageError> {
        if backup_path.is_none()
            && self.path != std::path::Path::new(":memory:")
            && self.has_typed_relations()?
        {
            // The open-time check found nothing to copy, but links are here
            // now (an older binary compiled in between). Writing them without
            // the rollback file is the one thing this step must not do; the
            // next open sees them and takes the backup first.
            return Err(StorageError::Migration(
                "person links appeared after the pre-migration backup check; nothing was \
                 changed. Run the command again and the v14 backup will be taken first."
                    .into(),
            ));
        }
        let mut report = self.copy_typed_relations_to_facts()?;
        report.compile_jobs_cancelled = self.cancel_compile_jobs()?;
        report.backup_path = backup_path;
        self.conn.execute(
            "INSERT OR REPLACE INTO migration_log(version, ran_at, summary_json)
             VALUES (14, ?1, ?2)",
            params![Utc::now().to_rfc3339(), serde_json::to_string(&report)?],
        )?;
        tracing::info!(
            target: "engram::migration",
            version = 14,
            copied = report.copied.len(),
            already_facts = report.already_facts,
            skipped_no_live_source = report.skipped_no_live_source,
            skipped_not_people = report.skipped_not_people,
            "v13 -> v14"
        );
        Ok(report)
    }

    /// Copy every typed relation with no matching live fact into `facts`.
    /// Runs inside the caller's transaction. Safe to run again: see the module
    /// note.
    pub fn copy_typed_relations_to_facts(&self) -> Result<RelationCopyReport, StorageError> {
        let mut report = RelationCopyReport::default();
        if !self.has_typed_relations()? {
            return Ok(report);
        }
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.kb, fe.canonical_name, te.canonical_name, r.predicate,
                    r.provenance_json, fe.kind = 'person' AND te.kind = 'person'
             FROM relations r
             JOIN entities fe ON fe.id = r.from_entity_id
             JOIN entities te ON te.id = r.to_entity_id
             WHERE r.predicate NOT IN ('co_occurs_with', 'related_to')
             ORDER BY r.created_at, r.id",
        )?;
        let rows: Vec<TypedRelation> = stmt
            .query_map([], |r| {
                Ok(TypedRelation {
                    id: r.get(0)?,
                    kb: r.get(1)?,
                    from: r.get(2)?,
                    to: r.get(3)?,
                    predicate: r.get(4)?,
                    provenance: r.get(5)?,
                    people: r.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        for rel in rows {
            debug_assert!(!UNTYPED.contains(&rel.predicate.as_str()));
            report.typed_relations += 1;
            if !rel.people {
                report.skipped_not_people += 1;
                continue;
            }
            let predicate = fact_predicate(&rel.predicate);
            let subject_norm = rel.from.trim().to_lowercase();
            let object_norm = rel.to.trim().to_lowercase();
            let Some((memory_id, diary, valid_from)) = self.live_source(&rel.provenance)? else {
                report.skipped_no_live_source += 1;
                continue;
            };
            if self.fact_ever_recorded(&rel.kb, &diary, &predicate, &subject_norm, &object_norm)? {
                report.already_facts += 1;
                continue;
            }
            if relation_spec(&predicate).is_single_valued()
                && (self.slot_recorded(&rel.kb, &diary, &predicate, &subject_norm)?
                    || (symmetric(&predicate)
                        && self.slot_recorded(&rel.kb, &diary, &predicate, &object_norm)?))
            {
                report.skipped_slot_recorded += 1;
                continue;
            }
            let fact = Fact {
                id: Uuid::new_v4(),
                source_memory_id: memory_id,
                subject: rel.from.trim().to_string(),
                subject_norm,
                predicate,
                object: rel.to.trim().to_string(),
                object_norm,
                // The compiler's typed person links carried no confidence of
                // their own; this is what compile wrote for an extracted fact
                // that did not state one.
                confidence: 0.7,
                created_at: Utc::now(),
                superseded_by: None,
                superseded_at: None,
                diary,
                fact_type: FactType::default(),
                valid_from,
                valid_until: None,
                resolution: None,
                source_kind: SourceKind::CompileLlm,
                confirmations: 0,
                closed_by: None,
                close_reason: None,
                close_authority: None,
                kb: rel.kb.clone(),
            };
            self.insert_fact_row(&fact)?;
            report.copied.push(CopiedRelation {
                relation_id: rel.id,
                fact_id: fact.id.to_string(),
            });
        }
        Ok(report)
    }

    /// Does any fact in this KB and diary — open, closed or superseded — give
    /// `norm` a value for this relation, from either end when it is symmetric?
    fn slot_recorded(
        &self,
        kb: &str,
        diary: &str,
        predicate: &str,
        norm: &str,
    ) -> Result<bool, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT predicate, subject_norm = ?3 FROM facts
             WHERE kb = ?1 AND diary = ?2 AND (subject_norm = ?3 OR object_norm = ?3)",
        )?;
        let rows: Vec<(String, bool)> = stmt
            .query_map(params![kb, diary, norm], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows
            .iter()
            .any(|(p, as_subject)| fact_predicate(p) == predicate && (*as_subject || symmetric(predicate))))
    }

    /// Mark compile jobs that can never finish as cancelled. Job bookkeeping,
    /// not memory: the rows stay, with the reason.
    fn cancel_compile_jobs(&self) -> Result<usize, StorageError> {
        // The job columns arrive with `ensure_jobs_schema`, which runs after
        // migrate on an old store: use only what exists. Before `kind`
        // existed every job was a compile job.
        let columns: Vec<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(compile_jobs)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            rows.collect::<Result<_, _>>()?
        };
        if columns.is_empty() {
            return Ok(0);
        }
        let has = |c: &str| columns.iter().any(|x| x == c);
        let mut set = vec!["status = 'cancelled'", "message = 'compile was removed in schema v14'", "updated_at = ?1"];
        if has("finished_at") {
            set.push("finished_at = COALESCE(finished_at, ?1)");
        }
        if has("lease_owner") {
            set.push("lease_owner = NULL");
        }
        if has("lease_expires_at") {
            set.push("lease_expires_at = NULL");
        }
        let kind = if has("kind") { "kind = 'compile' AND" } else { "" };
        Ok(self.conn.execute(
            &format!(
                "UPDATE compile_jobs SET {} WHERE {kind}
                 status NOT IN ('completed', 'failed', 'cancelled', 'abandoned')",
                set.join(", ")
            ),
            params![Utc::now().to_rfc3339()],
        )?)
    }

    /// Has any fact in this KB and diary — open, closed or superseded — ever
    /// recorded this link, under any spelling of its predicate?
    fn fact_ever_recorded(
        &self,
        kb: &str,
        diary: &str,
        predicate: &str,
        subject_norm: &str,
        object_norm: &str,
    ) -> Result<bool, StorageError> {
        let mut stmt = self.conn.prepare(
            "SELECT predicate, subject_norm FROM facts
             WHERE kb = ?1 AND diary = ?2
               AND ((subject_norm = ?3 AND object_norm = ?4)
                    OR (subject_norm = ?4 AND object_norm = ?3))",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![kb, diary, subject_norm, object_norm], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows.iter().any(|(p, s)| {
            let same = fact_predicate(p) == predicate;
            same && (s == subject_norm || symmetric(predicate))
        }))
    }

    /// The memory behind the first cited chunk that still exists, with its
    /// diary and the time its event happened.
    fn live_source(
        &self,
        provenance_json: &str,
    ) -> Result<Option<(Uuid, String, Option<DateTime<Utc>>)>, StorageError> {
        let value: serde_json::Value =
            serde_json::from_str(provenance_json).unwrap_or(serde_json::Value::Null);
        let sources: Vec<serde_json::Value> = match &value {
            serde_json::Value::Array(items) => items.clone(),
            serde_json::Value::Object(map) => match map.get("sources") {
                Some(serde_json::Value::Array(items)) => items.clone(),
                _ => vec![value.clone()],
            },
            _ => Vec::new(),
        };
        for source in &sources {
            // The chunk, not the memory: an edit keeps the memory id but
            // replaces its chunks, so only a surviving chunk proves the
            // sentence the link came from is still what the memory says.
            let Some(chunk) = source.get("chunk_id").and_then(|v| v.as_str()) else {
                continue;
            };
            let mut row: Option<(String, String, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT m.id, m.diary, COALESCE(m.event_time, m.created_at)
                     FROM chunks c JOIN memories m ON m.id = c.memory_id
                     WHERE c.id = ?1 AND m.deleted = 0",
                    params![chunk],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            // `chunks repair` re-chunks a memory without changing its text, so
            // the chunk id is gone but the sentence is not. The memory still
            // supports the link if its current text contains that sentence.
            if row.is_none() {
                if let (Some(memory), Some(sentence)) = (
                    source.get("memory_id").and_then(|v| v.as_str()),
                    source.get("sentence").and_then(|v| v.as_str()),
                ) {
                    let current: Option<(String, String, Option<String>, String)> = self
                        .conn
                        .query_row(
                            "SELECT id, diary, COALESCE(event_time, created_at), content
                             FROM memories WHERE id = ?1 AND deleted = 0",
                            params![memory],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?;
                    if let Some((id, diary, at, content)) = current {
                        let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
                        let sentence = flat(sentence);
                        if !sentence.is_empty() && flat(&content).contains(&sentence) {
                            row = Some((id, diary, at));
                        }
                    }
                }
            }
            if let Some((id, diary, at)) = row {
                let Ok(id) = Uuid::parse_str(&id) else {
                    continue;
                };
                let at = at
                    .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
                    .map(|t| t.with_timezone(&Utc));
                return Ok(Some((id, diary, at)));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMORY: &str = "6f0c2a1e-6d0b-4a55-9d1e-2b7c1f3e9a40";
    const CHUNK: &str = "c1";

    /// A store with one memory and its chunk, two people, one typed link and
    /// one co-occurrence edge, written the way `compile` wrote them.
    fn seeded() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        let now = Utc::now().to_rfc3339();
        store
            .conn
            .execute_batch(&format!(
                "INSERT OR IGNORE INTO knowledge_bases(name, created_at, updated_at)
                     VALUES ('network', '{now}', '{now}');
                 INSERT INTO memories(id, content, created_at, importance, emotional_weight,
                     access_count, stability, source_json, diary, tags_json, deleted, kb)
                 VALUES ('{MEMORY}', 'Lira introduced me to Elias at the lab.', '{now}', 5, 0, 0,
                     1.0, '{{\"type\":\"user\"}}', 'default', '[]', 0, 'network');
                 INSERT INTO chunks(id, memory_id, content, position)
                     VALUES ('{CHUNK}', '{MEMORY}', 'Lira introduced me to Elias at the lab.', 0);
                 INSERT INTO entities(id, kb, canonical_name, kind, mention_count)
                     VALUES ('e1', 'network', 'Elias', 'person', 1),
                            ('e2', 'network', 'Lira', 'person', 1),
                            ('e3', 'network', 'Example Labs', 'organization', 1);
                 INSERT INTO relations(id, kb, from_entity_id, to_entity_id, predicate, weight,
                     created_at, provenance_json)
                 VALUES ('r1', 'network', 'e1', 'e2', 'introduced_by', 1.0, '{now}',
                         '{{\"sources\":[{{\"chunk_id\":\"{CHUNK}\",\"memory_id\":\"{MEMORY}\"}}]}}'),
                        ('r2', 'network', 'e1', 'e2', 'co_occurs_with', 3.0, '{now}', '[]');"
            ))
            .unwrap();
        store
    }

    fn copy(store: &SqliteStore) -> RelationCopyReport {
        store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap()
    }

    fn set_link(store: &SqliteStore, from: &str, to: &str, predicate: &str) {
        store
            .conn
            .execute(
                "UPDATE relations SET from_entity_id = ?1, to_entity_id = ?2, predicate = ?3
                 WHERE id = 'r1'",
                params![from, to, predicate],
            )
            .unwrap();
    }

    fn fact_rows(store: &SqliteStore) -> Vec<(String, String, String, bool)> {
        let mut stmt = store
            .conn
            .prepare(
                "SELECT subject, predicate, object, valid_until IS NULL AND superseded_by IS NULL
                 FROM facts ORDER BY created_at",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// An existing fact, written as an agent or import would have.
    fn existing_fact(store: &SqliteStore, s: &str, p: &str, o: &str, diary: &str, open: bool) {
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: Uuid::parse_str(MEMORY).unwrap(),
            subject: s.into(),
            subject_norm: s.to_lowercase(),
            predicate: p.into(),
            object: o.into(),
            object_norm: o.to_lowercase(),
            confidence: 1.0,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: diary.into(),
            fact_type: FactType::default(),
            valid_from: None,
            valid_until: if open { None } else { Some(Utc::now()) },
            resolution: None,
            source_kind: SourceKind::UserStated,
            confirmations: 0,
            closed_by: None,
            close_reason: if open { None } else { Some("corrected".into()) },
            close_authority: None,
            kb: "network".into(),
        };
        store.with_tx(|s| s.insert_fact_row(&f)).unwrap();
    }

    #[test]
    fn a_typed_link_becomes_one_fact_and_a_second_run_writes_nothing() {
        let store = seeded();
        let first = copy(&store);
        assert_eq!(first.typed_relations, 1, "co_occurs_with is not a link");
        assert_eq!(first.copied.len(), 1);
        assert_eq!(first.copied[0].relation_id, "r1");
        let fact = store
            .get_active_facts("elias", "introduced_by", "default")
            .unwrap();
        assert_eq!(fact.len(), 1);
        assert_eq!(fact[0].object, "Lira");
        assert_eq!(fact[0].source_memory_id.to_string(), MEMORY);
        assert_eq!(fact[0].source_kind, SourceKind::CompileLlm);
        assert_eq!(fact[0].kb, "network");

        let second = copy(&store);
        assert!(second.copied.is_empty());
        assert_eq!(second.already_facts, 1);
        assert_eq!(fact_rows(&store).len(), 1);
        let relations: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(relations, 2, "the relation rows themselves are untouched");
    }

    #[test]
    fn a_link_whose_sentence_was_edited_away_is_not_copied() {
        let store = seeded();
        // `edit` replaces a memory's chunks and keeps the memory.
        store
            .conn
            .execute_batch(&format!(
                "DELETE FROM chunks WHERE id = '{CHUNK}';
                 INSERT INTO chunks(id, memory_id, content, position)
                     VALUES ('c2', '{MEMORY}', 'Noemi introduced me to Elias.', 0);"
            ))
            .unwrap();
        let report = copy(&store);
        assert_eq!(report.skipped_no_live_source, 1);
        assert!(fact_rows(&store).is_empty());
    }

    #[test]
    fn a_link_with_only_forgotten_sources_is_left_alone() {
        let store = seeded();
        store
            .conn
            .execute("UPDATE memories SET deleted = 1 WHERE id = ?1", params![MEMORY])
            .unwrap();
        assert_eq!(copy(&store).skipped_no_live_source, 1);
        assert!(fact_rows(&store).is_empty());
    }

    #[test]
    fn a_closed_fact_is_a_correction_and_the_link_is_not_resurrected() {
        let store = seeded();
        existing_fact(&store, "Elias", "introduced_by", "Lira", "default", false);
        let report = copy(&store);
        assert_eq!(report.already_facts, 1);
        let rows = fact_rows(&store);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].3, "the closed fact stays the only, closed, record");
    }

    #[test]
    fn a_predicate_alias_or_the_reverse_direction_counts_as_recorded() {
        let store = seeded();
        set_link(&store, "e1", "e2", "married_to");
        existing_fact(&store, "Lira", "spouse_of", "Elias", "default", true);
        assert_eq!(copy(&store).already_facts, 1);

        let store = seeded();
        set_link(&store, "e1", "e2", "colleague_of");
        existing_fact(&store, "Lira", "colleague_of", "Elias", "default", true);
        assert_eq!(copy(&store).already_facts, 1);
    }

    #[test]
    fn a_fact_in_another_diary_does_not_suppress_the_copy() {
        let store = seeded();
        existing_fact(&store, "Elias", "introduced_by", "Lira", "private", true);
        assert_eq!(copy(&store).copied.len(), 1);
    }

    #[test]
    fn only_links_between_two_people_are_copied() {
        let store = seeded();
        set_link(&store, "e1", "e3", "partner_of");
        let report = copy(&store);
        assert_eq!(report.skipped_not_people, 1);
        assert!(fact_rows(&store).is_empty());
    }

    #[test]
    fn a_one_value_relation_the_facts_already_hold_is_not_set_twice() {
        let store = seeded();
        set_link(&store, "e1", "e2", "married_to");
        existing_fact(&store, "Elias", "married_to", "Carol", "default", true);
        let report = copy(&store);
        assert_eq!(report.skipped_slot_recorded, 1);
        assert_eq!(fact_rows(&store).len(), 1, "Carol stays the only spouse on record");
    }

    #[test]
    fn a_repaired_chunk_still_supports_the_link_when_the_sentence_is_unchanged() {
        let store = seeded();
        store
            .conn
            .execute_batch(&format!(
                "UPDATE relations SET provenance_json = '{{\"sources\":[{{\"chunk_id\":\"gone\",\"memory_id\":\"{MEMORY}\",\"sentence\":\"Lira introduced me to  Elias\"}}]}}' WHERE id = 'r1';"
            ))
            .unwrap();
        assert_eq!(copy(&store).copied.len(), 1);
    }

    #[test]
    fn unfinishable_compile_jobs_are_cancelled_with_a_reason() {
        let store = seeded();
        store.ensure_jobs_schema().unwrap();
        let now = Utc::now().to_rfc3339();
        store
            .conn
            .execute_batch(&format!(
                "INSERT INTO compile_jobs(id, kb, status, mode, created_at, updated_at, kind)
                 VALUES ('j1', 'network', 'queued', 'evidence', '{now}', '{now}', 'compile'),
                        ('j2', 'network', 'completed', 'evidence', '{now}', '{now}', 'compile');"
            ))
            .unwrap();
        assert_eq!(store.with_tx(|s| s.cancel_compile_jobs()).unwrap(), 1);
        let status: String = store
            .conn
            .query_row("SELECT status FROM compile_jobs WHERE id = 'j1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[test]
    fn a_person_to_person_met_at_is_filed_as_met() {
        let store = seeded();
        set_link(&store, "e1", "e2", "met_at");
        copy(&store);
        assert_eq!(fact_rows(&store)[0].1, "met");
    }
}
