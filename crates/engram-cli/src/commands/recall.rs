//! `engram recall <query>` — cloud-quality hybrid retrieval over a KB.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_envelope, round_score, Metadata, Scope, Verdict};
use crate::read_surface::{self, RequestShape};
use crate::respond;
use crate::retrieval::{
    hybrid_recall, render_answer_context, resolve_kb_scope, resolve_retrieval_backends, Filters,
    HybridParams, RecallMode, RelevanceJudge, RetrievalProfile,
};
use chrono::{DateTime, Utc};
use engram_core::layers::approx_tokens;
use engram_core::relations::{canonical_relation, is_safety_slot};
use engram_core::temporal::{parse_bound, DayBound};
use engram_core::types::{Fact, Layer};
use engram_storage::SqliteStore;
use serde_json::{json, Value};
use std::time::Instant;

/// One recall request, however it was spelled.
///
/// `recall`, `search` and `memory search` used to be three call shapes into
/// one function, and the third hard-coded a different layer, rerank depth and
/// graph fan-out — two commands with one name, answering differently. One
/// struct means a default that differs is visible in `cli.rs` rather than
/// buried in a dispatch arm.
pub struct RecallArgs {
    pub query: String,
    pub top_k: usize,
    pub layer: String,
    pub mode: String,
    pub profile: String,
    pub kb: Option<String>,
    pub all_kbs: bool,
    pub diary: String,
    pub rerank_top_n: Option<usize>,
    pub allow_mixed_embeddings: bool,
    pub since: Option<String>,
    pub until: Option<String>,
    /// `--include-expired`: admit memories whose `valid_until` has passed.
    pub include_expired: bool,
    /// `--full`: repeat each passage's text in `results[].content`. Without
    /// it the text is printed once, in `answer_context`.
    pub full: bool,
    /// Which spelling the caller used, reported as `metadata.command_alias`
    /// (blessed by D2). It changes NO behaviour; it makes a transcript
    /// traceable to the surface that produced it.
    pub alias: &'static str,
}

/// What a caller may do with an open conflict, in the order `respond.rs`
/// publishes them (`keep-both` first: the only universally safe blind
/// action). Permission is never narrowed by safety — advice is (R7).
pub const ALLOWED_ACTIONS: &[&str] = &["keep-both", "supersede-old", "reject-new"];

