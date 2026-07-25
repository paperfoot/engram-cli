//! `engram people list|show` — the networking view over the entity graph.
//!
//! `entities list` answers "what is in this KB". These commands answer the
//! question a networker actually asks: *who* do I know, and *how* are they
//! connected? Every relation is printed with its predicate AND the memory it
//! came from, so an answer like "Noemi — introduced_by → Avi Roy" can always
//! be checked against the sentence that produced it.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_storage::{EntityRecord, RelationRecord};
use serde_json::{json, Value};

/// Relations per person in the list view.
const LIST_RELATIONS: usize = 5;
/// Upper bound on relations pulled for one person in the show view.
const SHOW_RELATIONS: usize = 200;

pub fn list(ctx: &AppContext, kb: String, limit: usize, min_mentions: u32) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let people = ctx.store.list_entities_by_kind(
        kb_filter,
        engram_graph::people::KIND_PERSON,
        limit,
        min_mentions,
    )?;

    let mut rows = Vec::with_capacity(people.len());
    for person in &people {
        let relations = ctx
            .store
            .relations_for_entity(&person.kb, person.id, SHOW_RELATIONS)?;
        let ranked = rank_relations(&person.canonical_name, relations);
        rows.push(json!({
            "name": person.canonical_name,
            "kb": person.kb,
            "aliases": person.aliases,
            "mentions": person.mention_count,
            "relation_count": ranked.len(),
            "top_relations": ranked
                .iter()
                .take(LIST_RELATIONS)
                .map(edge_json)
                .collect::<Vec<_>>(),
        }));
    }

    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("people", rows.len());
    print_success(
        ctx.format,
        json!({ "kb": kb, "people": rows }),
        meta,
        |data| {
            let Some(arr) = data.get("people").and_then(|v| v.as_array()) else {
                return;
            };
            if arr.is_empty() {
                println!("(no people yet — run `engram compile --kb <kb> --all`)");
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
            println!("{} people", arr.len());
        },
    );
    Ok(())
}

