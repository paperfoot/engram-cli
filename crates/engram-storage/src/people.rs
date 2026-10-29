//! People as a view over live facts.
//!
//! There is no person table: a person is a name that live facts treat as one —
//! the subject of a personal fact (`works_at`, `has_age`, `takes_medication`, …)
//! or either end of a link between people (`friend_of`, `introduced_by`, …).
//! Everything `people` prints is therefore a fact an agent can cite, close or
//! correct with the fact commands, and a person appears the moment the first
//! such fact is written.

use crate::error::StorageError;
use crate::sqlite::{fact_columns, parse_fact_row_pub, SqliteStore, OPEN_INTERVAL_PREDICATE as OPEN_FACT};
use engram_core::types::Fact;
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;

/// Links between two people. Both ends are people.
pub const PERSON_LINKS: &[&str] = &[
    "knows",
    "met",
    "friend_of",
    "is_friend_of",
    "has_friend",
    "family_of",
    "sibling_of",
    "parent_of",
    "child_of",
    "married_to",
    "partner_of",
    "has_partner",
    "dating",
    "introduced",
    "introduced_by",
    "works_with",
    "colleague_of",
    "collaborates_with",
    "mentor_of",
    "reports_to",
];

/// Facts only a person has. Their subject is a person; their object is not.
pub const PERSON_ATTRIBUTES: &[&str] = &[
    "has_age",
    "born_on",
    "born_in",
    "has_nationality",
    "graduated_from",
    "works_at",
    "has_role",
    "lives_in",
    "met_at",
    "speaks_language",
    "has_nickname",
    "has_email",
    "has_phone_number",
    "client_of",
    "investor_in",
    "allergic_to",
    "takes_medication",
    "diagnosed_with",
    "blood_type",
];

/// Attributes whose object is sometimes a person (`met_at` a conference or a
/// colleague, `client_of` a firm or a consultant). The object does not become
/// a person through them, but a person already known shows the link too.
const OBJECT_MAY_BE_PERSON: &[&str] = &["met_at", "client_of", "investor_in"];

/// Predicates whose object is another name for the subject.
pub const ALIAS_PREDICATES: &[&str] = &["has_nickname", "also_known_as", "goes_by"];

/// One person in one KB.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonRow {
    pub name: String,
    pub name_norm: String,
    pub kb: String,
    /// Live facts that name this person: as subject, or as the other end of a link.
    pub facts: u32,
}

