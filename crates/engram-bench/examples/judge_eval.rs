//! Judge eval: runs the SHIPPED question module against labelled data, so the
//! wording evaluated is the wording engram sends.
//!
//! ```text
//! judge_eval grounding --pairs P.jsonl --docs D.jsonl --out O.jsonl [--route R] [--limit N]
//! judge_eval pairs     --pairs P.jsonl --out O.jsonl [--route R] [--limit N]
//! ```
//!
//! - grounding: pairs `{id, doc_id, claim, label}`, docs `{doc_id, title, text}`;
//!   passage = `title + ". " + text` (just `text` when the title is empty);
//!   one request per doc carrying all its claims, 8 docs in flight. Writes
//!   `{id, label, p, lexical, numbers_ok}` per pair (`p` null when
//!   unanswered).
//! - pairs: items `{id, new_text, new_fact{subject,predicate,object},
//!   existing{subject,predicate,object}, gold}`; one pair request per item.
//!   Writes `{id, gold, restates, same_slot, contradicts, old_ended}`.
//!
//! Both end with one `{"summary": ...}` line: requests, input tokens, cost,
//! latency p50/p95. Score with `benchmarks/jev/score.py`.
//!
//! Route: `--route typesafe` (TYPESAFE_API_KEY), `openrouter`
//! (OPENROUTER_API_KEY) or `stub` (ENGRAM_JUDGE_STUB, default 0.5, no
//! network). Without `--route`: TypeSafe if its key is set, else OpenRouter.
//! Keys are read from the environment only and never printed. Each attempt
//! may take 30 s (the probe's limit; engram's own calls use 5 s): the eval
//! measures answers, and a timed-out request is a missing answer.

use engram_llm::judge::lexical::{claim_grounded_lexically, numbers_ok};
use engram_llm::judge::questions::{self, FactTriple, PairAnswers};
use engram_llm::judge::stub::StubJudge;
use engram_llm::judge::typesafe::{TypeSafeJudge, TypeSafeRoute};
use engram_llm::judge::{ask_many, wire_decimal as wire, Judge, JudgeRequest, JudgeResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::time::Duration;

const DOC_CONCURRENCY: usize = 8;
const EVAL_TIMEOUT: Duration = Duration::from_secs(30);

type Error = Box<dyn std::error::Error>;

struct Args {
    mode: String,
    flags: HashMap<String, String>,
}

impl Args {
    fn parse() -> Result<Self, Error> {
        let mut it = std::env::args().skip(1);
        let mode = it
            .next()
            .ok_or("usage: judge_eval grounding|pairs --pairs P --out O ...")?;
        let mut flags = HashMap::new();
        while let Some(flag) = it.next() {
            let name = flag
                .strip_prefix("--")
                .ok_or_else(|| format!("unexpected argument {flag}"))?;
            let value = it.next().ok_or_else(|| format!("--{name} needs a value"))?;
            flags.insert(name.to_string(), value);
        }
        Ok(Self { mode, flags })
    }

    fn get(&self, name: &str) -> Result<&str, Error> {
        self.flags
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("--{name} is required").into())
    }

    fn limit(&self) -> Result<Option<usize>, Error> {
        Ok(match self.flags.get("limit") {
            Some(v) => Some(v.parse()?),
            None => None,
        })
    }
}

fn key(env: &str) -> Option<String> {
    std::env::var(env).ok().filter(|k| !k.trim().is_empty())
}

fn resolve_judge(route: Option<&str>) -> Result<(Box<dyn Judge>, &'static str), Error> {
    let typesafe = || -> Result<(Box<dyn Judge>, &'static str), Error> {
        let key = key("TYPESAFE_API_KEY").ok_or("TYPESAFE_API_KEY is not set")?;
        Ok((
            Box::new(TypeSafeJudge::new(TypeSafeRoute::Direct { key }).with_timeout(EVAL_TIMEOUT)),
            "typesafe",
        ))
    };
    let openrouter = || -> Result<(Box<dyn Judge>, &'static str), Error> {
        let key = key("OPENROUTER_API_KEY").ok_or("OPENROUTER_API_KEY is not set")?;
        Ok((
            Box::new(
                TypeSafeJudge::new(TypeSafeRoute::OpenRouter { key }).with_timeout(EVAL_TIMEOUT),
            ),
            "openrouter",
        ))
    };
    match route {
        Some("typesafe") => typesafe(),
        Some("openrouter") => openrouter(),
        Some("stub") => {
            let spec = key("ENGRAM_JUDGE_STUB").unwrap_or_else(|| "0.5".to_string());
            Ok((Box::new(StubJudge::parse(&spec)?), "stub"))
        }
        Some(other) => Err(format!("--route {other}: expected typesafe|openrouter|stub").into()),
        None if key("TYPESAFE_API_KEY").is_some() => typesafe(),
        None => openrouter(),
    }
}

fn read_jsonl(path: &str) -> Result<Vec<Value>, Error> {
    let file = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if !line.trim().is_empty() {
            out.push(serde_json::from_str(&line)?);
        }
    }
    Ok(out)
}

fn text(v: &Value, field: &str) -> Result<String, Error> {
    match &v[field] {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Err(format!("missing field `{field}`").into()),
        other => Ok(other.to_string()),
    }
}

fn triple(v: &Value) -> Result<FactTriple, Error> {
    Ok(FactTriple::new(
        text(v, "subject")?,
        text(v, "predicate")?,
        text(v, "object")?,
    ))
}

