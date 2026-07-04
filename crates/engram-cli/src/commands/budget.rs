//! `engram budget` — configure and inspect local API spend guardrails.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use chrono::{Datelike, TimeZone, Utc};
use serde_json::json;

pub fn show(ctx: &AppContext, kb: Option<String>) -> Result<(), CliError> {
    let scope = scope_for(kb.as_deref());
    let budget = ctx.store.get_usage_budget(&scope)?;
    let now = Utc::now();
    let today_start = Utc
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .unwrap()
        .to_rfc3339();
    let month_start = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .unwrap()
        .to_rfc3339();
    let daily = totals(ctx, kb.as_deref(), &today_start)?;
    let monthly = totals(ctx, kb.as_deref(), &month_start)?;

    let daily_budget = budget.as_ref().and_then(|b| b.daily_usd);
    let monthly_budget = budget.as_ref().and_then(|b| b.monthly_usd);
    let daily_remaining = daily_budget.map(|b| b - daily.cost_usd_estimated);
    let monthly_remaining = monthly_budget.map(|b| b - monthly.cost_usd_estimated);

    let mut meta = Metadata::default();
    meta.add("scope", scope.clone());
    meta.add(
        "daily_cost_usd_estimated",
        format!("{:.6}", daily.cost_usd_estimated),
    );
    meta.add(
        "monthly_cost_usd_estimated",
        format!("{:.6}", monthly.cost_usd_estimated),
    );
    print_success(
        ctx.format,
        json!({
            "scope": scope,
            "kb": kb,
            "budget": budget,
            "daily": {
                "since": today_start,
                "cost_usd_estimated": daily.cost_usd_estimated,
                "input_tokens_estimated": daily.input_tokens_estimated,
                "requests": daily.requests,
                "search_units": daily.search_units,
                "budget_usd": daily_budget,
                "remaining_usd_estimated": daily_remaining,
                "over_budget": daily_remaining.map(|v| v < 0.0).unwrap_or(false),
            },
            "monthly": {
                "since": month_start,
                "cost_usd_estimated": monthly.cost_usd_estimated,
                "input_tokens_estimated": monthly.input_tokens_estimated,
                "requests": monthly.requests,
                "search_units": monthly.search_units,
                "budget_usd": monthly_budget,
                "remaining_usd_estimated": monthly_remaining,
                "over_budget": monthly_remaining.map(|v| v < 0.0).unwrap_or(false),
            },
            "note": "Budgets are local guardrails over recorded usage estimates; provider invoices remain the source of truth."
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn set(
    ctx: &AppContext,
    kb: Option<String>,
    daily_usd: Option<f64>,
    monthly_usd: Option<f64>,
) -> Result<(), CliError> {
    if daily_usd.is_none() && monthly_usd.is_none() {
        return Err(CliError::BadInput(
            "budget set requires --daily-usd and/or --monthly-usd".into(),
        ));
    }
    for value in [daily_usd, monthly_usd].into_iter().flatten() {
        if value < 0.0 {
            return Err(CliError::BadInput("budget values must be >= 0".into()));
        }
    }
    let scope = scope_for(kb.as_deref());
    ctx.store
        .upsert_usage_budget(&scope, kb.as_deref(), daily_usd, monthly_usd)?;
    let budget = ctx.store.get_usage_budget(&scope)?;
    print_success(
        ctx.format,
        json!({ "scope": scope, "budget": budget }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn clear(ctx: &AppContext, kb: Option<String>) -> Result<(), CliError> {
    let scope = scope_for(kb.as_deref());
    let deleted = ctx.store.delete_usage_budget(&scope)?;
    print_success(
        ctx.format,
        json!({ "scope": scope, "deleted": deleted }),
        Metadata::default(),
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

fn scope_for(kb: Option<&str>) -> String {
    kb.map(|k| format!("kb:{k}"))
        .unwrap_or_else(|| "global".to_string())
}

/// Enforce spend guardrails BEFORE an operation makes paid API calls. Checks
/// both the KB-scoped budget (if `kb` is set) and the global budget; if either
/// daily or monthly recorded spend is already at/over its cap, returns a
/// rate-limit error (exit code 4) so an autonomous agent actually stops instead
/// of blowing past `--daily-usd`. A no-op when no budget is configured.
///
/// This is a guardrail over *recorded* usage estimates (provider invoices
/// remain the source of truth), and it gates on spend already incurred — it
/// does not pre-authorize the specific next call's cost.
pub fn enforce_budget(ctx: &AppContext, kb: Option<&str>) -> Result<(), CliError> {
    // Disable with ENGRAM_BUDGET_ENFORCE=off for a one-off override.
    if std::env::var("ENGRAM_BUDGET_ENFORCE")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "off" | "0" | "false" | "no"))
        .unwrap_or(false)
    {
        return Ok(());
    }
    for scope_kb in [kb, None] {
        check_scope(ctx, scope_kb)?;
        if scope_kb.is_none() {
            break; // avoid checking global twice when kb is None
        }
    }
    Ok(())
}

fn check_scope(ctx: &AppContext, kb: Option<&str>) -> Result<(), CliError> {
    let scope = scope_for(kb);
    let Some(budget) = ctx.store.get_usage_budget(&scope)? else {
        return Ok(());
    };
    let now = Utc::now();
    if let Some(cap) = budget.daily_usd {
        let today_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
            .single()
            .unwrap()
            .to_rfc3339();
        let spent = totals(ctx, kb, &today_start)?.cost_usd_estimated;
        if spent >= cap {
            return Err(CliError::RateLimited(format!(
                "daily budget for scope `{scope}` reached: ${spent:.4} spent of ${cap:.2} cap. \
                 Raise it with `engram budget set {}--daily-usd <n>`, wait for the day to \
                 roll over, or set ENGRAM_BUDGET_ENFORCE=off to override.",
                kb.map(|k| format!("--kb {k} ")).unwrap_or_default()
            )));
        }
    }
    if let Some(cap) = budget.monthly_usd {
        let month_start = Utc
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .unwrap()
            .to_rfc3339();
        let spent = totals(ctx, kb, &month_start)?.cost_usd_estimated;
        if spent >= cap {
            return Err(CliError::RateLimited(format!(
                "monthly budget for scope `{scope}` reached: ${spent:.4} spent of ${cap:.2} cap. \
                 Raise it with `engram budget set {}--monthly-usd <n>` or set \
                 ENGRAM_BUDGET_ENFORCE=off to override.",
                kb.map(|k| format!("--kb {k} ")).unwrap_or_default()
            )));
        }
    }
    Ok(())
}

#[derive(Default)]
struct UsageTotals {
    requests: i64,
    input_tokens_estimated: i64,
    search_units: f64,
    cost_usd_estimated: f64,
}

fn totals(ctx: &AppContext, kb: Option<&str>, since: &str) -> Result<UsageTotals, CliError> {
    let summary = ctx.store.usage_summary(kb, Some(since))?;
    Ok(UsageTotals {
        requests: summary.iter().map(|s| s.request_count).sum(),
        input_tokens_estimated: summary.iter().map(|s| s.input_tokens_estimated).sum(),
        search_units: summary.iter().map(|s| s.search_units).sum(),
        cost_usd_estimated: summary.iter().map(|s| s.cost_usd_estimated).sum(),
    })
}