pub async fn run(ctx: &AppContext, args: RecallArgs) -> Result<(), CliError> {
    let RecallArgs {
        query,
        top_k,
        layer,
        mode,
        profile,
        kb,
        all_kbs,
        diary,
        rerank_top_n,
        allow_mixed_embeddings,
        since,
        until,
        include_expired,
        full,
        alias,
    } = args;
    if query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }

    let start = Instant::now();
    let layer_enum = parse_layer(&layer)?;
    let recall_mode = RecallMode::parse(&mode)?;
    let profile_enum = RetrievalProfile::parse(&profile)?;
    let since_dt = parse_optional_time(&since, DayBound::Start)?;
    let until_dt = parse_optional_time(&until, DayBound::End)?;
    let rrf_k: f32 = std::env::var("ENGRAM_RRF_K")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60.0);

    // Scope resolution: explicit --kb > retrieval.default_kb > every KB.
    // The old default ("default") meant a plain recall searched a single,
    // usually near-empty namespace and missed everything filed elsewhere.
    let scope = resolve_kb_scope(kb.as_deref(), all_kbs);

    let rerank_candidates = rerank_top_n.unwrap_or_else(|| profile_enum.default_rerank_top_n());
    let filters = Filters {
        diary: if diary == "*" {
            None
        } else {
            Some(diary.clone())
        },
        kb: scope.kb.clone(),
        since: since_dt,
        until: until_dt,
        include_expired,
    };
    // Resolved before any paid call, so a bad `judge.relevance` is exit 2
    // with nothing spent. `--profile offline` promises zero network.
    let relevance_judge = RelevanceJudge::resolve(profile_enum == RetrievalProfile::Offline)?;
    let params = HybridParams {
        query: &query,
        // With the judge active, retrieve a wider pool: the misses are
        // dropped before the cut to top_k, not after it.
        top_k: if relevance_judge.is_some() {
            RelevanceJudge::pool(top_k, rerank_candidates)
        } else {
            top_k
        },
        rrf_k,
        rerank_top_n: rerank_candidates,
        filters,
        mode: recall_mode,
        allow_mixed_embeddings,
        defer_doc_cap: relevance_judge.is_some(),
    };

    // Provider choice, the missing-key hard error and the spend guardrail all
    // live in `resolve_retrieval_backends` now — the three copies of this block
    // drifted, and research and the daemon kept silently falling back to the
    // 64-dim stub long after recall stopped.
    let backends = resolve_retrieval_backends(&ctx.store, profile_enum, scope.as_deref())?;
    let outcome = hybrid_recall(
        &ctx.store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;

    let mut results = outcome.results;
    let notes = outcome.notes;
    let stats = outcome.stats;
    let fused_any = !results.is_empty();

    // Judged before the layer budget, so the budget is spent on passages
    // that answer the query rather than on the ones they displaced.
    let relevance = match &relevance_judge {
        Some(judge) => {
            let report = judge.apply(&ctx.store, &query, &mut results, top_k).await;
            if let Some(response) = &report.response {
                crate::judge::record_judge_usage(
                    ctx,
                    crate::judge::JudgeSite::Relevance.operation(),
                    response,
                    scope.as_deref(),
                    if diary == "*" { None } else { Some(&diary) },
                );
            }
            Some(report)
        }
        None => None,
    };

    // The memory-layer token budget trims the returned set to fit an agent's
    // context window. But it must never silently return FEWER than the caller's
    // explicit --top-k: previously a default `topic` budget (800 tokens) capped
    // a `--top-k 10` recall to ~2-4 paper chunks. Honor the smaller of what the
    // user asked for and the budget, but always deliver at least top_k results
    // when that many were retrieved.
    let budget = layer_enum.default_token_budget();
    let mut used_tokens = 0usize;
    let mut layered = Vec::new();
    for r in &results {
        let cost = approx_tokens(&r.content);
        // Always include up to top_k results regardless of budget; only the
        // budget-driven trim beyond top_k applies.
        if layered.len() >= top_k && used_tokens + cost > budget {
            break;
        }
        layered.push(r);
        used_tokens += cost;
    }

    // Surface contradictions relevant to this query: if the KB records that a
    // fact about a queried subject was superseded (or two active facts about it
    // disagree), report it so the caller can "cite both / note the update"
    // instead of unknowingly answering from a stale fact. This is the read-path
    // half of the contradiction-resolution promise.
    //
    // The diary is threaded through both halves now: a `--diary work` recall
    // used to report a conflict between two `personal` facts, which is a
    // cross-diary leak on the exact surface that exists to stop silent wrong
    // answers.
    let conflict_diary = if diary == "*" {
        None
    } else {
        Some(diary.as_str())
    };
    let surface = surface_conflicts(&ctx.store, scope.as_deref(), conflict_diary, &query, 10)?;
    let conflicts = surface.items;
    let conflicts_overridden = conflicts
        .iter()
        .filter(|c| c["suggested_action_overridden"] == json!(true))
        .count();

    // `layered` is a Vec<&HybridResult>; `.iter()` would hand the renderer
    // `&&HybridResult`, so copy the references out.
    let answer_context = render_answer_context(layered.iter().copied());
    let results_json: Vec<_> = layered
        .iter()
        .map(|r| {
            let mut v = json!({
            "id": r.id.to_string(),
            // The id a mutator accepts. `id` keeps its meaning (the row's own
            // id, which for a chunk hit is a chunk id) and this is the one to
            // hand `edit` or `forget`.
            "memory_id": r.provenance.memory_id.map(|m| m.to_string()),
            "handle": read_surface::hit_handle(&r.kind, r.id),
            "provenance": r.provenance.to_json(),
            "kind": r.kind,
            "score": round_score(r.score),
            "rerank_score": r.rerank_score.map(round_score),
            "kb": r.kb,
            "citations": r.citations.iter().map(|c| json!({
                "document_id": c.document_id.map(|id: uuid::Uuid| id.to_string()),
                "chunk_id": c.chunk_id.map(|id: uuid::Uuid| id.to_string()),
                "page": c.page,
                "section": c.section,
                "source": c.source,
            })).collect::<Vec<_>>(),
            "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
            });
            // One copy on the wire: the text is in `answer_context`, quoted.
            // Repeating it here doubled every recall an agent reads. The
            // human view prints from it, so it stays there.
            if full || ctx.format == crate::output::OutputFormat::Human {
                v["content"] = json!(r.content);
            }
            // Present only when the relevance judge answered for this row.
            if let Some(p) = r.relevance {
                v["relevance"] = json!(engram_llm::judge::wire_decimal(p));
                // The judge saw only the passage's prefix: the score is not
                // a verdict on the whole, and the result was never dropped.
                if crate::retrieval::judged_on_prefix(&r.content) {
                    v["relevance_on_prefix"] = json!(true);
                }
            }
            v
        })
        .collect();

    let query_tokens_estimated = estimated_tokens(&query);
    let gemini_query_cost = if backends.cloud_embedder {
        gemini_embed_cost_usd(query_tokens_estimated)
    } else {
        0.0
    };
    let cohere_search_units = if backends.use_cohere() && fused_any {
        1.0
    } else {
        0.0
    };
    let cohere_cost = cohere_rerank_cost_usd(cohere_search_units);

    if backends.cloud_embedder {
        ctx.store.record_usage_event(
            "gemini",
            "recall_query_embed",
            Some(backends.embed_model.as_str()),
            scope.as_deref(),
            if diary == "*" { None } else { Some(&diary) },
            1,
            1,
            query_tokens_estimated,
            0,
            0.0,
            gemini_query_cost,
            json!({
                "dimensions": backends.embed_dims,
                "prompt_format": backends.prompt_format.clone(),
                "estimated": true,
            }),
        )?;
    }
    if backends.use_cohere() && fused_any {
        ctx.store.record_usage_event(
            "cohere",
            "recall_rerank",
            Some(backends.cohere_model.as_str()),
            scope.as_deref(),
            if diary == "*" { None } else { Some(&diary) },
            1,
            rerank_candidates as i64,
            0,
            0,
            cohere_search_units,
            cohere_cost,
            json!({
                "top_k": top_k,
                "candidate_budget": rerank_candidates,
            }),
        )?;
    }

    let mut meta = Metadata {
        elapsed_ms: start.elapsed().as_millis() as u64,
        ..Default::default()
    };
    meta.add("profile", profile_enum.as_str());
    // A silent failover must not stay silent: if a fallback key served this
    // command, say so in metadata AND on stderr so both agents and humans see
    // the primary needs fixing while everything still works.
    if let Some(note) = backends.embedder.degradation_note() {
        meta.add("embedder_degraded", note.clone());
        eprintln!("warning: {note}");
    }
    meta.add("mode", recall_mode.as_str());
    meta.add("kb", scope.label());
    meta.add("kb_scope", scope.origin.as_str());
    meta.add("diary", if diary == "*" { "*" } else { diary.as_str() });
    // Validity: expired memories are withheld by default, and the
    // withholding is always visible ("N memories expired; --include-expired
    // reaches them"), never a silent thinning of results.
    if let Some(expiry) = notes.expiry_json() {
        meta.add("expiry", expiry);
    }
    meta.add(
        "retriever",
        if backends.cloud_embedder {
            "hybrid_gemini"
        } else {
            "hybrid_stub"
        },
    );
    meta.add("embed_model", backends.embed_model.clone());
    meta.add("embed_dims", backends.embed_dims);
    meta.add("embed_prompt_format", backends.prompt_format.clone());
    meta.add("reranker", backends.reranker_label.clone());
    meta.add("rrf_k", rrf_k);
    if let Some(report) = &relevance {
        meta.add("relevance", report.to_json());
        if let Some(e) = &report.error {
            eprintln!("warning: {e}");
        }
    }
    meta.add("layer", format!("{:?}", layer_enum).to_lowercase());
    meta.add("token_budget", budget);
    meta.add("tokens_used", used_tokens);
    meta.add("results_returned", results_json.len());
    meta.add("candidates_considered", rerank_candidates);
    meta.add("gemini_query_tokens_estimated", query_tokens_estimated);
    meta.add("gemini_cost_usd", format!("{:.7}", gemini_query_cost));
    meta.add("cohere_search_units", cohere_search_units);
    meta.add("cohere_cost_usd", format!("{:.5}", cohere_cost));
    meta.add(
        "total_cost_usd_estimated",
        format!("{:.6}", gemini_query_cost + cohere_cost),
    );

    meta.command_alias(alias);
    meta.suggested_action_overridden(conflicts_overridden);
    // What this read actually looked at — the answer to "did you search the
    // namespace I meant?", which no response could previously give.
    Scope::retrieval(
        if scope.as_deref().is_some() {
            vec![scope.label().to_string()]
        } else if stats.kbs_searched.is_empty() {
            vec!["*".to_string()]
        } else {
            stats.kbs_searched.clone()
        },
        if diary == "*" { "*" } else { diary.as_str() },
        scope.origin.as_str(),
        stats.chunks_considered,
    )
    .attach(&mut meta);

    // `verdict` and `status` are computed from the SAME boolean, in one place:
    // two fields answering one question is how `count` came to mean two
    // things.
    let empty = results_json.is_empty();
    let status = if empty { "no_results" } else { "success" };
    let searched_kbs = if scope.as_deref().is_some() {
        vec![scope.label().to_string()]
    } else if stats.kbs_searched.is_empty() {
        vec!["*".to_string()]
    } else {
        stats.kbs_searched.clone()
    };
    let verdict = if let Some(report) = relevance.as_ref().filter(|r| r.abstained) {
        // Candidates were found and every one was judged not to answer: a
        // real cutoff rejected real candidates, so the certificate names it.
        Verdict::NoMatch(read_surface::absence_below_cutoff(
            &stats,
            searched_kbs,
            report.top_relevance,
            report.floor,
        ))
    } else if empty {
        let shape = request_shape(&ctx.store, &diary, since_dt.is_some() || until_dt.is_some())?;
        Verdict::NoMatch(read_surface::absence_from(&stats, searched_kbs, shape))
    } else {
        Verdict::Results
    };

    let Value::Object(mut data) = json!({
        "status": status,
        // A ranked read is a truncation, not a page: its order depends on a
        // network reranker whose output is not guaranteed stable between
        // calls, so an offset cursor would be a lie with a stable-looking
        // shape. There is no `cursor` here, ever — to see more, raise
        // --top-k. `candidates_fused` is the pool the ranking chose from and
        // is deliberately NOT called `total`, which means "matching rows in
        // scope" everywhere it appears.
        "returned": results_json.len(),
        "candidates_fused": stats.candidates_fused,
        "conflicts_returned": conflicts.len(),
        "conflicts_total": surface.total,
        "answer_context": answer_context,
        "results": results_json,
        "conflicts": conflicts,
        // `RECALL_DATA` has promised both at the top of `data` since the
        // contract landed, as `always`; they were only ever emitted one level
        // down, so recall failed its own published schema. The copies in
        // `data.metadata` and the envelope's `metadata` stay.
        "kb": scope.label(),
        "kb_scope": scope.origin.as_str(),
        "metadata": {
            "profile": profile_enum.as_str(),
            "embed_model": backends.embed_model,
            "embed_dims": backends.embed_dims,
            "prompt_format": backends.prompt_format,
            "reranker": backends.reranker_label,
            "candidates_considered": rerank_candidates,
            "kb": scope.label(),
            "kb_scope": scope.origin.as_str()
        }
    }) else {
        unreachable!("json! object literal");
    };
    // Present only when they say something, exactly as in the envelope's
    // `metadata`: a `null` on every call is bytes paid to learn nothing.
    if let Some(Value::Object(inner)) = data.get_mut("metadata") {
        if let Some(note) = notes.expiry_json() {
            inner.insert("expiry".to_string(), note);
        }
    }
    // `absence` lands only on `no_match`: a certificate of absence beside
    // results would be a certificate of nothing.
    verdict.attach(&mut data);

    print_envelope(ctx.format, status, Value::Object(data), meta, |data| {
        if let Some(arr) = data.get("results").and_then(|v| v.as_array()) {
            for r in arr {
                let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("result");
                let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let snippet: String = content.chars().take(110).collect();
                println!("{:.3}  {:10}  {}", score, kind, snippet);
            }
        }
    });
    Ok(())
}

