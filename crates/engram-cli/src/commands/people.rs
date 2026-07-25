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

/// `engram people show <name> [--kb '*']`
///
/// One person usually exists once PER KB — the compiler builds entity rows per
/// namespace — so an unscoped lookup has to merge them. It used to call
/// `find_entity(None, …)`, whose SQL is `LIMIT 1` with no ORDER BY: SQLite
/// handed back an arbitrary KB's row and every other KB's aliases, mentions,
/// relations and memories were silently dropped, which is the opposite of what
/// `--kb '*'` asks for. `find_entities` returns one row per KB, so this now
/// unions the aliases, sums the mention counts, and keeps relations and
/// snippets grouped by their own KB (provenance survives the merge).
pub fn show(ctx: &AppContext, name: String, kb: String, snippets: usize) -> Result<(), CliError> {
    let kb_filter = if kb == "*" { None } else { Some(kb.as_str()) };
    // One row per KB. The cap is generous: a name is expected in a handful of
    // KBs, never hundreds.
    let matches = ctx.store.find_entities(kb_filter, &name, 64)?;
    let Some(primary) = matches.first().cloned() else {
        let scope = if kb == "*" {
            "any kb".to_string()
        } else {
            format!("kb {kb:?}")
        };
        return Err(CliError::BadInput(format!(
            "no entity named {name:?} in {scope} — run `engram people list` to browse"
        )));
    };

    // Per-KB views, in the order find_entities ranked them (best-mentioned
    // first), so the merged output leads with the KB that knows this person best.
    let mut per_kb: Vec<(EntityRecord, Vec<Edge>)> = Vec::with_capacity(matches.len());
    for person in matches {
        let relations = ctx
            .store
            .relations_for_entity(&person.kb, person.id, SHOW_RELATIONS)?;
        let ranked = rank_relations(&person.canonical_name, relations);
        per_kb.push((person, ranked));
    }

    // Union of aliases (and of the other KBs' canonical spellings, which are
    // aliases from the merged person's point of view), case-insensitively
    // deduped against the primary name.
    let mut aliases: Vec<String> = Vec::new();
    let mut seen_alias: Vec<String> = vec![primary.canonical_name.to_ascii_lowercase()];
    let push_alias = |candidate: &str, out: &mut Vec<String>, seen: &mut Vec<String>| {
        let key = candidate.to_ascii_lowercase();
        if candidate.trim().is_empty() || seen.contains(&key) {
            return;
        }
        seen.push(key);
        out.push(candidate.to_string());
    };
    for (person, _) in &per_kb {
        push_alias(&person.canonical_name, &mut aliases, &mut seen_alias);
        for alias in &person.aliases {
            push_alias(alias, &mut aliases, &mut seen_alias);
        }
    }

    let mentions: u32 = per_kb.iter().map(|(p, _)| p.mention_count).sum();
    let relation_total: usize = per_kb.iter().map(|(_, r)| r.len()).sum();
    let kbs: Vec<String> = per_kb.iter().map(|(p, _)| p.kb.clone()).collect();

    // Flat relation list (each edge labelled with its KB) plus the per-KB
    // grouping, so a caller can either scan everything or stay inside one
    // namespace.
    let mut flat_relations: Vec<Value> = Vec::with_capacity(relation_total);
    let mut relations_by_kb: Vec<Value> = Vec::with_capacity(per_kb.len());
    let mut memories: Vec<Value> = Vec::new();
    for (person, ranked) in &per_kb {
        let mut group: Vec<Value> = Vec::with_capacity(ranked.len());
        for edge in ranked {
            let mut v = edge_json(edge);
            v["kb"] = json!(person.kb);
            group.push(v.clone());
            flat_relations.push(v);
        }
        relations_by_kb.push(json!({
            "kb": person.kb,
            "entity_id": person.id.to_string(),
            "mentions": person.mention_count,
            "relations": group,
        }));
        // Snippets come from every KB, each tagged, instead of only whichever
        // row SQLite happened to return first.
        for mut snippet in memory_snippets(ctx, person, ranked, snippets)? {
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
    meta.add("kind", primary.kind.clone());
    let payload = json!({
        "kb": kb_label,
        "kbs": kbs,
        "person": {
            "name": primary.canonical_name,
            "kind": primary.kind,
            "aliases": aliases,
            "mentions": mentions,
            "entity_id": primary.id.to_string(),
        },
        "relations": flat_relations,
        "relations_by_kb": relations_by_kb,
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
        let groups = data["relations_by_kb"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let total: usize = groups
            .iter()
            .map(|g| g["relations"].as_array().map(|a| a.len()).unwrap_or(0))
            .sum();
        if total == 0 {
            println!("Relations: (none yet)");
        } else {
            for group in &groups {
                let relations = group["relations"].as_array().cloned().unwrap_or_default();
                if relations.is_empty() {
                    continue;
                }
                println!("Relations (kb={}):", group["kb"].as_str().unwrap_or(""));
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
