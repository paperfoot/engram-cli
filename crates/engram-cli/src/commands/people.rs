//! `engram people list|show` — who do I know, and how are they connected?
//!
//! A thin view over live facts (`engram_storage::people`): a person is a name
//! the facts treat as one, and every relation printed is a fact with its
//! source memory, so "Noemi — introduced_by → Avi Roy" can always be checked
//! against the sentence that produced it and corrected with the fact commands.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Listing, Metadata, Scope};
use crate::read_surface::{self, CursorKey, Provenance};
use engram_core::handle::HandleKind;
use engram_core::relations::canonical_relation;
use engram_core::types::Fact;
use engram_storage::people::{ALIAS_PREDICATES, PERSON_LINKS};
use engram_storage::PersonRow;
use serde_json::{json, Value};

/// Relations per person in the list view.
const LIST_RELATIONS: usize = 5;
/// Every fact about a person: the count, the aliases and the page agree.
const ALL: usize = usize::MAX;

/// Words that make a name an organisation's, whatever its shape.
const ORGANISATION_WORDS: &[&str] = &[
    "ag", "ai", "bank", "capital", "clinic", "co", "company", "corp", "corporation", "foundation",
    "fund", "gmbh", "group", "holdings", "hospital", "inc", "institute", "lab", "labs", "llc",
    "ltd", "partners", "pte", "studio", "studios", "technologies", "tech", "university",
    "ventures",
];

/// Reads as a person's name: the graph crate's check, minus organisation words.
fn person_name(name: &str) -> bool {
    engram_graph::people::looks_like_person_name(name)
        && !name
            .split(|c: char| !c.is_alphanumeric())
            .any(|w| ORGANISATION_WORDS.contains(&w.to_lowercase().as_str()))
}

pub fn list(
    ctx: &AppContext,
    kb: String,
    limit: usize,
    min_mentions: u32,
    cursor: Option<String>,
) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let key = CursorKey::new(
        "people.list",
        &[
            ("kb", kb_filter),
            ("min_mentions", Some(&min_mentions.to_string())),
        ],
    );
    let offset = match &cursor {
        Some(raw) => read_surface::decode_cursor(raw, &key)?,
        None => 0,
    };
    let (people, total) = ctx
        .store
        .list_people(kb_filter, limit, offset, min_mentions, &person_name)?;

    let mut rows = Vec::with_capacity(people.len());
    for person in &people {
        let facts = ctx
            .store
            .person_facts(&person.kb, &person.name_norm, ALL)?;
        let edges = edges(person, &facts);
        rows.push(json!({
            "name": person.name,
            "kb": person.kb,
            "aliases": aliases(person, &facts),
            "mentions": person.facts,
            "relation_count": edges.len(),
            "top_relations": edges.iter().take(LIST_RELATIONS).map(edge_json).collect::<Vec<_>>(),
        }));
    }

    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("people", rows.len());
    Scope::undiaried(
        vec![kb.clone()],
        if kb_filter.is_some() {
            "flag"
        } else {
            "all_kbs"
        },
    )
    .attach(&mut meta);
    let Value::Object(mut payload) = json!({ "kb": kb, "people": rows }) else {
        unreachable!("json! object literal");
    };
    Listing::page(
        people.len(),
        total as i64,
        read_surface::next_cursor(&key, offset, people.len(), total as i64),
    )
    .attach(&mut payload);
    print_success(ctx.format, Value::Object(payload), meta, |data| {
        let Some(arr) = data.get("people").and_then(|v| v.as_array()) else {
            return;
        };
        if arr.is_empty() {
            println!("(no people yet — they appear as facts about them are remembered)");
            return;
        }
        for person in arr {
            let name = person["name"].as_str().unwrap_or("");
            let mentions = person["mentions"].as_u64().unwrap_or(0);
            let aliases = person["aliases"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            if aliases.is_empty() {
                println!("{mentions:5}  {name}");
            } else {
                println!("{mentions:5}  {name}  (aka {aliases})");
            }
            for edge in person["top_relations"].as_array().into_iter().flatten() {
                println!(
                    "         {} -> {}",
                    edge["predicate"].as_str().unwrap_or(""),
                    edge["other"].as_str().unwrap_or("")
                );
            }
        }
        println!();
        println!(
            "{} of {} people",
            arr.len(),
            data["total"].as_i64().unwrap_or(arr.len() as i64)
        );
    });
    Ok(())
}

