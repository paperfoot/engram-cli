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
use engram_core::relations::canonical_relation;
use engram_core::types::Fact;
use std::collections::{BTreeMap, BTreeSet};

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
/// colleague, `client_of` a firm or a consultant). The object counts as a
/// person when it reads as a person's name, and a person shows the link from
/// that end too.
const OBJECT_MAY_BE_PERSON: &[&str] = &["met_at", "client_of", "investor_in"];

/// Predicates whose object is another name for the subject.
pub const ALIAS_PREDICATES: &[&str] = &["has_nickname", "also_known_as", "goes_by"];

/// One person in one KB.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonRow {
    pub name: String,
    pub name_norm: String,
    pub kb: String,
    /// Live facts that name this person — exactly the facts `person_facts`
    /// returns for them.
    pub facts: u32,
}

/// Every predicate is compared by its canonical form, so `spouse_of` and
/// `married_to` are one relation here as everywhere else in engram.
fn canon(f: &Fact) -> String {
    canonical_relation(&f.predicate)
}

/// Facts organisations have too (an email, an investment, a business
/// partner). They make their subject or object a person only when the name
/// reads as a person's.
const NEEDS_A_NAME: &[&str] = &[
    "has_email",
    "has_phone_number",
    "client_of",
    "investor_in",
    "met_at",
    "partner_of",
    "works_with",
    "collaborates_with",
];

fn subject_is_person(f: &Fact, canon: &str, is_name: PersonName) -> bool {
    (PERSON_LINKS.contains(&canon)
        || PERSON_ATTRIBUTES.contains(&canon)
        || ALIAS_PREDICATES.contains(&canon))
        && (!NEEDS_A_NAME.contains(&canon) || is_name(&f.subject))
}

/// Decides whether a bare value reads as a person's name. The CLI passes
/// `engram_graph::people::looks_like_person_name`; storage does not depend on
/// the graph crate.
pub type PersonName<'a> = &'a dyn Fn(&str) -> bool;

fn object_is_person(f: &Fact, canon: &str, is_name: PersonName) -> bool {
    (PERSON_LINKS.contains(&canon) && (!NEEDS_A_NAME.contains(&canon) || is_name(&f.object)))
        || (OBJECT_MAY_BE_PERSON.contains(&canon) && is_name(&f.object))
}

/// Does this fact belong on `norm`'s page?
fn names(f: &Fact, canon: &str, norm: &str) -> bool {
    f.subject_norm == norm
        || (f.object_norm == norm
            && (PERSON_LINKS.contains(&canon) || OBJECT_MAY_BE_PERSON.contains(&canon)))
}

/// The rows if they are all one person (one name, any number of KBs), else
/// none: two people answering to one name is not an answer.
fn unique_person(rows: Vec<PersonRow>) -> Vec<PersonRow> {
    let norms: BTreeSet<&str> = rows.iter().map(|p| p.name_norm.as_str()).collect();
    if norms.len() == 1 {
        rows
    } else {
        Vec::new()
    }
}