/// Which filter to blame for an empty read.
///
/// "The request carried a diary" is not the same claim as "the diary is why
/// you got nothing": every recall carries a diary, because `--diary default`
/// is the default. Blaming it unconditionally would tell a caller with an
/// EMPTY store to go looking for a diary that never existed. So the diary is
/// only named when the store actually holds memories somewhere else — two
/// COUNT(*)s, and only on the path that already returned nothing.
pub fn request_shape(
    store: &SqliteStore,
    diary: &str,
    has_window: bool,
) -> Result<RequestShape, CliError> {
    let has_diary_filter = if diary == "*" {
        false
    } else {
        store.count_memories_scoped(Some(diary), None)? == 0
            && store.count_memories_scoped(None, None)? > 0
    };
    Ok(RequestShape {
        has_window,
        has_diary_filter,
    })
}

/// The conflicts array plus the size of the backlog it was drawn from.
///
/// `total` is separate from `items.len()` because the two are different
/// questions and a caller that reads one for the other under-reports a queue:
/// the shape pinned by D9 so the daemon renders the same object the CLI does.
pub struct ConflictSurface {
    pub items: Vec<Value>,
    pub total: i64,
}

/// Find contradictions relevant to the query.
///
/// Two kinds are surfaced: CLOSED intervals (a fact stopped being true — cite
/// the current value, or say the state ended) and CONTESTED pairs (an
/// unresolved conflict is queued, both values are active, and the caller must
/// not pick one silently). The memory never pretends to know an answer it
/// does not have.
///
/// Takes `&SqliteStore` rather than `&AppContext` (D9) so the daemon can call
/// it from a pooled reader, and takes `diary` because the version that did not
/// leaked one diary's conflicts into another's recall.
///
/// Prints no value a reader may no longer see: a fact from a memory that was
/// forgotten, or whose content changed since, is never shown. Forget and edit
/// close a fact but keep its text, and an open conflict row outlives both, so
/// without this rule recall printed a forgotten value back as a `retracted`
/// or `contested` entry. The storage queries apply it, so the page limit and
/// the total count only ever see visible facts. Expiry is deliberately not
/// part of it: an expired memory is withheld, not gone (`--include-expired`
/// brings it back), and dropping its side of a contested pair would make the
/// other value look settled.
pub fn surface_conflicts(
    store: &SqliteStore,
    kb: Option<&str>,
    diary: Option<&str>,
    query: &str,
    limit: usize,
) -> Result<ConflictSurface, CliError> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Ok(ConflictSurface {
            items: Vec::new(),
            total: 0,
        });
    }
    let facts = store.facts_matching_terms(kb, diary, &terms, 20)?;
    let successors: Vec<uuid::Uuid> = facts
        .iter()
        .filter_map(|f| f.closed_by.or(f.superseded_by))
        .collect();
    let visible_successors = store.visible_facts(&successors)?;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for f in facts {
        // THE SWEEP. This used to read `superseded_by`, so a medication the
        // user STOPPED — closed interval, no successor — never surfaced here
        // and the store went on answering as if it were current. `is_open_fact`
        // is the one definition of "still asserted".
        if engram_core::interval::is_open_fact(&f) {
            continue;
        }
        if !seen.insert(f.id) {
            continue;
        }
        // A fact closed because the state ENDED has no successor, and printing
        // an empty `new_object` as though it did is the stopped-medication bug
        // inverted. Follow `closed_by` first (it names whatever closed the
        // interval), then the compat `superseded_by`. A successor the reader
        // may no longer see is withheld: the entry still says this value is
        // not current, without the value that replaced it.
        let successor = f
            .closed_by
            .or(f.superseded_by)
            .filter(|id| visible_successors.contains(id));
        let new_object = match successor {
            Some(id) => store.get_fact(id)?.map(|nf| nf.object),
            None => None,
        };
        out.push(json!({
            "status": closed_status(&f),
            "subject": f.subject,
            "predicate": f.predicate,
            "old_object": f.object,
            "new_object": new_object,
            "close_reason": f.close_reason,
            "close_authority": f.close_authority,
            "valid_from": f.valid_from.map(|t| t.to_rfc3339()),
            "valid_until": f.valid_until.map(|t| t.to_rfc3339()),
            "superseded_at": f.superseded_at.map(|t| t.to_rfc3339()),
            "old_fact_id": f.id.to_string(),
            "old_fact_handle": read_surface::handle(engram_core::handle::HandleKind::Fact, f.id),
            "new_fact_id": successor.map(|id| id.to_string()),
            "note": closed_note(&f),
        }));
    }

    let closed_rows = out.len() as i64;

    // Contested: open conflicts touching the query. Both values are active;
    // report both rather than silently returning one side.
    let contested = store.open_conflicts_matching_terms(kb, diary, &terms, limit)?;
    let open_total = store.count_open_conflicts_matching_terms(kb, diary, &terms)?;
    for (row, new_f, old_f) in contested {
        let safety = pair_is_safety(&new_f, &old_f);
        let suggested = read_surface::suggested_action_for(row.suggested.as_deref(), safety);
        let rendered = respond::conflicts(&[respond::ConflictInput {
            id: row.id,
            new: &new_f,
            old: &old_f,
            detector: &row.detector,
            suggested,
            allowed: ALLOWED_ACTIONS,
            status: "contested",
        }]);
        let Some(Value::Object(mut entry)) = rendered.into_iter().next() else {
            continue;
        };
        entry.insert("kb".to_string(), json!(row.kb));
        entry.insert("diary".to_string(), json!(row.diary));
        entry.insert("decision_key".to_string(), json!(row.decision_key));
        entry.insert("member_key".to_string(), json!(row.member_key));
        entry.insert("registry_version".to_string(), json!(row.registry_version));
        entry.insert("safety".to_string(), json!(safety));
        // Named per row, not just counted in metadata: a write-side regression
        // that keeps emitting `supersede-old` on a safety pair must stay
        // visible through the guard that corrects it.
        entry.insert(
            "suggested_action_overridden".to_string(),
            json!(read_surface::suggested_action_overridden(
                row.suggested.as_deref(),
                suggested
            )),
        );
        entry.insert("new".to_string(), conflict_side(&new_f));
        entry.insert("old".to_string(), conflict_side(&old_f));
        out.push(Value::Object(entry));
    }
    // The closed-interval half is a bounded scan with no counting query, so
    // its rows count once; the contested half reports its true backlog. A
    // caller that sees `conflicts_returned: 3, conflicts_total: 953` knows
    // there is a queue behind the page — the number nobody had before.
    Ok(ConflictSurface {
        total: closed_rows + open_total,
        items: out,
    })
}