/// `engram people show <name> [--kb '*']`
///
/// One name can be a person in several KBs. An unscoped lookup merges them:
/// aliases are unioned, fact counts summed, and relations and snippets stay
/// grouped by their own KB so provenance survives the merge.
pub fn show(ctx: &AppContext, name: String, kb: String, snippets: usize) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let matches = ctx
        .store
        .resolve_person(kb_filter, &name, &person_name)?;
    let Some(primary) = matches.first().cloned() else {
        let scope = if kb == "*" {
            "any kb".to_string()
        } else {
            format!("kb {kb:?}")
        };
        return Err(CliError::BadInput(format!(
            "no person named {name:?} in {scope} — run `engram people list` to browse"
        )));
    };

    let mut per_kb: Vec<(PersonRow, Vec<Fact>)> = Vec::with_capacity(matches.len());
    for person in matches {
        let facts = ctx
            .store
            .person_facts(&person.kb, &person.name_norm, ALL)?;
        per_kb.push((person, facts));
    }

    let mut all_aliases: Vec<String> = Vec::new();
    for (person, facts) in &per_kb {
        for alias in aliases(person, facts) {
            if !all_aliases.iter().any(|a| a.eq_ignore_ascii_case(&alias)) {
                all_aliases.push(alias);
            }
        }
    }
    let mentions: u32 = per_kb.iter().map(|(p, _)| p.facts).sum();
    let kbs: Vec<String> = per_kb.iter().map(|(p, _)| p.kb.clone()).collect();

    let mut flat_relations: Vec<Value> = Vec::new();
    let mut relations_by_kb: Vec<Value> = Vec::with_capacity(per_kb.len());
    let mut memories: Vec<Value> = Vec::new();
    for (person, facts) in &per_kb {
        let edges = edges(person, facts);
        let mut group: Vec<Value> = Vec::with_capacity(edges.len());
        for edge in &edges {
            let mut v = edge_json(edge);
            v["kb"] = json!(person.kb);
            group.push(v.clone());
            flat_relations.push(v);
        }
        relations_by_kb.push(json!({
            "kb": person.kb,
            "mentions": person.facts,
            "relations": group,
        }));
        for mut snippet in memory_snippets(ctx, person, facts, snippets)? {
            snippet["kb"] = json!(person.kb);
            memories.push(snippet);
        }
    }

    let kb_label = if kb_filter.is_some() {
        primary.kb.clone()
    } else {
        "*".to_string()
    };
    let mut meta = Metadata::default();
    meta.add("kb", kb_label.clone());
    meta.add("kbs", kbs.clone());
    meta.add("relations", flat_relations.len());
    let payload = json!({
        "kb": kb_label,
        "kbs": kbs,
        "person": {
            "name": primary.name,
            "aliases": all_aliases,
            "mentions": mentions,
        },
        "relations": flat_relations,
        "relations_by_kb": relations_by_kb,
        "memories": memories,
    });

    print_success(ctx.format, payload, meta, |data| {
        let person = &data["person"];
        println!(
            "{}  ({} facts)",
            person["name"].as_str().unwrap_or(""),
            person["mentions"].as_u64().unwrap_or(0)
        );
        if let Some(aliases) = person["aliases"].as_array() {
            if !aliases.is_empty() {
                println!(
                    "Also known as: {}",
                    aliases
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        if let Some(kbs) = data["kbs"].as_array() {
            if kbs.len() > 1 {
                println!(
                    "Known in: {}",
                    kbs.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        println!();
        for group in data["relations_by_kb"].as_array().into_iter().flatten() {
            let relations = group["relations"].as_array().cloned().unwrap_or_default();
            if relations.is_empty() {
                continue;
            }
            println!("Relations (kb={}):", group["kb"].as_str().unwrap_or(""));
            for edge in &relations {
                println!(
                    "  {} — {} -> {}  [{}]",
                    edge["subject"].as_str().unwrap_or(""),
                    edge["predicate"].as_str().unwrap_or(""),
                    edge["object"].as_str().unwrap_or(""),
                    edge["handle"].as_str().unwrap_or("")
                );
            }
        }
        let memories = data["memories"].as_array().cloned().unwrap_or_default();
        if !memories.is_empty() {
            println!();
            println!("Memories:");
            for memory in &memories {
                println!(
                    "  - {} [memory:{}]",
                    memory["snippet"].as_str().unwrap_or(""),
                    memory["memory_id"].as_str().unwrap_or("")
                );
            }
        }
    });
    Ok(())
}

/// One fact as seen FROM the person being displayed.
struct Edge<'a> {
    fact: &'a Fact,
    /// The other end: a person for a link, a value for an attribute.
    other: &'a str,
    /// `outgoing` when the person is the subject, `incoming` otherwise.
    direction: &'static str,
}

/// Links to other people first, then what the facts say about the person;
/// newest first within each (the order `person_facts` returns). Alias facts
/// are reported as aliases, not relations.
fn edges<'a>(person: &PersonRow, facts: &'a [Fact]) -> Vec<Edge<'a>> {
    facts
        .iter()
        .filter(|f| !ALIAS_PREDICATES.contains(&canonical_relation(&f.predicate).as_str()))
        .map(|fact| {
            let outgoing = fact.subject_norm == person.name_norm;
            Edge {
                fact,
                other: if outgoing { &fact.object } else { &fact.subject },
                direction: if outgoing { "outgoing" } else { "incoming" },
            }
        })
        .collect()
}

fn aliases(person: &PersonRow, facts: &[Fact]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in facts {
        if f.subject_norm == person.name_norm
            && ALIAS_PREDICATES.contains(&canonical_relation(&f.predicate).as_str())
            && !f.object.eq_ignore_ascii_case(&person.name)
            && !out.iter().any(|a| a.eq_ignore_ascii_case(&f.object))
        {
            out.push(f.object.clone());
        }
    }
    out
}

fn edge_json(edge: &Edge) -> Value {
    json!({
        "subject": edge.fact.subject,
        "predicate": edge.fact.predicate,
        "object": edge.fact.object,
        "other": edge.other,
        "direction": edge.direction,
        "link": PERSON_LINKS.contains(&canonical_relation(&edge.fact.predicate).as_str()),
        "confidence": edge.fact.confidence,
        "fact_id": edge.fact.id.to_string(),
        "handle": read_surface::handle(HandleKind::Fact, edge.fact.id),
        "sources": [{ "memory_id": edge.fact.source_memory_id.to_string() }],
    })
}

/// Memory snippets for a person: the memories their facts came from first
/// (the ones worth re-reading), then any other memory that names them.
fn memory_snippets(
    ctx: &AppContext,
    person: &PersonRow,
    facts: &[Fact],
    limit: usize,
) -> Result<Vec<Value>, CliError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<uuid::Uuid> = Vec::new();
    for fact in facts {
        if out.len() >= limit {
            break;
        }
        if seen.contains(&fact.source_memory_id) {
            continue;
        }
        if let Some(chunk) = ctx
            .store
            .chunks_for_memory(fact.source_memory_id)?
            .into_iter()
            .next()
        {
            seen.push(chunk.memory_id);
            out.push(chunk_json(chunk));
        }
    }
    if out.len() < limit {
        for chunk in ctx
            .store
            .chunks_containing(&person.kb, &person.name, limit * 2)?
        {
            if out.len() >= limit {
                break;
            }
            if !seen.contains(&chunk.memory_id) {
                seen.push(chunk.memory_id);
                out.push(chunk_json(chunk));
            }
        }
    }
    out.truncate(limit);
    Ok(out)
}

fn chunk_json(chunk: engram_storage::ChunkRecord) -> Value {
    json!({
        "memory_id": chunk.memory_id.to_string(),
        "chunk_id": chunk.chunk_id.to_string(),
        // The same handle and chain every other surface publishes, so a
        // caller can hand this row to `edit` without a second lookup.
        "handle": read_surface::handle(HandleKind::Memory, chunk.memory_id),
        "provenance": Provenance {
            chunk_id: Some(chunk.chunk_id),
            memory_id: Some(chunk.memory_id),
            document_id: chunk.document_id,
            kb: Some(chunk.kb.clone()),
            diary: Some(chunk.diary.clone()),
            ..Default::default()
        }.to_json(),
        "source": chunk.source,
        "snippet": snippet(&chunk.content),
    })
}

fn snippet(content: &str) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 240 {
        return flat;
    }
    let mut out: String = flat.chars().take(240).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use engram_core::types::{FactType, SourceKind};
    use uuid::Uuid;

    fn fact(s: &str, p: &str, o: &str) -> Fact {
        Fact {
            id: Uuid::new_v4(),
            source_memory_id: Uuid::new_v4(),
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
            kb: "default".into(),
        }
    }

    fn person(name: &str) -> PersonRow {
        PersonRow {
            name: name.into(),
            name_norm: name.to_lowercase(),
            kb: "default".into(),
            facts: 0,
        }
    }

    #[test]
    fn direction_is_relative_to_the_person_shown() {
        let facts = vec![fact("Noemi", "friend_of", "Avi Roy")];
        let edges = edges(&person("Avi Roy"), &facts);
        assert_eq!(edges[0].direction, "incoming");
        assert_eq!(edges[0].other, "Noemi");
    }

    #[test]
    fn nicknames_are_aliases_not_relations() {
        let facts = vec![
            fact("Noemi", "has_nickname", "Nomi"),
            fact("Noemi", "works_at", "Example Labs"),
        ];
        let p = person("Noemi");
        assert_eq!(aliases(&p, &facts), vec!["Nomi".to_string()]);
        let edges = edges(&p, &facts);
        assert_eq!(edges.len(), 1);
        assert_eq!(edge_json(&edges[0])["sources"][0]["memory_id"], facts[1].source_memory_id.to_string());
    }
}