fn in_list(list: &[&str]) -> String {
    list.iter()
        .map(|p| format!("'{p}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn kb_clause(kb: Option<&str>, args: &mut Vec<SqlValue>) -> &'static str {
    match kb {
        Some(kb) => {
            args.push(SqlValue::Text(kb.to_string()));
            "AND kb = ?"
        }
        None => "",
    }
}

/// The rows if they are all one person (one name, any number of KBs), else
/// none: two people answering to one name is not an answer.
fn unique_person(rows: Vec<PersonRow>) -> Vec<PersonRow> {
    let mut norms: Vec<&str> = rows.iter().map(|p| p.name_norm.as_str()).collect();
    norms.sort();
    norms.dedup();
    if norms.len() == 1 {
        rows
    } else {
        Vec::new()
    }
}

impl SqliteStore {
    /// People ranked by how many live facts name them, one row per KB, plus the
    /// total for paging.
    pub fn list_people(
        &self,
        kb: Option<&str>,
        limit: usize,
        offset: usize,
        min_facts: u32,
    ) -> Result<(Vec<PersonRow>, usize), StorageError> {
        let mut args = Vec::new();
        let kb_sql = kb_clause(kb, &mut args);
        let links = in_list(PERSON_LINKS);
        let attributes = in_list(PERSON_ATTRIBUTES);
        let ranked = format!(
            "WITH live AS (SELECT subject, subject_norm, predicate, object, object_norm, kb
                           FROM facts WHERE {OPEN_FACT} {kb_sql}),
                  named AS (SELECT subject AS name, subject_norm AS norm, kb FROM live
                            WHERE predicate IN ({links}) OR predicate IN ({attributes})
                            UNION ALL
                            SELECT object, object_norm, kb FROM live WHERE predicate IN ({links})),
                  people AS (SELECT norm, kb, MIN(name) AS name, COUNT(*) AS n
                             FROM named WHERE norm <> '' GROUP BY norm, kb),
                  counted AS (SELECT p.name, p.norm, p.kb,
                                     (SELECT COUNT(*) FROM live l
                                      WHERE l.kb = p.kb
                                        AND (l.subject_norm = p.norm
                                             OR (l.object_norm = p.norm
                                                 AND l.predicate IN ({links})))) AS n
                              FROM people p)
             SELECT name, norm, kb, n FROM counted WHERE n >= {min_facts}"
        );
        let total: usize = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM ({ranked})"),
            params_from_iter(args.iter()),
            |r| r.get::<_, i64>(0),
        )? as usize;
        let mut stmt = self.conn.prepare(&format!(
            "{ranked} ORDER BY n DESC, norm, kb LIMIT {limit} OFFSET {offset}"
        ))?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), |r| {
                Ok(PersonRow {
                    name: r.get(0)?,
                    name_norm: r.get(1)?,
                    kb: r.get(2)?,
                    facts: r.get::<_, i64>(3)? as u32,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total))
    }

    /// The live facts about one person in one KB: what they are (subject) and
    /// who they are linked to (either end of a link). Links first, newest first.
    pub fn person_facts(
        &self,
        kb: &str,
        name_norm: &str,
        limit: usize,
    ) -> Result<Vec<Fact>, StorageError> {
        let links = in_list(PERSON_LINKS);
        let incoming = in_list(&[PERSON_LINKS, OBJECT_MAY_BE_PERSON].concat());
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {cols} FROM facts
             WHERE {OPEN_FACT} AND kb = ?1
               AND (subject_norm = ?2 OR (object_norm = ?2 AND predicate IN ({incoming})))
             ORDER BY (predicate IN ({links})) DESC, created_at DESC
             LIMIT {limit}",
            cols = fact_columns("")
        ))?;
        let rows = stmt
            .query_map(rusqlite::params![kb, name_norm], parse_fact_row_pub)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The person a name refers to, one row per KB, best-known first: the
    /// exact name; else whoever a nickname fact in scope names; else the one
    /// person whose first name it is. A nickname or first name two people
    /// share resolves to nobody rather than to a guess.
    pub fn resolve_person(&self, kb: Option<&str>, name: &str) -> Result<Vec<PersonRow>, StorageError> {
        let q = name.trim().to_lowercase();
        let (all, _) = self.list_people(kb, usize::MAX >> 1, 0, 1)?;
        let exact: Vec<_> = all.iter().filter(|p| p.name_norm == q).cloned().collect();
        if !exact.is_empty() {
            return Ok(exact);
        }
        let mut args = vec![SqlValue::Text(q.clone())];
        let kb_sql = kb_clause(kb, &mut args);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT kb, subject_norm FROM facts
             WHERE {OPEN_FACT} AND object_norm = ? AND predicate IN ({}) {kb_sql}",
            in_list(ALIAS_PREDICATES)
        ))?;
        let nicknamed: Vec<(String, String)> = stmt
            .query_map(params_from_iter(args.iter()), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        // The nickname must be on the same KB's row: a personal-KB nickname
        // says nothing about a work-KB namesake.
        let by_nickname: Vec<_> = all
            .iter()
            .filter(|p| nicknamed.iter().any(|(k, n)| *k == p.kb && *n == p.name_norm))
            .cloned()
            .collect();
        if !by_nickname.is_empty() {
            return Ok(unique_person(by_nickname));
        }
        let by_first: Vec<_> = all
            .into_iter()
            .filter(|p| p.name_norm.split_whitespace().next() == Some(q.as_str()))
            .collect();
        Ok(unique_person(by_first))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use engram_core::types::{FactType, SourceKind};
    use uuid::Uuid;

    fn fact(store: &SqliteStore, memory: Uuid, s: &str, p: &str, o: &str) {
        let f = Fact {
            id: Uuid::new_v4(),
            source_memory_id: memory,
            subject: s.into(),
            subject_norm: s.to_lowercase(),
            predicate: p.into(),
            object: o.into(),
            object_norm: o.to_lowercase(),
            confidence: 1.0,
            created_at: Utc::now(),
            superseded_by: None,
            superseded_at: None,
            diary: "default".into(),
            fact_type: FactType::default(),
            valid_from: None,
            valid_until: None,
            resolution: None,
            source_kind: SourceKind::UserStated,
            confirmations: 0,
            closed_by: None,
            close_reason: None,
            close_authority: None,
            kb: "network".into(),
        };
        store.with_tx(|s| s.insert_fact_row(&f)).unwrap();
    }

    fn seeded() -> SqliteStore {
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
                 VALUES ('{memory}', 'Lira introduced me to Elias.', '{now}', 5, 0, 0,
                     1.0, '{{\"type\":\"user\"}}', 'default', '[]', 0, 'network');"
            ))
            .unwrap();
        fact(&store, memory, "Elias", "introduced_by", "Lira");
        fact(&store, memory, "Elias", "works_at", "Example Labs");
        fact(&store, memory, "Example Labs", "located_in", "Lisbon");
        store
    }

    #[test]
    fn people_are_the_names_facts_treat_as_people() {
        let store = seeded();
        let (people, total) = store.list_people(None, 10, 0, 1).unwrap();
        let names: Vec<_> = people.iter().map(|p| (p.name.as_str(), p.facts)).collect();
        // Elias: a link and an attribute; Lira: the other end of the link.
        // Example Labs is only ever an object or the subject of a place fact.
        assert_eq!(names, vec![("Elias", 2), ("Lira", 1)]);
        assert_eq!(total, 2);
    }

    #[test]
    fn a_persons_facts_include_links_where_they_are_the_object() {
        let store = seeded();
        let lira = store.person_facts("network", "lira", 50).unwrap();
        assert_eq!(lira.len(), 1);
        assert_eq!(lira[0].subject, "Elias");
        let elias = store.person_facts("network", "elias", 50).unwrap();
        assert_eq!(elias[0].predicate, "introduced_by", "links come first");
        assert_eq!(elias.len(), 2);
    }

    #[test]
    fn a_first_name_resolves_only_when_one_person_has_it() {
        let store = seeded();
        let found = store.resolve_person(None, "elias").unwrap();
        assert_eq!(found[0].name, "Elias");
        let memory: Uuid = store
            .conn
            .query_row("SELECT id FROM memories LIMIT 1", [], |r| r.get::<_, String>(0))
            .map(|id| Uuid::parse_str(&id).unwrap())
            .unwrap();
        fact(&store, memory, "Lira Kostova", "works_at", "Example Labs");
        fact(&store, memory, "Lira Marenova", "works_at", "Example Labs");
        // "Lira" alone is a person here, so the exact match wins.
        assert_eq!(store.resolve_person(None, "lira").unwrap()[0].name, "Lira");
        fact(&store, memory, "Noemi Varkoni", "works_at", "Example Labs");
        fact(&store, memory, "Noemi Brandtke", "works_at", "Example Labs");
        assert!(store.resolve_person(None, "noemi").unwrap().is_empty());
        fact(&store, memory, "Noemi Varkoni", "has_nickname", "Nomi");
        assert_eq!(store.resolve_person(None, "nomi").unwrap()[0].name, "Noemi Varkoni");
        // A nickname two people share names nobody.
        fact(&store, memory, "Noemi Brandtke", "has_nickname", "Nomi");
        assert!(store.resolve_person(None, "nomi").unwrap().is_empty());
        // A nickname outside the requested KB does not count.
        assert!(store.resolve_person(Some("elsewhere"), "nomi").unwrap().is_empty());
    }

    #[test]
    fn a_known_person_sees_an_incoming_met_at() {
        let store = seeded();
        let memory: Uuid = store
            .conn
            .query_row("SELECT id FROM memories LIMIT 1", [], |r| r.get::<_, String>(0))
            .map(|id| Uuid::parse_str(&id).unwrap())
            .unwrap();
        fact(&store, memory, "Elias", "met_at", "Lira");
        let lira = store.person_facts("network", "lira", 50).unwrap();
        assert!(lira.iter().any(|f| f.predicate == "met_at"));
        // ...but a conference is not made a person by being met at.
        fact(&store, memory, "Elias", "met_at", "Basel Summit");
        let (people, _) = store.list_people(None, 10, 0, 1).unwrap();
        assert!(people.iter().all(|p| p.name != "Basel Summit"));
    }

    #[test]
    fn paging_and_threshold() {
        let store = seeded();
        let (page, total) = store.list_people(None, 1, 1, 1).unwrap();
        assert_eq!((page[0].name.as_str(), total), ("Lira", 2));
        let (busy, total) = store.list_people(Some("network"), 10, 0, 2).unwrap();
        assert_eq!((busy.len(), total), (1, 1));
    }
}
