//! `engram usage` — summarize API/search usage recorded by cloud paths.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub const GEMINI_EMBED_USD_PER_MILLION_TOKENS: f64 = 0.15;

pub fn estimated_tokens(text: &str) -> i64 {
    text.len().div_ceil(4) as i64
}

pub fn estimated_tokens_for_texts(texts: &[&str]) -> i64 {
    texts.iter().map(|t| estimated_tokens(t)).sum()
}

pub fn gemini_embed_cost_usd(tokens: i64) -> f64 {
    tokens as f64 * GEMINI_EMBED_USD_PER_MILLION_TOKENS / 1_000_000.0
}

pub fn cohere_rerank_cost_usd(search_units: f64) -> f64 {
    std::env::var("ENGRAM_COHERE_RERANK_USD_PER_SEARCH")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
        * search_units
}

/// Record one Gemini embedding charge against the ledger every `engram budget`
/// cap is measured on.
///
/// NEVER FATAL, and never AFTER the write it pays for. The embedding is paid
/// the moment the provider answers, whether or not the database write that
/// follows commits, so the row goes in first and a failure to write it is a
/// `warn!`: the money is already spent, and losing the memory (or the edit)
/// over a bookkeeping row is the worse lie. This is the convention `remember`
/// established in Wave 0; `edit` used `?` after its transaction, which turned a
/// bookkeeping failure into a non-zero exit on an edit that had already landed
/// — and a caller who retries that pays twice.
#[allow(clippy::too_many_arguments)]
pub fn record_embed_spend(
    store: &engram_storage::SqliteStore,
    operation: &str,
    model: &str,
    kb: Option<&str>,
    diary: Option<&str>,
    texts: &[&str],
    requests: i64,
    metadata: serde_json::Value,
) {
    if texts.is_empty() {
        return;
    }
    let tokens = estimated_tokens_for_texts(texts);
    if let Err(e) = store.record_usage_event(
        "gemini",
        operation,
        Some(model),
        kb,
        diary,
        requests.max(1),
        texts.len() as i64,
        tokens,
        0,
        0.0,
        gemini_embed_cost_usd(tokens),
        metadata,
    ) {
        tracing::warn!("could not record {operation} usage: {e}");
    }
}

pub fn run(ctx: &AppContext, kb: Option<String>, since: Option<String>) -> Result<(), CliError> {
    let summary = ctx.store.usage_summary(kb.as_deref(), since.as_deref())?;
    let total_events: i64 = summary.iter().map(|s| s.events).sum();
    let total_requests: i64 = summary.iter().map(|s| s.request_count).sum();
    let total_input_tokens_estimated: i64 = summary.iter().map(|s| s.input_tokens_estimated).sum();
    let total_output_tokens_estimated: i64 =
        summary.iter().map(|s| s.output_tokens_estimated).sum();
    let total_search_units: f64 = summary.iter().map(|s| s.search_units).sum();
    let total_search_units = if total_search_units.abs() < f64::EPSILON {
        0.0
    } else {
        total_search_units
    };
    let total_cost_usd_estimated: f64 = summary.iter().map(|s| s.cost_usd_estimated).sum();
    let total_cost_usd_estimated = if total_cost_usd_estimated.abs() < f64::EPSILON {
        0.0
    } else {
        total_cost_usd_estimated
    };

    let mut meta = Metadata::default();
    meta.add("rows", summary.len());
    meta.add("events", total_events);
    meta.add("requests", total_requests);
    meta.add("input_tokens_estimated", total_input_tokens_estimated);
    meta.add("output_tokens_estimated", total_output_tokens_estimated);
    meta.add("search_units", total_search_units);
    meta.add(
        "cost_usd_estimated",
        format!("{:.6}", total_cost_usd_estimated),
    );

    print_success(
        ctx.format,
        json!({
            "filters": {
                "kb": kb,
                "since": since,
            },
            "summary": summary,
            "totals": {
                "events": total_events,
                "requests": total_requests,
                "input_tokens_estimated": total_input_tokens_estimated,
                "output_tokens_estimated": total_output_tokens_estimated,
                "search_units": total_search_units,
                "cost_usd_estimated": total_cost_usd_estimated,
            },
            "notes": [
                "Gemini embedding tokens are estimated locally as chars/4 because the embedding REST response does not expose billed token counts in this client path.",
                "Cohere rerank usage is tracked as search_units. Set ENGRAM_COHERE_RERANK_USD_PER_SEARCH to add local cost estimates.",
                "Fact-extraction calls (Anthropic or OpenRouter) and synthesis are recorded per call under their provider; costs are not estimated locally because model pricing varies by route."
            ]
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_storage::SqliteStore;

    #[test]
    fn an_embed_charge_is_attributed_to_the_kb_a_budget_can_see() {
        let store = SqliteStore::open_in_memory().unwrap();
        record_embed_spend(
            &store,
            "edit_embed",
            "gemini-embedding-2",
            Some("personal"),
            Some("work"),
            &["a chunk of text", "another chunk"],
            2,
            json!({ "estimated": true, "partial": false }),
        );
        // A kb-scoped summary is exactly what `budget::check_scope` reads.
        let scoped = store.usage_summary(Some("personal"), None).unwrap();
        assert_eq!(scoped.len(), 1, "the charge is invisible to a per-KB cap");
        assert_eq!(scoped[0].operation, "edit_embed");
        assert_eq!(scoped[0].item_count, 2);
        assert!(scoped[0].cost_usd_estimated > 0.0);
        assert!(
            store.usage_summary(Some("other"), None).unwrap().is_empty(),
            "the charge leaked into another KB's budget"
        );
    }

    #[test]
    fn an_empty_charge_writes_no_row() {
        let store = SqliteStore::open_in_memory().unwrap();
        record_embed_spend(&store, "edit_embed", "m", None, None, &[], 0, json!({}));
        assert!(store.usage_summary(None, None).unwrap().is_empty());
    }
}