/// Summary over the requests that answered; percentiles by the probe's
/// index rule so the two harnesses report comparable numbers.
fn summary(route: &str, responses: &[Result<JudgeResponse, String>], missing: usize) -> Value {
    let ok: Vec<&JudgeResponse> = responses.iter().filter_map(|r| r.as_ref().ok()).collect();
    let mut latency: Vec<u64> = ok.iter().map(|r| r.latency_ms).collect();
    latency.sort_unstable();
    let pct = |q: f64| {
        latency
            .get(((latency.len() as f64) * q) as usize)
            .copied()
            .unwrap_or(0)
    };
    let mut errors: Vec<&String> = responses.iter().filter_map(|r| r.as_ref().err()).collect();
    errors.sort();
    errors.dedup();
    json!({
        "summary": true,
        "route": route,
        "model": ok.first().map(|r| r.model.clone()),
        "requests": ok.iter().map(|r| r.requests as u64).sum::<u64>(),
        "failed_requests": responses.len() - ok.len(),
        "errors": errors,
        "missing_answers": missing,
        "input_tokens": ok.iter().map(|r| r.input_tokens).sum::<u64>(),
        "cost_usd": ok.iter().map(|r| r.cost_usd).sum::<f64>(),
        "latency_ms_p50": pct(0.5),
        "latency_ms_p95": pct(0.95),
    })
}

async fn run_all(
    judge: &dyn Judge,
    requests: &[JudgeRequest],
) -> Vec<Result<JudgeResponse, String>> {
    ask_many(judge, requests, DOC_CONCURRENCY)
        .await
        .into_iter()
        .map(|r| r.map_err(|e| e.to_string()))
        .collect()
}

async fn grounding(args: &Args, judge: &dyn Judge, route: &str) -> Result<Vec<Value>, Error> {
    let mut pairs = read_jsonl(args.get("pairs")?)?;
    if let Some(n) = args.limit()? {
        pairs.truncate(n);
    }
    let mut passages: HashMap<String, String> = HashMap::new();
    for d in read_jsonl(args.get("docs")?)? {
        let title = match &d["title"] {
            Value::Null => String::new(),
            _ => text(&d, "title")?,
        };
        let body = text(&d, "text")?;
        let passage = if title.trim().is_empty() {
            body
        } else {
            format!("{title}. {body}")
        };
        passages.insert(text(&d, "doc_id")?, passage);
    }
    // Group claims by doc in first-seen order: one request per passage.
    let mut order: Vec<String> = Vec::new();
    let mut claims: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for p in &pairs {
        let doc = text(p, "doc_id")?;
        if !passages.contains_key(&doc) {
            return Err(format!("pair {} names unknown doc {doc}", text(p, "id")?).into());
        }
        claims
            .entry(doc.clone())
            .or_insert_with(|| {
                order.push(doc.clone());
                Vec::new()
            })
            .push((text(p, "id")?, text(p, "claim")?));
    }
    let requests: Vec<JudgeRequest> = order
        .iter()
        .map(|doc| questions::grounding_request(&passages[doc], &claims[doc]))
        .collect();
    let responses = run_all(judge, &requests).await;

    let mut p_by_id: HashMap<String, f32> = HashMap::new();
    for (request, response) in requests.iter().zip(&responses) {
        if let Ok(resp) = response {
            for (id, _) in &request.questions {
                if let Ok(p) = resp.noul(id) {
                    p_by_id.insert(id.clone(), p);
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut missing = 0;
    for p in &pairs {
        let id = text(p, "id")?;
        let claim = text(p, "claim")?;
        let passage = &passages[&text(p, "doc_id")?];
        let score = p_by_id.get(&id).copied().map(wire);
        missing += usize::from(score.is_none());
        out.push(json!({
            "id": id,
            "label": p["label"],
            "p": score,
            "lexical": claim_grounded_lexically(&claim, passage),
            "numbers_ok": numbers_ok(&claim, passage),
        }));
    }
    out.push(summary(route, &responses, missing));
    Ok(out)
}

async fn pairs(args: &Args, judge: &dyn Judge, route: &str) -> Result<Vec<Value>, Error> {
    let mut items = read_jsonl(args.get("pairs")?)?;
    if let Some(n) = args.limit()? {
        items.truncate(n);
    }
    const CANDIDATE: &str = "existing";
    let mut requests = Vec::new();
    for item in &items {
        requests.push(questions::pair_request(
            &text(item, "new_text")?,
            &triple(&item["new_fact"])?,
            &[(CANDIDATE.to_string(), triple(&item["existing"])?)],
        ));
    }
    let responses = run_all(judge, &requests).await;
    let mut out = Vec::new();
    let mut missing = 0;
    for (item, response) in items.iter().zip(&responses) {
        let answers = response
            .as_ref()
            .ok()
            .and_then(|r| PairAnswers::from_response(r, CANDIDATE).ok());
        missing += usize::from(answers.is_none());
        out.push(json!({
            "id": item["id"],
            "gold": item["gold"],
            "restates": answers.map(|a| wire(a.restates)),
            "same_slot": answers.map(|a| wire(a.same_slot)),
            "contradicts": answers.map(|a| wire(a.contradicts)),
            "old_ended": answers.map(|a| wire(a.old_ended)),
        }));
    }
    out.push(summary(route, &responses, missing));
    Ok(out)
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse()?;
    let (judge, route) = resolve_judge(args.flags.get("route").map(String::as_str))?;
    let lines = match args.mode.as_str() {
        "grounding" => grounding(&args, judge.as_ref(), route).await?,
        "pairs" => pairs(&args, judge.as_ref(), route).await?,
        other => return Err(format!("unknown mode {other}: expected grounding|pairs").into()),
    };
    let out_path = args.get("out")?;
    let mut out = std::io::BufWriter::new(std::fs::File::create(out_path)?);
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    if let Some(summary) = lines.last() {
        eprintln!("{summary}");
    }
    Ok(())
}