/// The status word for a closed interval. Widened from `superseded` alone,
/// because a stopped medication and a corrected typo are not the same event
/// and an agent that treats them alike gives medical advice from a retired
/// prescription.
fn closed_status(f: &Fact) -> &'static str {
    match f.close_reason.as_deref() {
        Some("superseded") => "superseded",
        Some("stopped") => "ended",
        Some("corrected") => "corrected",
        Some("retracted_source") => "retracted",
        // Pre-v10 rows: a successor means it was superseded, anything else is
        // honestly just closed.
        _ if f.superseded_by.is_some() => "superseded",
        _ => "closed",
    }
}

fn closed_note(f: &Fact) -> &'static str {
    match f.close_reason.as_deref() {
        Some("stopped") => {
            "this state ENDED and has no successor; do not cite it as current and do not \
             invent a replacement value"
        }
        _ => "this fact was updated in the knowledge base; cite the current value or surface the change",
    }
}

fn pair_is_safety(new_f: &Fact, old_f: &Fact) -> bool {
    is_safety_slot(&canonical_relation(&new_f.predicate))
        || is_safety_slot(&canonical_relation(&old_f.predicate))
}

/// One side of a conflict, with the provenance every other row now carries.
fn conflict_side(f: &Fact) -> Value {
    json!({
        "id": f.id.to_string(),
        "handle": read_surface::handle(engram_core::handle::HandleKind::Fact, f.id),
        "memory_id": f.source_memory_id.to_string(),
        "subject": f.subject,
        "predicate": f.predicate,
        "object": f.object,
        "confidence": f.confidence,
        "created_at": f.created_at.to_rfc3339(),
        "open": engram_core::interval::is_open_fact(f),
        "valid_from": f.valid_from.map(|t| t.to_rfc3339()),
        "valid_until": f.valid_until.map(|t| t.to_rfc3339()),
        "provenance": read_surface::Provenance {
            memory_id: Some(f.source_memory_id),
            kb: Some(f.kb.clone()),
            diary: Some(f.diary.clone()),
            derived_from: Some(read_surface::DERIVED_FACT),
            ..Default::default()
        }.to_json(),
    })
}