impl SqliteStore {
    fn live_facts(&self, kb: Option<&str>) -> Result<Vec<Fact>, StorageError> {
        let sql = format!(
            "SELECT {cols} FROM facts WHERE {OPEN_FACT} {kb_sql} ORDER BY created_at DESC, id",
            cols = fact_columns(""),
            kb_sql = if kb.is_some() { "AND kb = ?1" } else { "" }
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match kb {
            Some(kb) => stmt.query_map([kb], parse_fact_row_pub)?,
            None => stmt.query_map([], parse_fact_row_pub)?,
        };
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every person, one row per KB, ranked by how many live facts name them.
    fn all_people(&self, kb: Option<&str>, is_name: PersonName) -> Result<Vec<PersonRow>, StorageError> {
        let facts = self.live_facts(kb)?;
        // Who is a person, with the first spelling seen (newest fact first).
        let mut people: BTreeMap<(String, String), String> = BTreeMap::new();
        for f in &facts {
            let c = canon(f);
            if subject_is_person(f, &c, is_name) && !f.subject_norm.is_empty() {
                people
                    .entry((f.kb.clone(), f.subject_norm.clone()))
                    .or_insert_with(|| f.subject.clone());
            }
            if object_is_person(f, &c, is_name) && !f.object_norm.is_empty() {
                people
                    .entry((f.kb.clone(), f.object_norm.clone()))
                    .or_insert_with(|| f.object.clone());
            }
        }
        let mut counts: BTreeMap<(String, String), u32> = BTreeMap::new();
        for f in &facts {
            let c = canon(f);
            let mut seen_here = Vec::with_capacity(2);
            for norm in [&f.subject_norm, &f.object_norm] {
                let key = (f.kb.clone(), norm.clone());
                if people.contains_key(&key) && names(f, &c, norm) && !seen_here.contains(norm) {
                    seen_here.push(norm.clone());
                    *counts.entry(key).or_default() += 1;
                }
            }
        }
        let mut rows: Vec<PersonRow> = people
            .into_iter()
            .map(|((kb, norm), name)| PersonRow {
                facts: counts.get(&(kb.clone(), norm.clone())).copied().unwrap_or(0),
                name,
                name_norm: norm,
                kb,
            })
            .collect();
        rows.sort_by(|a, b| {
            b.facts
                .cmp(&a.facts)
                .then_with(|| a.name_norm.cmp(&b.name_norm))
                .then_with(|| a.kb.cmp(&b.kb))
        });
        Ok(rows)
    }

    /// People ranked by how many live facts name them, one row per KB, plus the
    /// total for paging.
    pub fn list_people(
        &self,
        kb: Option<&str>,
        limit: usize,
        offset: usize,
        min_facts: u32,
        is_name: PersonName,
    ) -> Result<(Vec<PersonRow>, usize), StorageError> {
        let rows: Vec<PersonRow> = self
            .all_people(kb, is_name)?
            .into_iter()
            .filter(|p| p.facts >= min_facts)
            .collect();
        let total = rows.len();
        Ok((rows.into_iter().skip(offset).take(limit).collect(), total))
    }

    /// The live facts about one person in one KB: what they are (subject) and
    /// who they are linked to (either end of a link). Links first, newest first.
    pub fn person_facts(
        &self,
        kb: &str,
        name_norm: &str,
        limit: usize,
    ) -> Result<Vec<Fact>, StorageError> {
        let mut facts: Vec<(bool, Fact)> = self
            .live_facts(Some(kb))?
            .into_iter()
            .filter_map(|f| {
                let c = canon(&f);
                names(&f, &c, name_norm).then(|| (PERSON_LINKS.contains(&c.as_str()), f))
            })
            .collect();
        // Stable: newest-first order from `live_facts` survives within each group.
        facts.sort_by_key(|(link, _)| !*link);
        Ok(facts.into_iter().map(|(_, f)| f).take(limit).collect())
    }

    /// The person a name refers to, one row per KB, best-known first: the
    /// exact name; else whoever a nickname fact in scope names; else the one
    /// person whose first name it is. A nickname or first name two people
    /// share resolves to nobody rather than to a guess.
    pub fn resolve_person(
        &self,
        kb: Option<&str>,
        name: &str,
        is_name: PersonName,
    ) -> Result<Vec<PersonRow>, StorageError> {
        let q = name.trim().to_lowercase();
        let all = self.all_people(kb, is_name)?;
        let exact: Vec<_> = all.iter().filter(|p| p.name_norm == q).cloned().collect();
        if !exact.is_empty() {
            return Ok(exact);
        }
        // Ambiguity is judged on everyone the nickname names, before anything
        // is filtered: two holders mean no answer, whoever else they are.
        let holders: BTreeSet<(String, String)> = self
            .live_facts(kb)?
            .into_iter()
            .filter(|f| f.object_norm == q && ALIAS_PREDICATES.contains(&canon(f).as_str()))
            .map(|f| (f.kb, f.subject_norm))
            .collect();
        let holder_norms: BTreeSet<&str> = holders.iter().map(|(_, n)| n.as_str()).collect();
        if holder_norms.len() > 1 {
            return Ok(Vec::new());
        }
        if holder_norms.len() == 1 {
            // The nickname must be on the same KB's row: a personal-KB
            // nickname says nothing about a work-KB namesake.
            return Ok(all.into_iter().filter(|p| holders.contains(&(p.kb.clone(), p.name_norm.clone()))).collect());
        }
        let by_first: Vec<_> = all
            .into_iter()
            .filter(|p| p.name_norm.split_whitespace().next() == Some(q.as_str()))
            .collect();
        Ok(unique_person(by_first))
    }

    /// The one subject a live nickname fact in `kb` gives this name, if
    /// exactly one: what fact writes and `state` resolve an alias to.
    pub fn nickname_subject(&self, kb: &str, alias_norm: &str) -> Result<Option<String>, StorageError> {
        let subjects: BTreeMap<String, String> = self
            .live_facts(Some(kb))?
            .into_iter()
            .filter(|f| f.object_norm == alias_norm && ALIAS_PREDICATES.contains(&canon(f).as_str()))
            .map(|f| (f.subject_norm, f.subject))
            .collect();
        Ok(if subjects.len() == 1 {
            subjects.into_values().next()
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use engram_core::types::{FactType, SourceKind};
    use uuid::Uuid;

    /// Capitalised words, like the graph crate's check for the cases here.
    fn name(v: &str) -> bool {
        v.split_whitespace().all(|w| w.chars().next().is_some_and(char::is_uppercase))
            && !v.contains("Summit")
    }

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
        let (people, total) = store.list_people(None, 10, 0, 1, &name).unwrap();
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
        let found = store.resolve_person(None, "elias", &name).unwrap();
        assert_eq!(found[0].name, "Elias");
        let memory: Uuid = store
            .conn
            .query_row("SELECT id FROM memories LIMIT 1", [], |r| r.get::<_, String>(0))
            .map(|id| Uuid::parse_str(&id).unwrap())
            .unwrap();
        fact(&store, memory, "Lira Kostova", "works_at", "Example Labs");
        fact(&store, memory, "Lira Marenova", "works_at", "Example Labs");
        // "Lira" alone is a person here, so the exact match wins.
        assert_eq!(store.resolve_person(None, "lira", &name).unwrap()[0].name, "Lira");
        fact(&store, memory, "Noemi Varkoni", "works_at", "Example Labs");
        fact(&store, memory, "Noemi Brandtke", "works_at", "Example Labs");
        assert!(store.resolve_person(None, "noemi", &name).unwrap().is_empty());
        fact(&store, memory, "Noemi Varkoni", "has_nickname", "Nomi");
        assert_eq!(store.resolve_person(None, "nomi", &name).unwrap()[0].name, "Noemi Varkoni");
        // A nickname two people share names nobody.
        fact(&store, memory, "Noemi Brandtke", "has_nickname", "Nomi");
        assert!(store.resolve_person(None, "nomi", &name).unwrap().is_empty());
        // A nickname outside the requested KB does not count.
        assert!(store.resolve_person(Some("elsewhere"), "nomi", &name).unwrap().is_empty());
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
        let (people, _) = store.list_people(None, 10, 0, 1, &name).unwrap();
        assert!(people.iter().all(|p| p.name != "Basel Summit"));
    }

    fn memory_of(store: &SqliteStore) -> Uuid {
        store
            .conn
            .query_row("SELECT id FROM memories LIMIT 1", [], |r| r.get::<_, String>(0))
            .map(|id| Uuid::parse_str(&id).unwrap())
            .unwrap()
    }

    #[test]
    fn a_predicate_alias_makes_a_person_like_its_canonical_form() {
        let store = seeded();
        let m = memory_of(&store);
        fact(&store, m, "Noemi Varkoni", "spouse_of", "Avi Roy");
        let (people, _) = store.list_people(None, 50, 0, 1, &name).unwrap();
        let names: Vec<_> = people.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"Noemi Varkoni") && names.contains(&"Avi Roy"), "{names:?}");
        assert_eq!(store.person_facts("network", "avi roy", 50).unwrap().len(), 1);
    }

    #[test]
    fn the_listed_count_is_what_show_returns() {
        let store = seeded();
        let m = memory_of(&store);
        fact(&store, m, "Elias", "met_at", "Lira");
        for p in store.list_people(None, 50, 0, 1, &name).unwrap().0 {
            assert_eq!(
                p.facts as usize,
                store.person_facts(&p.kb, &p.name_norm, 500).unwrap().len(),
                "{}",
                p.name
            );
        }
        let (busy, _) = store.list_people(None, 50, 0, 2, &name).unwrap();
        assert!(busy.iter().any(|p| p.name == "Lira"), "Lira: introduced_by + met_at");
    }

    #[test]
    fn a_nickname_shared_across_alias_spellings_is_ambiguous() {
        let store = seeded();
        let m = memory_of(&store);
        fact(&store, m, "Alice Smith", "has_nickname", "Ace");
        fact(&store, m, "Bob Jones", "also_known_as", "Ace");
        assert!(store.resolve_person(None, "ace", &name).unwrap().is_empty());
    }

    #[test]
    fn an_organisation_with_an_email_or_an_investment_is_not_a_person() {
        let store = seeded();
        let m = memory_of(&store);
        fact(&store, m, "Acme Labs", "has_email", "support@example.com");
        fact(&store, m, "Acme Capital", "investor_in", "Example Labs");
        let not_org = |v: &str| name(v) && !v.contains("Acme") && !v.contains("Labs");
        let (people, _) = store.list_people(None, 50, 0, 1, &not_org).unwrap();
        assert!(people.iter().all(|p| !p.name.contains("Acme")), "{people:?}");
    }

    #[test]
    fn paging_and_threshold() {
        let store = seeded();
        let (page, total) = store.list_people(None, 1, 1, 1, &name).unwrap();
        assert_eq!((page[0].name.as_str(), total), ("Lira", 2));
        let (busy, total) = store.list_people(Some("network"), 10, 0, 2, &name).unwrap();
        assert_eq!((busy.len(), total), (1, 1));
    }
}
