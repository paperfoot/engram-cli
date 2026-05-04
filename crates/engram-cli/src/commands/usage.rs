//! `engram usage` — summarize API/search usage recorded by cloud paths.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;

pub const GEMINI_EMBED_USD_PER_MILLION_TOKENS: f64 = 0.15;

pub fn estimated_tokens(text: &str) -> i64 {
    ((text.len() + 3) / 4) as i64
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
                "OpenRouter extraction/synthesis prompt/completion tokens are recorded when the provider response includes usage; costs are not estimated locally because model pricing varies by route."
            ]
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}
