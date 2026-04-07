//! Memory layers (L0–L3) — tiered context loading inspired by MemPalace.
//!
//! L0 = identity (static)
//! L1 = critical facts (AAAK-compressed)
//! L2 = topic context (loaded on demand)
//! L3 = deep search

use crate::types::Layer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerBudget {
    pub layer: Layer,
    pub max_tokens: usize,
    pub include_compressed: bool,
}

impl Default for LayerBudget {
    fn default() -> Self {
        Self {
            layer: Layer::Topic,
            max_tokens: Layer::Topic.default_token_budget(),
            include_compressed: false,
        }
    }
}

/// Approximate token count using a 4-chars-per-token heuristic.
/// Replaced with a tokenizer when one is wired up.
pub fn approx_tokens(text: &str) -> usize {
    (text.len() + 3) / 4
}

/// Trim a payload to fit within a token budget. Greedy: keeps highest-ranked
/// items first.
pub fn fit_to_budget<'a, I>(items: I, budget: usize) -> Vec<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut used = 0usize;
    let mut out = Vec::new();
    for item in items {
        let cost = approx_tokens(item);
        if used + cost > budget {
            break;
        }
        used += cost;
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_roughly_chars_div_four() {
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens("abcdefgh"), 2);
        assert_eq!(approx_tokens(""), 0);
    }

    #[test]
    fn budget_is_respected() {
        let items = vec!["aaaa", "bbbb", "cccc", "dddd"]; // 1 token each
        let kept = fit_to_budget(items.iter().copied(), 2);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn default_budgets_match_spec() {
        assert_eq!(Layer::Identity.default_token_budget(), 50);
        assert_eq!(Layer::Critical.default_token_budget(), 120);
    }
}
