//! v13 -> v14: the compiled person graph's typed links become facts.
//!
//! `compile` was the only writer of `relations`, and it is gone; from v14 on
//! nothing reads that table. Almost all of it is `co_occurs_with` (two names
//! in one chunk), which carries no claim and is left where it is. The typed
//! links — `introduced_by`, `friend_of`, `family_of`, … — are things the owner
//! wrote down, and `people` now reads facts, so each one with no matching live
//! fact is copied into `facts` once.
//!
//! Nothing is deleted or rewritten: the copy only INSERTs, through the same
//! row writer and `fact.observed` event every fact uses, so a journal rebuild
//! reproduces it. Every copied fact is `source_kind = compile_llm` (the
//! compiler extracted the link), sourced to the first live memory the
//! relation cites, and `migration_log` maps each old relation id to its fact.
//! A store holding typed relations gets a `VACUUM INTO` backup first, like v10.
//!
//! Idempotent: a link already present as a live fact (either direction for the
//! symmetric relations) is skipped, and that includes the copies themselves, so
//! running the copy twice writes nothing the second time.

use crate::error::StorageError;
use crate::sqlite::SqliteStore;
use chrono::{DateTime, Utc};
use engram_core::relations::canonical_relation;
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
        "friend_of" | "knows" | "married_to" | "partner_of" | "works_with" | "family_of"
    )
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
    /// Already present as a live fact; nothing written.
    pub already_facts: usize,
    /// No live memory behind the relation (its sources were all forgotten),
    /// so there is nothing a fact could cite. Left in `relations`.
    pub skipped_no_live_source: usize,
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
        let mut report = self.copy_typed_relations_to_facts()?;
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
                    r.provenance_json
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
                })
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        for rel in rows {
            debug_assert!(!UNTYPED.contains(&rel.predicate.as_str()));
            report.typed_relations += 1;
            let predicate = canonical_relation(&rel.predicate);
            let subject_norm = rel.from.trim().to_lowercase();
            let object_norm = rel.to.trim().to_lowercase();
            if self.live_fact_exists(&rel.kb, &predicate, &subject_norm, &object_norm)? {
                report.already_facts += 1;
                continue;
            }
            let Some((memory_id, diary, valid_from)) = self.live_source(&rel.provenance)? else {
                report.skipped_no_live_source += 1;
                continue;
            };
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

    fn live_fact_exists(
        &self,
        kb: &str,
        predicate: &str,
        subject_norm: &str,
        object_norm: &str,
    ) -> Result<bool, StorageError> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM facts
                 WHERE kb = ?1 AND predicate = ?2
                   AND valid_until IS NULL AND superseded_by IS NULL
                   AND ((subject_norm = ?3 AND object_norm = ?4)
                        OR (?5 AND subject_norm = ?4 AND object_norm = ?3))
                 LIMIT 1",
                params![kb, predicate, subject_norm, object_norm, symmetric(predicate)],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// The first live memory a relation's provenance cites — directly, or
    /// through a chunk — with its diary and the time its event happened.
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
            let memory = source.get("memory_id").and_then(|v| v.as_str());
            let chunk = source.get("chunk_id").and_then(|v| v.as_str());
            let row: Option<(String, String, Option<String>)> = self
                .conn
                .query_row(
                    "SELECT m.id, m.diary, COALESCE(m.event_time, m.created_at)
                     FROM memories m
                     WHERE m.deleted = 0
                       AND (m.id = ?1
                            OR m.id = (SELECT memory_id FROM chunks WHERE id = ?2))
                     LIMIT 1",
                    params![memory, chunk],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
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

    /// A store with one memory, two people, one typed link and one
    /// co-occurrence edge, written the way `compile` wrote them.
    fn seeded() -> (SqliteStore, Uuid) {
        let store = SqliteStore::open_in_memory().unwrap();
        let memory = Uuid::new_v4();
        let now = Utc::now().to_rfc3339();
        store
            .conn
            .execute_batch(&format!(
                "INSERT OR IGNORE INTO knowledge_bases(name, created_at, updated_at)
                     VALUES ('network', '{now}', '{now}');
                 INSERT INTO memories(id, content, created_at, importance, emotional_weight,
                     access_count, stability, source_json, diary, tags_json, deleted, kb)
                 VALUES ('{memory}', 'Lira introduced me to Elias at the lab.', '{now}', 5, 0, 0,
                     1.0, '{{\"type\":\"user\"}}', 'default', '[]', 0, 'network');
                 INSERT INTO entities(id, kb, canonical_name, kind, mention_count)
                     VALUES ('e1', 'network', 'Elias', 'person', 1),
                            ('e2', 'network', 'Lira', 'person', 1);
                 INSERT INTO relations(id, kb, from_entity_id, to_entity_id, predicate, weight,
                     created_at, provenance_json)
                 VALUES ('r1', 'network', 'e1', 'e2', 'introduced_by', 1.0, '{now}',
                         '{{\"sources\":[{{\"memory_id\":\"{memory}\"}}]}}'),
                        ('r2', 'network', 'e1', 'e2', 'co_occurs_with', 3.0, '{now}', '[]');"
            ))
            .unwrap();
        (store, memory)
    }

    fn live_facts(store: &SqliteStore) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM facts WHERE valid_until IS NULL AND superseded_by IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn a_typed_link_becomes_one_fact_and_a_second_run_writes_nothing() {
        let (store, memory) = seeded();
        let first = store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap();
        assert_eq!(first.typed_relations, 1, "co_occurs_with is not a link");
        assert_eq!(first.copied.len(), 1);
        assert_eq!(first.copied[0].relation_id, "r1");
        let fact = store
            .get_active_facts("elias", "introduced_by", "default")
            .unwrap();
        assert_eq!(fact.len(), 1);
        assert_eq!(fact[0].object, "Lira");
        assert_eq!(fact[0].source_memory_id, memory);
        assert_eq!(fact[0].source_kind, SourceKind::CompileLlm);
        assert_eq!(fact[0].kb, "network");

        let second = store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap();
        assert!(second.copied.is_empty());
        assert_eq!(second.already_facts, 1);
        assert_eq!(live_facts(&store), 1);
        // The relation rows themselves are untouched.
        let relations: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM relations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(relations, 2);
    }

    #[test]
    fn a_link_with_only_forgotten_sources_is_left_alone() {
        let (store, memory) = seeded();
        store
            .conn
            .execute(
                "UPDATE memories SET deleted = 1 WHERE id = ?1",
                params![memory.to_string()],
            )
            .unwrap();
        let report = store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap();
        assert_eq!(report.skipped_no_live_source, 1);
        assert_eq!(live_facts(&store), 0);
    }

    #[test]
    fn a_symmetric_link_already_stated_the_other_way_is_not_copied() {
        let (store, _) = seeded();
        store
            .conn
            .execute_batch(
                "UPDATE relations SET predicate = 'friend_of' WHERE id = 'r1';",
            )
            .unwrap();
        // Lira friend_of Elias, stated as a fact.
        store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap();
        store
            .conn
            .execute_batch(
                "UPDATE facts SET subject = 'Lira', subject_norm = 'lira',
                                  object = 'Elias', object_norm = 'elias';",
            )
            .unwrap();
        let report = store.with_tx(|s| s.copy_typed_relations_to_facts()).unwrap();
        assert_eq!(report.already_facts, 1);
        assert_eq!(live_facts(&store), 1);
    }
}