/// Lowercase content tokens from a query (length >= 3, no stopwords) used to
/// match fact subjects/objects.
fn query_terms(query: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "what", "which", "how", "does", "are", "was", "were", "with", "that",
        "this", "from", "into", "who", "why", "when", "where", "current", "latest", "value",
    ];
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 || STOP.contains(&lower.as_str()) {
            continue;
        }
        if seen.insert(lower.clone()) {
            out.push(lower);
        }
    }
    out
}

fn parse_layer(s: &str) -> Result<Layer, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "identity" | "l0" => Ok(Layer::Identity),
        "critical" | "l1" => Ok(Layer::Critical),
        "topic" | "l2" => Ok(Layer::Topic),
        "deep" | "l3" => Ok(Layer::Deep),
        other => Err(CliError::BadInput(format!(
            "unknown layer: {other} (expected identity|critical|topic|deep)"
        ))),
    }
}

/// Parse `--since` / `--until`.
///
/// The write side (`remember --event-time`) accepts RFC3339 *or* `YYYY-MM-DD`
/// and its own help text says it drives `recall --since/--until`, so the read
/// side has to accept the same spellings — an RFC3339-only reader turned the
/// documented round-trip (`--event-time 2026-06-01` then `--since 2026-06-01`)
/// into exit 3.
///
/// A bare date is a whole day, so the bound it denotes depends on its side of
/// the range: `--since 2026-06-01` starts at 00:00:00Z and
/// `--until 2026-06-01` ends at 23:59:59.999999999Z, which makes
/// `--since D --until D` select exactly day D. RFC3339 input is an exact
/// instant and is unchanged. Anything else is still a hard error, never a
/// silent "now".
fn parse_optional_time(
    s: &Option<String>,
    bound: DayBound,
) -> Result<Option<DateTime<Utc>>, CliError> {
    match s {
        None => Ok(None),
        Some(v) => parse_bound(v, bound).map(Some).ok_or_else(|| {
            CliError::BadInput(format!(
                "invalid timestamp {v}: expected RFC3339 (2026-06-01T09:30:00Z) or YYYY-MM-DD"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_and_until_accept_a_bare_date_and_bracket_the_day() {
        let since = parse_optional_time(&Some("2026-06-01".into()), DayBound::Start)
            .unwrap()
            .unwrap();
        let until = parse_optional_time(&Some("2026-06-01".into()), DayBound::End)
            .unwrap()
            .unwrap();
        assert_eq!(since.to_rfc3339(), "2026-06-01T00:00:00+00:00");
        assert_eq!(until.to_rfc3339(), "2026-06-01T23:59:59.999999999+00:00");
    }

    #[test]
    fn rfc3339_bounds_are_unchanged() {
        let t = parse_optional_time(&Some("2026-06-01T09:30:00Z".into()), DayBound::End)
            .unwrap()
            .unwrap();
        assert_eq!(t.to_rfc3339(), "2026-06-01T09:30:00+00:00");
    }

    #[test]
    fn garbage_bounds_are_bad_input_exit_3() {
        for raw in ["not a date", "2026-13-45", ""] {
            let err = parse_optional_time(&Some(raw.into()), DayBound::Start).unwrap_err();
            assert_eq!(err.exit_code(), 3, "{raw}");
        }
        assert!(parse_optional_time(&None, DayBound::Start)
            .unwrap()
            .is_none());
    }
}