pub fn show(ctx: &AppContext, name: String, kb: String, snippets: usize) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    let Some(person) = ctx.store.find_entity(kb_filter, &name)? else {
        return Err(CliError::BadInput(format!(
            "no entity named {name:?} in kb {kb:?} — run `engram people list --kb {kb}`"
        )));
    };
    let relations = ctx
        .store
        .relations_for_entity(&person.kb, person.id, SHOW_RELATIONS)?;
    let ranked = rank_relations(&person.canonical_name, relations);
    let memories = memory_snippets(ctx, &person, &ranked, snippets)?;

    let mut meta = Metadata::default();
    meta.add("kb", person.kb.clone());
    meta.add("relations", ranked.len());
    meta.add("kind", person.kind.clone());
    let payload = json!({
        "kb": person.kb,
        "person": {
            "name": person.canonical_name,
            "kind": person.kind,
            "aliases": person.aliases,
            "mentions": person.mention_count,
            "entity_id": person.id.to_string(),
        },
        "relations": ranked.iter().map(edge_json).collect::<Vec<_>>(),
        "memories": memories,
    });

    print_success(ctx.format, payload, meta, |data| {
        let person = &data["person"];
        println!(
            "{}  ({}, {} mentions)",
            person["name"].as_str().unwrap_or(""),
            person["kind"].as_str().unwrap_or(""),
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
        println!();
        let relations = data["relations"].as_array().cloned().unwrap_or_default();
        if relations.is_empty() {
            println!("Relations: (none yet)");
        } else {
            println!("Relations:");
            for edge in &relations {
                let source = edge["sources"]
                    .as_array()
                    .and_then(|s| s.first())
                    .and_then(|s| s.get("memory_id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unsourced");
                println!(
                    "  {} — {} -> {}, source: {}",
                    edge["subject"].as_str().unwrap_or(""),
                    edge["predicate"].as_str().unwrap_or(""),
                    edge["object"].as_str().unwrap_or(""),
                    source
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

/// One relation as seen FROM the person being displayed.
struct Edge {
    record: RelationRecord,
    /// The other endpoint's display name.
    other: String,
    /// `outgoing` when the person is the subject, `incoming` otherwise.
    direction: &'static str,
}

/// Order relations so the networking-relevant ones come first: typed edges
/// (introduced_by, friend_of, …) before the weak `co_occurs_with` fallback,
/// then by weight.
fn rank_relations(person: &str, relations: Vec<RelationRecord>) -> Vec<Edge> {
    let mut edges: Vec<Edge> = relations
        .into_iter()
        .map(|record| {
            let outgoing = record.from_entity.eq_ignore_ascii_case(person);
            let other = if outgoing {
                record.to_entity.clone()
            } else {
                record.from_entity.clone()
            };
            Edge {
                record,
                other,
                direction: if outgoing { "outgoing" } else { "incoming" },
            }
        })
        .collect();
    edges.sort_by(|a, b| {
        typed_rank(&a.record.predicate)
            .cmp(&typed_rank(&b.record.predicate))
            .then_with(|| {
                b.record
                    .weight
                    .partial_cmp(&a.record.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.other.cmp(&b.other))
    });
    edges
}

/// 0 = typed relation, 1 = generic co-occurrence.
fn typed_rank(predicate: &str) -> u8 {
    match predicate {
        "co_occurs_with" | "related_to" => 1,
        _ => 0,
    }
}

fn edge_json(edge: &Edge) -> Value {
    let sources = edge
        .record
        .provenance
        .get("sources")
        .cloned()
        .unwrap_or_else(|| match &edge.record.provenance {
            Value::Null => json!([]),
            other => json!([other]),
        });
    json!({
        "subject": edge.record.from_entity,
        "predicate": edge.record.predicate,
        "object": edge.record.to_entity,
        "other": edge.other,
        "direction": edge.direction,
        "weight": edge.record.weight,
        "sources": sources,
    })
}

/// Memory snippets for a person: the memories that produced their relations
/// first (they are the ones a networker wants to re-read), then any other
/// memory that names them or one of their aliases.
fn memory_snippets(
    ctx: &AppContext,
    person: &EntityRecord,
    edges: &[Edge],
    limit: usize,
) -> Result<Vec<Value>, CliError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut out: Vec<Value> = Vec::new();
    let mut seen: Vec<uuid::Uuid> = Vec::new();

    let mut push_chunk = |chunk: engram_storage::ChunkRecord, out: &mut Vec<Value>| {
        if seen.contains(&chunk.chunk_id) {
            return;
        }
        seen.push(chunk.chunk_id);
        out.push(json!({
            "memory_id": chunk.memory_id.to_string(),
            "chunk_id": chunk.chunk_id.to_string(),
            "source": chunk.source,
            "snippet": snippet(&chunk.content),
        }));
    };

    for edge in edges {
        if out.len() >= limit {
            break;
        }
        for source in edge
            .record
            .provenance
            .get("sources")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
        {
            let Some(chunk_id) = source
                .get("chunk_id")
                .and_then(|v| v.as_str())
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
            else {
                continue;
            };
            if let Some(chunk) = ctx.store.get_chunk_record(chunk_id)? {
                push_chunk(chunk, &mut out);
            }
            if out.len() >= limit {
                break;
            }
        }
    }

    if out.len() < limit {
        let mut names = vec![person.canonical_name.clone()];
        names.extend(person.aliases.iter().cloned());
        for needle in names {
            if out.len() >= limit {
                break;
            }
            for chunk in ctx
                .store
                .chunks_containing(&person.kb, &needle, limit * 2)?
            {
                push_chunk(chunk, &mut out);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out.truncate(limit);
    Ok(out)
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
    use uuid::Uuid;

    fn relation(from: &str, to: &str, predicate: &str, weight: f32) -> RelationRecord {
        RelationRecord {
            id: Uuid::new_v4(),
            kb: "default".into(),
            from_entity: from.into(),
            to_entity: to.into(),
            predicate: predicate.into(),
            weight,
            provenance: json!({ "sources": [{ "memory_id": "m1" }] }),
            from_entity_id: Uuid::new_v4(),
            to_entity_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn typed_relations_outrank_co_occurrence() {
        let ranked = rank_relations(
            "Noemi",
            vec![
                relation("Noemi", "Rapamycin", "co_occurs_with", 9.0),
                relation("Noemi", "Avi Roy", "introduced_by", 1.0),
            ],
        );
        assert_eq!(ranked[0].record.predicate, "introduced_by");
        assert_eq!(ranked[1].record.predicate, "co_occurs_with");
    }

    #[test]
    fn direction_is_relative_to_the_person_shown() {
        let ranked = rank_relations(
            "Avi Roy",
            vec![relation("Noemi", "Avi Roy", "friend_of", 1.0)],
        );
        assert_eq!(ranked[0].direction, "incoming");
        assert_eq!(ranked[0].other, "Noemi");
    }

    #[test]
    fn legacy_flat_provenance_still_lists_a_source() {
        let mut record = relation("Noemi", "Avi Roy", "co_occurs_with", 1.0);
        record.provenance = json!({ "chunk_id": "c1" });
        let ranked = rank_relations("Noemi", vec![record]);
        let value = edge_json(&ranked[0]);
        assert_eq!(value["sources"].as_array().map(|a| a.len()), Some(1));
    }
}
