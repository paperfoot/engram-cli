//! `engram graph` — inspect SQLite-backed entity relations.
//!
//! Output is person-centric: the seed is resolved through its aliases, the
//! neighborhood comes back closest-first (BFS over the weighted relation graph
//! in `engram_graph::expand`), one row per neighbor, and every row carries the
//! predicate that links it plus the sources that prove it. `data.relations`
//! still holds the raw relation records for callers that want the edge list.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use engram_graph::expand::{expand_entity_names, expand_seeds_top_n, GraphEdge};
use engram_graph::people::KIND_PERSON;
use engram_storage::RelationRecord;
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn neighbors(
    ctx: &AppContext,
    name: String,
    kb: String,
    hops: u8,
    min_weight: f32,
    limit: usize,
    typed_only: bool,
) -> Result<(), CliError> {
    let hops = hops.clamp(1, 2);
    let seed = ctx.store.find_entity(Some(&kb), &name)?;
    let relations: Vec<RelationRecord> =
        expand_entity_names(&ctx.store, &kb, std::slice::from_ref(&name), hops)?
            .into_iter()
            .filter(|r| r.weight >= min_weight)
            .filter(|r| !typed_only || is_typed(&r.predicate))
            .collect();

    // Rank the neighborhood by graph distance from the seed using the shared
    // bounded BFS, so query-time expansion behaves the same everywhere.
    let edges: Vec<GraphEdge> = relations
        .iter()
        .map(|r| GraphEdge {
            from: r.from_entity_id,
            to: r.to_entity_id,
            weight: r.weight,
        })
        .collect();
    let seed_id = seed.as_ref().map(|s| s.id);
    let order: HashMap<uuid::Uuid, usize> = match seed_id {
        Some(id) => expand_seeds_top_n(&[id], &edges, hops, limit.saturating_add(1))
            .into_iter()
            .enumerate()
            .map(|(rank, id)| (id, rank))
            .collect(),
        None => HashMap::new(),
    };

    // Collapse edges into one row per neighbor, keeping every predicate that
    // ties them into the seed's neighborhood.
    let mut nodes: Vec<(usize, Value)> = Vec::new();
    let mut index: HashMap<uuid::Uuid, usize> = HashMap::new();
    let mut kinds: HashMap<uuid::Uuid, String> = HashMap::new();
    for relation in &relations {
        for (other_id, other_name) in [
            (relation.to_entity_id, relation.to_entity.as_str()),
            (relation.from_entity_id, relation.from_entity.as_str()),
        ] {
            if seed_id == Some(other_id) {
                continue;
            }
            let entry = json!({
                "predicate": relation.predicate,
                "subject": relation.from_entity,
                "object": relation.to_entity,
                "direction": if seed_id == Some(relation.from_entity_id) {
                    "outgoing"
                } else if seed_id == Some(relation.to_entity_id) {
                    "incoming"
                } else {
                    "indirect"
                },
                "weight": relation.weight,
                "sources": sources_of(relation),
            });
            match index.get(&other_id) {
                Some(pos) => {
                    if let Some(list) = nodes[*pos].1["relations"].as_array_mut() {
                        list.push(entry);
                    }
                }
                None => {
                    let kind = match kinds.get(&other_id) {
                        Some(kind) => kind.clone(),
                        None => {
                            let kind = ctx
                                .store
                                .find_entity(Some(&kb), other_name)?
                                .map(|e| e.kind)
                                .unwrap_or_else(|| "concept".to_string());
                            kinds.insert(other_id, kind.clone());
                            kind
                        }
                    };
                    index.insert(other_id, nodes.len());
                    nodes.push((
                        order.get(&other_id).copied().unwrap_or(usize::MAX),
                        json!({
                            "name": other_name,
                            "kind": kind,
                            "relations": [entry],
                        }),
                    ));
                }
            }
        }
    }
    nodes.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            a.1["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b.1["name"].as_str().unwrap_or(""))
        })
    });
    let neighbors: Vec<Value> = nodes
        .into_iter()
        .map(|(_, node)| node)
        .take(limit)
        .collect();
    let people = neighbors
        .iter()
        .filter(|node| node["kind"] == KIND_PERSON)
        .count();

    let mut meta = Metadata::default();
    meta.add("kb", kb.clone());
    meta.add("relations", relations.len());
    meta.add("neighbors", neighbors.len());
    meta.add("people", people);
    meta.add("hops", hops);
    meta.add("min_weight", min_weight);
    print_success(
        ctx.format,
        json!({
            "kb": kb,
            "entity": name,
            "resolved": seed.as_ref().map(|e| json!({
                "name": e.canonical_name,
                "kind": e.kind,
                "aliases": e.aliases,
                "mentions": e.mention_count,
            })),
            "hops": hops,
            "min_weight": min_weight,
            "neighbors": neighbors,
            "relations": relations,
        }),
        meta,
        |data| {
            let entity = data["resolved"]["name"]
                .as_str()
                .unwrap_or_else(|| data["entity"].as_str().unwrap_or(""));
            let kind = data["resolved"]["kind"].as_str().unwrap_or("unknown");
            println!("{entity}  ({kind})");
            let Some(list) = data["neighbors"].as_array() else {
                return;
            };
            if list.is_empty() {
                println!("(no neighbors)");
                return;
            }
            for node in list {
                println!(
                    "  {}  [{}]",
                    node["name"].as_str().unwrap_or(""),
                    node["kind"].as_str().unwrap_or("")
                );
                for relation in node["relations"].as_array().into_iter().flatten() {
                    println!(
                        "      {} — {} -> {}",
                        relation["subject"].as_str().unwrap_or(""),
                        relation["predicate"].as_str().unwrap_or(""),
                        relation["object"].as_str().unwrap_or("")
                    );
                }
            }
        },
    );
    Ok(())
}

/// Typed relations carry meaning ("introduced_by"); co-occurrence is only a
/// weak "these appeared together" signal.
fn is_typed(predicate: &str) -> bool {
    !matches!(predicate, "co_occurs_with" | "related_to")
}

/// Provenance as a list, tolerating the flat single-source shape written by
/// the co-occurrence path.
fn sources_of(relation: &RelationRecord) -> Value {
    relation
        .provenance
        .get("sources")
        .cloned()
        .unwrap_or_else(|| match &relation.provenance {
            Value::Null => json!([]),
            other => json!([other]),
        })
}
