//! Query-time graph expansion helpers.

use engram_storage::{RelationRecord, SqliteStore, StorageError};
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdge {
    pub from: Uuid,
    pub to: Uuid,
    pub weight: f32,
}

/// Expand seed node IDs over weighted edges, returning seeds first and then
/// reachable nodes in breadth-first order. Edges are treated as undirected
/// because engram's extracted relations are evidence links, not a strict
/// ontology traversal contract.
pub fn expand_seeds(seeds: &[Uuid], edges: &[GraphEdge], max_hops: u8) -> Vec<Uuid> {
    expand_seeds_top_n(seeds, edges, max_hops, usize::MAX)
}

/// Same as `expand_seeds`, but stops after `max_nodes` unique IDs. This is
/// the query-time guard used by callers that want bounded graph fan-out.
pub fn expand_seeds_top_n(
    seeds: &[Uuid],
    edges: &[GraphEdge],
    max_hops: u8,
    max_nodes: usize,
) -> Vec<Uuid> {
    if max_nodes == 0 {
        return Vec::new();
    }

    let mut adjacency: HashMap<Uuid, Vec<(Uuid, f32)>> = HashMap::new();
    for edge in edges {
        adjacency
            .entry(edge.from)
            .or_default()
            .push((edge.to, edge.weight));
        adjacency
            .entry(edge.to)
            .or_default()
            .push((edge.from, edge.weight));
    }
    for neighbors in adjacency.values_mut() {
        neighbors.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    }

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    for seed in seeds {
        if seen.insert(*seed) {
            out.push(*seed);
            queue.push_back((*seed, 0u8));
            if out.len() >= max_nodes {
                return out;
            }
        }
    }

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_hops {
            continue;
        }
        for (neighbor, _weight) in adjacency.get(&node).into_iter().flatten() {
            if seen.insert(*neighbor) {
                out.push(*neighbor);
                if out.len() >= max_nodes {
                    return out;
                }
                queue.push_back((*neighbor, depth + 1));
            }
        }
    }
    out
}

/// Expand named entities through the SQLite graph layer and return relation
/// evidence. This keeps the public graph crate connected to the real store
/// implementation instead of duplicating SQL in the CLI.
pub fn expand_entity_names(
    store: &SqliteStore,
    kb: &str,
    seeds: &[String],
    max_hops: u8,
) -> Result<Vec<RelationRecord>, StorageError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for seed in seeds {
        for rel in store.graph_neighbors(kb, seed, max_hops)? {
            if seen.insert(rel.id) {
                out.push(rel);
            }
        }
    }
    out.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.from_entity.cmp(&b.from_entity))
            .then_with(|| a.to_entity.cmp(&b.to_entity))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_seeds_walks_weighted_edges_by_hop() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let d = Uuid::new_v4();
        let edges = vec![
            GraphEdge {
                from: a,
                to: b,
                weight: 0.2,
            },
            GraphEdge {
                from: a,
                to: c,
                weight: 0.9,
            },
            GraphEdge {
                from: c,
                to: d,
                weight: 0.8,
            },
        ];
        assert_eq!(expand_seeds(&[a], &edges, 1), vec![a, c, b]);
        assert_eq!(expand_seeds(&[a], &edges, 2), vec![a, c, b, d]);
    }

    #[test]
    fn expand_seeds_top_n_bounds_fanout() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let edges = vec![
            GraphEdge {
                from: a,
                to: b,
                weight: 0.1,
            },
            GraphEdge {
                from: a,
                to: c,
                weight: 0.9,
            },
        ];
        assert_eq!(expand_seeds_top_n(&[a], &edges, 1, 2), vec![a, c]);
    }
}
