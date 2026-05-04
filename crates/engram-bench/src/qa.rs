//! End-to-end QA track for LongMemEval.
//!
//! Takes the retrieval pipeline (from `longmemeval.rs`) and wraps it with:
//!   1. An answerer LLM that reads the retrieved context and produces an answer.
//!   2. A judge LLM that decides whether the answer matches the gold reference.
//!   3. Optional RAGAS metrics (faithfulness / relevance / precision / recall).
//!
//! This is the benchmark that addresses the "17% correct" critique: MemPalace's
//! published R@5=0.984 was retrieval-only; this track measures whether the LLM
//! actually answers correctly using the retrieved context.

use crate::error::BenchError;
use crate::judge::{judge_answer, judge_answer_cognitive, judge_answer_mab, JudgeVerdict};
use crate::longmemeval::{LongMemEvalDataset, LongMemEvalQuestion};
use crate::metrics::{recall_at_k, reciprocal_rank};
use crate::ragas::{compute_all, RagasMetrics};
use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
use engram_core::types::{Memory, MemorySource, RetrievalSource};
use engram_embed::{Embedder, TaskMode};
use engram_llm::ChatLlm;
use engram_rerank::{RerankCandidate, Reranker};
use engram_storage::SqliteStore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaRunResult {
    pub question_id: String,
    pub question_type: String,
    pub question: String,
    pub gold_answer: String,
    pub candidate_answer: String,
    pub correct: bool,
    pub recall_at_5: f32,
    pub mrr: f32,
    pub retrieved_sessions: Vec<String>,
    pub answer_session_ids: Vec<String>,
    pub ragas: Option<RagasMetrics>,
    pub latency_ms: u64,
    pub answerer_prompt_tokens: u32,
    pub answerer_completion_tokens: u32,
    pub judge_prompt_tokens: u32,
    pub judge_completion_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QaReport {
    pub suite: String,
    pub questions_evaluated: usize,
    pub correct_count: usize,
    pub accuracy: f32,
    pub recall_at_5: f32,
    pub mrr: f32,
    pub ragas: Option<RagasMetrics>,
    pub mean_latency_ms: f32,
    pub p50_latency_ms: f32,
    pub p95_latency_ms: f32,
    pub answerer_total_prompt_tokens: u64,
    pub answerer_total_completion_tokens: u64,
    pub judge_total_prompt_tokens: u64,
    pub judge_total_completion_tokens: u64,
    pub per_question: Vec<QaRunResult>,
    pub by_question_type: HashMap<String, QaTypeStats>,
    pub by_source: HashMap<String, QaTypeStats>,
    pub unscored_count: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QaTypeStats {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f32,
}

const ANSWERER_SYSTEM: &str =
    "You are a precise scientific assistant. Answer questions using ONLY the provided context.\n\n\
     METHOD:\n\
     1. Identify the key entities, names, dates, places, or numbers the question asks about.\n\
     2. Scan ALL retrieved sessions for those entities. Quote the exact text spans.\n\
     3. Collect ALL relevant facts before writing the answer. For list questions, ensure you \
        have found EVERY item across ALL sessions, not just the first match.\n\n\
     TEMPORAL RESOLUTION (CRITICAL):\n\
     Every session header shows an absolute date like '[session_3 — 2:30 pm on 12 May, 2023]'.\n\
     When the conversation text says 'yesterday', 'last week', 'last year', etc., you MUST \
     convert these to absolute dates using the session header as the anchor.\n\
     Example: if session header is '25 May 2023' and the text says 'last year', the answer \
     is '2022', NOT 'last year'. If the text says 'last Saturday', compute the actual date.\n\
     NEVER repeat a relative date as your answer — always resolve it.\n\n\
     PRECISION RULES:\n\
     - Use the EXACT terms from the context. Do not paraphrase, generalise, or summarise.\n\
     - For numbers and dates: give the precise value from the context.\n\
     - For list questions ('what books', 'what activities'): extract EVERY item mentioned across \
       ALL sessions. Missing items is a failure. Comma-separate the complete list.\n\
     - Give ONLY the direct answer. No surrounding narrative, no extra context, no qualifiers.\n\
     - For yes/no or inference questions, give a clear answer with brief reasoning.\n\
     - If the answer is genuinely not in the context, say 'I don't know'. Do NOT guess or \
       fabricate. An honest 'I don't know' is better than a wrong or incomplete answer.\n\n\
     ANSWER FORM (CRITICAL):\n\
     - For 'what is X's status/relationship?' → give the state word from the context, not a description.\n\
     - For 'what does X do?' → give the activity names, not descriptions.\n\
     - For 'what books/items?' → give the specific titles or names, nothing else.\n\
     - For 'how many?' → give the number.\n\
     - For 'how long?' → give the duration as stated ('six months', not '139 days').\n\
     - NEVER answer with a narrative quote from the conversation. Extract the fact.\n\n\
     OUTPUT FORMAT:\n\
     EVIDENCE: <direct quote(s) from context, citing which session each came from>\n\
     ANSWER: <precise, complete answer using exact terms from context>";

fn build_answerer_user(question: &str, context: &str) -> String {
    format!(
        "Context:\n{}\n\nQuestion: {}\n\nWork through the METHOD above, then output EVIDENCE and ANSWER lines:",
        context.trim(),
        question.trim()
    )
}

/// Extract just the ANSWER line from the model's EVIDENCE/ANSWER formatted output.
/// Falls back to the full content if the format isn't followed.
fn extract_answer_line(content: &str) -> String {
    for line in content.lines().rev() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("ANSWER:") {
            return rest.trim().to_string();
        }
        if let Some(rest) = trimmed.strip_prefix("Answer:") {
            return rest.trim().to_string();
        }
    }
    content.trim().to_string()
}

fn stable_id(prefix: &str, key: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{prefix}:{key}").as_bytes())
}

fn flatten_session(turns: &[crate::longmemeval::LongMemEvalTurn]) -> String {
    let mut s = String::new();
    for t in turns {
        s.push_str(&t.role);
        s.push_str(": ");
        s.push_str(&t.content);
        s.push('\n');
    }
    s
}

fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0f32;
    let mut na = 0f32;
    let mut nb = 0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn build_fts_query(text: &str) -> String {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "what", "which", "how", "does", "are", "was", "were",
        "from", "into", "this", "have", "has", "had", "been", "being", "shown", "show", "shows",
        "can", "could", "should", "would", "may", "might",
    ];
    let mut tokens: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        if lower.len() < 3 || STOPWORDS.contains(&lower.as_str()) {
            continue;
        }
        tokens.push(format!("\"{}\"", lower));
    }
    tokens.join(" OR ")
}

/// Run the full QA track on LongMemEval. For each question: build the haystack,
/// retrieve, answer via LLM, judge against gold. Optionally compute RAGAS too.
///
/// If `checkpoint_path` is Some, each completed question is appended as a JSONL
/// line to that file and fsynced, so a crash leaves a valid partial record.
#[allow(clippy::too_many_arguments)]
pub async fn run_longmemeval_qa<E, R, A, J>(
    dataset: &LongMemEvalDataset,
    embedder: &E,
    reranker: Option<&R>,
    answerer: &A,
    judge: &J,
    rrf_k: f32,
    top_k: usize,
    limit: Option<usize>,
    enable_ragas: bool,
    checkpoint_path: Option<std::path::PathBuf>,
) -> Result<QaReport, BenchError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
    A: ChatLlm + ?Sized,
    J: ChatLlm + ?Sized,
{
    use std::io::Write as _;
    let n = limit
        .map(|l| l.min(dataset.questions.len()))
        .unwrap_or(dataset.questions.len());

    let mut checkpoint_file: Option<std::fs::File> = if let Some(ref path) = checkpoint_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Some(f)
    } else {
        None
    };

    let mut results = Vec::with_capacity(n);
    let mut latencies = Vec::with_capacity(n);
    let mut ragas_accum = RagasMetrics::default();
    let mut ragas_count = 0usize;
    let mut total_correct = 0usize;
    let mut total_recall5 = 0f32;
    let mut total_mrr = 0f32;
    let mut answerer_prompt_tokens: u64 = 0;
    let mut answerer_completion_tokens: u64 = 0;
    let mut judge_prompt_tokens: u64 = 0;
    let mut judge_completion_tokens: u64 = 0;

    let chrono_epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    use chrono::TimeZone;

    for (i, q) in dataset.questions.iter().take(n).enumerate() {
        let run_start = Instant::now();
        let store = SqliteStore::open_in_memory()?;
        let mut chunk_to_session: HashMap<Uuid, String> = HashMap::new();
        let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
        let mut seen_sids = std::collections::HashSet::new();

        // Seed haystack.
        let mut pending_chunks: Vec<(Uuid, String, String)> = Vec::new(); // (chunk_id, text, fingerprint)
        for (sid, turns) in q
            .haystack_session_ids
            .iter()
            .zip(q.haystack_sessions.iter())
        {
            if !seen_sids.insert(sid.clone()) {
                continue;
            }
            let text = flatten_session(turns);
            let mem_id = stable_id("mem", &format!("{}:{}", q.question_id, sid));
            let chunk_id = stable_id("chunk", &format!("{}:{}", q.question_id, sid));
            let m = Memory {
                id: mem_id,
                content: text.clone(),
                created_at: chrono_epoch,
                event_time: None,
                importance: 5,
                emotional_weight: 0,
                access_count: 0,
                last_accessed: None,
                stability: 1.0,
                source: MemorySource::Conversation {
                    thread: sid.clone(),
                    turn: 0,
                },
                diary: "lme_qa".into(),
                valid_from: None,
                valid_until: None,
                tags: vec![],
            };
            store.insert_memory(&m)?;
            store.insert_chunk(chunk_id, mem_id, &text, 0, None)?;
            chunk_to_session.insert(chunk_id, sid.clone());
            pending_chunks.push((chunk_id, text, sid.clone()));
        }

        // Embed haystack (not cached across questions — each question's store is temp).
        let texts: Vec<&str> = pending_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
        if !texts.is_empty() {
            let vecs = embedder
                .embed_batch(&texts, TaskMode::RetrievalDocument)
                .await?;
            for ((cid, _, _), v) in pending_chunks.iter().zip(vecs.into_iter()) {
                chunk_embeddings.insert(*cid, v);
            }
        }

        // Embed query + retrieve.
        let q_emb = embedder
            .embed_one(&q.question, TaskMode::RetrievalQuery)
            .await?;

        let mut dense_scored: Vec<(Uuid, f32)> = chunk_to_session
            .keys()
            .map(|cid| {
                let emb = chunk_embeddings.get(cid).expect("seeded");
                (*cid, cosine_sim(&q_emb, emb))
            })
            .collect();
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run: Vec<RankedHit> = dense_scored
            .iter()
            .take(50)
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Dense,
            })
            .collect();

        let fts_q = build_fts_query(&q.question);
        let fts_hits = if fts_q.is_empty() {
            Vec::new()
        } else {
            store.fts_search(&fts_q, 50).unwrap_or_default()
        };
        let lexical_run: Vec<RankedHit> = fts_hits
            .iter()
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Lexical,
            })
            .collect();

        let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);

        let top_ids: Vec<Uuid> = if let Some(r) = reranker {
            let cands: Vec<RerankCandidate> = fused
                .iter()
                .take(50)
                .filter_map(|(id, _)| {
                    pending_chunks
                        .iter()
                        .find(|(cid, _, _)| cid == id)
                        .map(|(cid, text, _)| RerankCandidate {
                            id: cid.to_string(),
                            text: text.clone(),
                        })
                })
                .collect();
            if cands.is_empty() {
                Vec::new()
            } else {
                let reranked = r.rerank(&q.question, &cands, top_k).await?;
                reranked
                    .into_iter()
                    .filter_map(|rr| Uuid::parse_str(&rr.id).ok())
                    .collect()
            }
        } else {
            fused.iter().take(top_k).map(|(id, _)| *id).collect()
        };

        let retrieved_sessions: Vec<String> = top_ids
            .iter()
            .filter_map(|id| chunk_to_session.get(id).cloned())
            .collect();

        // Build context for the answerer.
        let context: String = top_ids
            .iter()
            .enumerate()
            .filter_map(|(i, id)| {
                pending_chunks
                    .iter()
                    .find(|(cid, _, _)| cid == id)
                    .map(|(_, text, sid)| format!("[session {} — {}]\n{}", i + 1, sid, text))
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        // Answer.
        use engram_llm::ChatMessage;
        let answerer_msgs = vec![
            ChatMessage::system(ANSWERER_SYSTEM),
            ChatMessage::user(build_answerer_user(&q.question, &context)),
        ];
        let answer_resp = answerer
            .chat(&answerer_msgs)
            .await
            .map_err(|e| BenchError::InvalidDataset(format!("answerer LLM: {e}")))?;
        let candidate_answer = extract_answer_line(&answer_resp.content);
        answerer_prompt_tokens += answer_resp.prompt_tokens.unwrap_or(0) as u64;
        answerer_completion_tokens += answer_resp.completion_tokens.unwrap_or(0) as u64;

        // Judge.
        let verdict: JudgeVerdict = judge_answer(judge, &q.question, &q.answer, &candidate_answer)
            .await
            .map_err(|e| BenchError::InvalidDataset(format!("judge LLM: {e}")))?;
        judge_prompt_tokens += verdict.prompt_tokens.unwrap_or(0) as u64;
        judge_completion_tokens += verdict.completion_tokens.unwrap_or(0) as u64;
        if verdict.correct {
            total_correct += 1;
        }

        // Metrics.
        let r5 = recall_at_k(&retrieved_sessions, &q.answer_session_ids, 5);
        let rr = reciprocal_rank(&retrieved_sessions, &q.answer_session_ids, 10);
        total_recall5 += r5;
        total_mrr += rr;

        // Optional RAGAS (expensive — 4 more LLM calls per question).
        let ragas = if enable_ragas {
            compute_all(judge, &q.question, &q.answer, &candidate_answer, &context)
                .await
                .ok()
        } else {
            None
        };
        if let Some(ref r) = ragas {
            ragas_accum.faithfulness += r.faithfulness;
            ragas_accum.answer_relevance += r.answer_relevance;
            ragas_accum.context_precision += r.context_precision;
            ragas_accum.context_recall += r.context_recall;
            ragas_count += 1;
        }

        let latency_ms = run_start.elapsed().as_millis() as u64;
        latencies.push(latency_ms);

        tracing::info!(
            "[qa {}/{}] {} correct={} r5={:.2} latency={}ms",
            i + 1,
            n,
            q.question_type,
            verdict.correct,
            r5,
            latency_ms
        );

        let result = QaRunResult {
            question_id: q.question_id.clone(),
            question_type: q.question_type.clone(),
            question: q.question.clone(),
            gold_answer: q.answer.clone(),
            candidate_answer,
            correct: verdict.correct,
            recall_at_5: r5,
            mrr: rr,
            retrieved_sessions,
            answer_session_ids: q.answer_session_ids.clone(),
            ragas,
            latency_ms,
            answerer_prompt_tokens: answer_resp.prompt_tokens.unwrap_or(0),
            answerer_completion_tokens: answer_resp.completion_tokens.unwrap_or(0),
            judge_prompt_tokens: verdict.prompt_tokens.unwrap_or(0),
            judge_completion_tokens: verdict.completion_tokens.unwrap_or(0),
        };

        // Append-only checkpoint with fsync — see run_locomo_qa for rationale.
        if let Some(ref mut f) = checkpoint_file {
            let line = serde_json::to_string(&result).unwrap_or_default();
            if let Err(e) = writeln!(f, "{}", line).and_then(|_| f.sync_data()) {
                tracing::warn!("checkpoint write failed (continuing): {}", e);
            }
        }

        results.push(result);
    }

    let nf = n as f32;
    let accuracy = total_correct as f32 / nf.max(1.0);
    let r5 = total_recall5 / nf.max(1.0);
    let mrr = total_mrr / nf.max(1.0);
    let mean_lat = latencies.iter().sum::<u64>() as f32 / nf.max(1.0);
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let p50 = if sorted.is_empty() {
        0.0
    } else {
        sorted[sorted.len() / 2] as f32
    };
    let p95 = if sorted.is_empty() {
        0.0
    } else {
        sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32
    };

    let ragas = if ragas_count > 0 {
        let c = ragas_count as f32;
        Some(RagasMetrics {
            faithfulness: ragas_accum.faithfulness / c,
            answer_relevance: ragas_accum.answer_relevance / c,
            context_precision: ragas_accum.context_precision / c,
            context_recall: ragas_accum.context_recall / c,
        })
    } else {
        None
    };

    // Per question-type stats.
    let mut by_type: HashMap<String, QaTypeStats> = HashMap::new();
    for r in &results {
        let entry = by_type.entry(r.question_type.clone()).or_default();
        entry.total += 1;
        if r.correct {
            entry.correct += 1;
        }
    }
    for stats in by_type.values_mut() {
        stats.accuracy = stats.correct as f32 / stats.total.max(1) as f32;
    }

    Ok(QaReport {
        suite: "longmemeval_qa".into(),
        questions_evaluated: n,
        correct_count: total_correct,
        accuracy,
        recall_at_5: r5,
        mrr,
        ragas,
        mean_latency_ms: mean_lat,
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        answerer_total_prompt_tokens: answerer_prompt_tokens,
        answerer_total_completion_tokens: answerer_completion_tokens,
        judge_total_prompt_tokens: judge_prompt_tokens,
        judge_total_completion_tokens: judge_completion_tokens,
        per_question: results,
        by_question_type: by_type,
        by_source: HashMap::new(),
        unscored_count: 0,
        notes: Vec::new(),
    })
}

// Helper alias so callers don't have to import LongMemEvalQuestion.
pub type QaQuestion = LongMemEvalQuestion;

/// Run the full QA track on LoCoMo using engram's HYBRID retriever
/// (dense + FTS5 + RRF + optional Cohere rerank), not the simplified cosine-only
/// pipeline that was previously in `bench.rs`. Produces the same `QaReport`
/// format as `run_longmemeval_qa` so the two suites are apples-to-apples.
///
/// Each LoCoMo sample has one conversation (many session_N keys) and ~200 QAs
/// that share that haystack. We embed the haystack once per sample, build an
/// in-memory SqliteStore, then loop the QAs through hybrid retrieval.
#[allow(clippy::too_many_arguments)]
pub async fn run_locomo_qa<E, R, A, J>(
    dataset: &crate::locomo::LocomoDataset,
    embedder: &E,
    reranker: Option<&R>,
    answerer: &A,
    judge: &J,
    rrf_k: f32,
    top_k: usize,
    limit: Option<usize>,
    enable_ragas: bool,
    checkpoint_path: Option<std::path::PathBuf>,
) -> Result<QaReport, BenchError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
    A: ChatLlm + ?Sized,
    J: ChatLlm + ?Sized,
{
    use crate::locomo::flatten_conversation;
    use std::io::Write as _;
    let max_questions = limit.unwrap_or(usize::MAX);

    // Open checkpoint file in append mode if requested. We write one
    // QaRunResult as a JSONL line per completed question, flushed
    // immediately so a crash leaves a valid partial record on disk.
    let mut checkpoint_file: Option<std::fs::File> = if let Some(ref path) = checkpoint_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Truncate any prior checkpoint — caller can rename it if they
        // want to keep a previous run's partial results.
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Some(f)
    } else {
        None
    };

    let chrono_epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    use chrono::TimeZone;

    let mut results: Vec<QaRunResult> = Vec::new();
    let mut latencies: Vec<u64> = Vec::new();
    let mut ragas_accum = RagasMetrics::default();
    let mut ragas_count = 0usize;
    let mut total_correct = 0usize;
    let mut total_recall5 = 0f32;
    let mut total_mrr = 0f32;
    let mut answerer_prompt_tokens: u64 = 0;
    let mut answerer_completion_tokens: u64 = 0;
    let mut judge_prompt_tokens: u64 = 0;
    let mut judge_completion_tokens: u64 = 0;

    'samples: for (sample_idx, sample) in dataset.samples.iter().enumerate() {
        if results.len() >= max_questions {
            break;
        }
        let sessions = flatten_conversation(&sample.conversation);
        if sessions.is_empty() {
            continue;
        }

        // Build an in-memory store and seed it with this sample's sessions.
        let store = SqliteStore::open_in_memory()?;
        let mut chunk_to_session: HashMap<Uuid, String> = HashMap::new();
        let mut pending_chunks: Vec<(Uuid, String, String)> = Vec::new();
        let sample_key = sample
            .sample_id
            .clone()
            .unwrap_or_else(|| format!("sample{}", sample_idx));
        for (sid, text) in &sessions {
            let mem_id = stable_id("mem", &format!("{sample_key}:{sid}"));
            let chunk_id = stable_id("chunk", &format!("{sample_key}:{sid}"));
            let m = Memory {
                id: mem_id,
                content: text.clone(),
                created_at: chrono_epoch,
                event_time: None,
                importance: 5,
                emotional_weight: 0,
                access_count: 0,
                last_accessed: None,
                stability: 1.0,
                source: MemorySource::Conversation {
                    thread: sid.clone(),
                    turn: 0,
                },
                diary: "locomo_qa".into(),
                valid_from: None,
                valid_until: None,
                tags: vec![],
            };
            store.insert_memory(&m)?;
            store.insert_chunk(chunk_id, mem_id, text, 0, None)?;
            chunk_to_session.insert(chunk_id, sid.clone());
            pending_chunks.push((chunk_id, text.clone(), sid.clone()));
        }

        // Embed the haystack once per sample.
        let texts: Vec<&str> = pending_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
        let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
        if !texts.is_empty() {
            let vecs = embedder
                .embed_batch(&texts, TaskMode::RetrievalDocument)
                .await?;
            for ((cid, _, _), v) in pending_chunks.iter().zip(vecs.into_iter()) {
                chunk_embeddings.insert(*cid, v);
            }
        }

        for qa in &sample.qa {
            if results.len() >= max_questions {
                break 'samples;
            }
            // Skip adversarial-only entries (no gold answer).
            let gold = match qa.answer.as_deref() {
                Some(a) if !a.is_empty() => a.to_string(),
                _ => continue,
            };

            let run_start = Instant::now();

            // Dense retrieval.
            let q_emb = embedder
                .embed_one(&qa.question, TaskMode::RetrievalQuery)
                .await?;
            let mut dense_scored: Vec<(Uuid, f32)> = chunk_to_session
                .keys()
                .map(|cid| {
                    let emb = chunk_embeddings.get(cid).expect("seeded");
                    (*cid, cosine_sim(&q_emb, emb))
                })
                .collect();
            dense_scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            let dense_run: Vec<RankedHit> = dense_scored
                .iter()
                .take(50)
                .enumerate()
                .map(|(i, (id, score))| RankedHit {
                    chunk_id: *id,
                    rank: i + 1,
                    raw_score: *score,
                    source: RetrievalSource::Dense,
                })
                .collect();

            // Lexical retrieval.
            let fts_q = build_fts_query(&qa.question);
            let fts_hits = if fts_q.is_empty() {
                Vec::new()
            } else {
                store.fts_search(&fts_q, 50).unwrap_or_default()
            };
            let lexical_run: Vec<RankedHit> = fts_hits
                .iter()
                .enumerate()
                .map(|(i, (id, score))| RankedHit {
                    chunk_id: *id,
                    rank: i + 1,
                    raw_score: *score,
                    source: RetrievalSource::Lexical,
                })
                .collect();

            // RRF fusion.
            let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);

            // Optional Cohere rerank.
            let top_ids: Vec<Uuid> = if let Some(r) = reranker {
                let cands: Vec<RerankCandidate> = fused
                    .iter()
                    .take(50)
                    .filter_map(|(id, _)| {
                        pending_chunks
                            .iter()
                            .find(|(cid, _, _)| cid == id)
                            .map(|(cid, text, _)| RerankCandidate {
                                id: cid.to_string(),
                                text: text.clone(),
                            })
                    })
                    .collect();
                if cands.is_empty() {
                    Vec::new()
                } else {
                    let reranked = r.rerank(&qa.question, &cands, top_k).await?;
                    reranked
                        .into_iter()
                        .filter_map(|rr| Uuid::parse_str(&rr.id).ok())
                        .collect()
                }
            } else {
                fused.iter().take(top_k).map(|(id, _)| *id).collect()
            };

            let retrieved_sessions: Vec<String> = top_ids
                .iter()
                .filter_map(|id| chunk_to_session.get(id).cloned())
                .collect();

            // Build context for the answerer.
            // Each chunk text already has a header like
            // "[session_3 — 2:30 pm on 12 May, 2023]\n..." from
            // flatten_conversation, so we pass it as-is to avoid
            // duplicate/confusing headers.
            let context: String = top_ids
                .iter()
                .filter_map(|id| {
                    pending_chunks
                        .iter()
                        .find(|(cid, _, _)| cid == id)
                        .map(|(_, text, _)| text.as_str())
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            // Answer.
            use engram_llm::ChatMessage;
            let answerer_msgs = vec![
                ChatMessage::system(ANSWERER_SYSTEM),
                ChatMessage::user(build_answerer_user(&qa.question, &context)),
            ];
            let answer_resp = answerer
                .chat(&answerer_msgs)
                .await
                .map_err(|e| BenchError::InvalidDataset(format!("answerer LLM: {e}")))?;
            let candidate_answer = extract_answer_line(&answer_resp.content);
            answerer_prompt_tokens += answer_resp.prompt_tokens.unwrap_or(0) as u64;
            answerer_completion_tokens += answer_resp.completion_tokens.unwrap_or(0) as u64;

            // Judge.
            let verdict: JudgeVerdict = judge_answer(judge, &qa.question, &gold, &candidate_answer)
                .await
                .map_err(|e| BenchError::InvalidDataset(format!("judge LLM: {e}")))?;
            judge_prompt_tokens += verdict.prompt_tokens.unwrap_or(0) as u64;
            judge_completion_tokens += verdict.completion_tokens.unwrap_or(0) as u64;
            if verdict.correct {
                total_correct += 1;
            }

            // LoCoMo carries gold evidence as `D{N}:{turn}` references.
            // Map these to session IDs (D1 → session_1) and compute R@5
            // against the retrieved_sessions list.
            //
            // Non-standard formats in the dataset:
            //   "D8:6; D9:17"          → semicolon-separated
            //   "D9:1 D4:4 D4:6"       → space-separated
            //   "D"                     → bare D (skip)
            //   "D:11:26"              → malformed (skip)
            // We split on whitespace and semicolons first, then parse each token.
            let gold_sessions: std::collections::HashSet<String> = qa
                .evidence
                .iter()
                .flat_map(|e| {
                    e.split(|c: char| c == ';' || c.is_whitespace())
                        .filter(|tok| !tok.is_empty())
                })
                .filter_map(|tok| {
                    // "D1:3" → "session_1", "D1" → "session_1"
                    let d_part = tok.split(':').next()?;
                    let n_str = d_part.strip_prefix('D')?;
                    let n: u32 = n_str.parse().ok()?;
                    Some(format!("session_{n}"))
                })
                .collect();
            // R@k = fraction of gold sessions found in the retrieved set.
            // For single-evidence questions this is binary (0 or 1). For
            // multi-hop questions with N gold sessions it rewards partial
            // retrieval (e.g. 2/3 gold sessions found = 0.667).
            let r5 = if gold_sessions.is_empty() {
                0f32
            } else {
                let found = gold_sessions
                    .iter()
                    .filter(|g| retrieved_sessions.contains(g))
                    .count();
                found as f32 / gold_sessions.len() as f32
            };
            let rr = if gold_sessions.is_empty() {
                0f32
            } else {
                retrieved_sessions
                    .iter()
                    .enumerate()
                    .find(|(_, s)| gold_sessions.contains(s.as_str()))
                    .map(|(rank, _)| 1.0 / (rank as f32 + 1.0))
                    .unwrap_or(0.0)
            };
            total_recall5 += r5;
            total_mrr += rr;

            let ragas = if enable_ragas {
                compute_all(judge, &qa.question, &gold, &candidate_answer, &context)
                    .await
                    .ok()
            } else {
                None
            };
            if let Some(ref r) = ragas {
                ragas_accum.faithfulness += r.faithfulness;
                ragas_accum.answer_relevance += r.answer_relevance;
                ragas_accum.context_precision += r.context_precision;
                ragas_accum.context_recall += r.context_recall;
                ragas_count += 1;
            }

            let latency_ms = run_start.elapsed().as_millis() as u64;
            latencies.push(latency_ms);

            let category_label = qa
                .category
                .map(|c| format!("category_{c}"))
                .unwrap_or_else(|| "uncategorized".to_string());

            tracing::info!(
                "[locomo-qa {}] {} correct={} latency={}ms",
                results.len() + 1,
                category_label,
                verdict.correct,
                latency_ms
            );

            let result = QaRunResult {
                question_id: format!("{sample_key}:{}", results.len()),
                question_type: category_label,
                question: qa.question.clone(),
                gold_answer: gold,
                candidate_answer,
                correct: verdict.correct,
                recall_at_5: r5,
                mrr: rr,
                retrieved_sessions,
                answer_session_ids: gold_sessions.into_iter().collect(),
                ragas,
                latency_ms,
                answerer_prompt_tokens: answer_resp.prompt_tokens.unwrap_or(0),
                answerer_completion_tokens: answer_resp.completion_tokens.unwrap_or(0),
                judge_prompt_tokens: verdict.prompt_tokens.unwrap_or(0),
                judge_completion_tokens: verdict.completion_tokens.unwrap_or(0),
            };

            // Append the result to the checkpoint JSONL immediately and
            // fsync so a crash leaves a valid partial record on disk.
            // Best-effort: a checkpoint write failure is logged but does
            // NOT abort the bench (we'd rather lose checkpoint coverage
            // than the whole run).
            if let Some(ref mut f) = checkpoint_file {
                let line = serde_json::to_string(&result).unwrap_or_default();
                if let Err(e) = writeln!(f, "{}", line).and_then(|_| f.sync_data()) {
                    tracing::warn!("checkpoint write failed (continuing): {}", e);
                }
            }

            results.push(result);
        }
    }

    let n = results.len();
    let nf = n as f32;
    let accuracy = total_correct as f32 / nf.max(1.0);
    let r5 = total_recall5 / nf.max(1.0);
    let mrr = total_mrr / nf.max(1.0);
    let mean_lat = if latencies.is_empty() {
        0.0
    } else {
        latencies.iter().sum::<u64>() as f32 / nf.max(1.0)
    };
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let p50 = if sorted.is_empty() {
        0.0
    } else {
        sorted[sorted.len() / 2] as f32
    };
    let p95 = if sorted.is_empty() {
        0.0
    } else {
        sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32
    };

    let ragas = if ragas_count > 0 {
        let c = ragas_count as f32;
        Some(RagasMetrics {
            faithfulness: ragas_accum.faithfulness / c,
            answer_relevance: ragas_accum.answer_relevance / c,
            context_precision: ragas_accum.context_precision / c,
            context_recall: ragas_accum.context_recall / c,
        })
    } else {
        None
    };

    // Per-category stats (LoCoMo's version of by_question_type).
    let mut by_type: HashMap<String, QaTypeStats> = HashMap::new();
    for r in &results {
        let entry = by_type.entry(r.question_type.clone()).or_default();
        entry.total += 1;
        if r.correct {
            entry.correct += 1;
        }
    }
    for stats in by_type.values_mut() {
        stats.accuracy = stats.correct as f32 / stats.total.max(1) as f32;
    }

    Ok(QaReport {
        suite: "locomo_qa".into(),
        questions_evaluated: n,
        correct_count: total_correct,
        accuracy,
        recall_at_5: r5,
        mrr,
        ragas,
        mean_latency_ms: mean_lat,
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        answerer_total_prompt_tokens: answerer_prompt_tokens,
        answerer_total_completion_tokens: answerer_completion_tokens,
        judge_total_prompt_tokens: judge_prompt_tokens,
        judge_total_completion_tokens: judge_completion_tokens,
        per_question: results,
        by_question_type: by_type,
        by_source: HashMap::new(),
        unscored_count: 0,
        notes: Vec::new(),
    })
}

/// Run the LoCoMo-Plus cognitive cue/trigger benchmark. Each cognitive entry is
/// stitched into a LoCoMo conversation chosen by `i % locomo.samples.len()`.
/// The haystack contains the original sessions plus the cue pseudo-session; the
/// trigger is used only as the query.
#[allow(clippy::too_many_arguments)]
pub async fn run_locomo_plus_qa<E, R, A, J>(
    dataset: &crate::locomo_plus::LocomoPlusDataset,
    locomo: &crate::locomo::LocomoDataset,
    embedder: &E,
    reranker: Option<&R>,
    answerer: &A,
    judge: &J,
    rrf_k: f32,
    top_k: usize,
    limit: Option<usize>,
    enable_ragas: bool,
    checkpoint_path: Option<std::path::PathBuf>,
) -> Result<QaReport, BenchError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
    A: ChatLlm + ?Sized,
    J: ChatLlm + ?Sized,
{
    use crate::locomo_plus::build_stitched_dialogue;
    use std::io::Write as _;

    if locomo.samples.is_empty() {
        return Err(BenchError::InvalidDataset("0 LoCoMo samples".into()));
    }

    let n = limit
        .map(|l| l.min(dataset.entries.len()))
        .unwrap_or(dataset.entries.len());
    let mut checkpoint_file: Option<std::fs::File> = if let Some(ref path) = checkpoint_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Some(f)
    } else {
        None
    };

    let chrono_epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    use chrono::TimeZone;

    let mut results = Vec::with_capacity(n);
    let mut latencies = Vec::with_capacity(n);
    let mut ragas_accum = RagasMetrics::default();
    let mut ragas_count = 0usize;
    let mut total_correct = 0usize;
    let mut total_recall5 = 0f32;
    let mut total_mrr = 0f32;
    let mut answerer_prompt_tokens: u64 = 0;
    let mut answerer_completion_tokens: u64 = 0;
    let mut judge_prompt_tokens: u64 = 0;
    let mut judge_completion_tokens: u64 = 0;
    let mut cue_missed: usize = 0;

    for (i, entry) in dataset.entries.iter().take(n).enumerate() {
        let run_start = Instant::now();
        let sample = &locomo.samples[i % locomo.samples.len()];
        let stitched = build_stitched_dialogue(entry, sample);
        if stitched.sessions.is_empty() {
            continue;
        }
        let question = if stitched.trigger.trim().is_empty() {
            entry.trigger_query.clone()
        } else {
            stitched.trigger.clone()
        };
        let evidence = if stitched.evidence_text.trim().is_empty() {
            entry.cue_dialogue.clone()
        } else {
            stitched.evidence_text.clone()
        };

        let store = SqliteStore::open_in_memory()?;
        let mut chunk_to_session: HashMap<Uuid, String> = HashMap::new();
        let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
        let mut pending_chunks: Vec<(Uuid, String, String)> = Vec::new();
        let sample_key = sample
            .sample_id
            .clone()
            .unwrap_or_else(|| format!("sample{}", i % locomo.samples.len()));

        for (sid, text) in &stitched.sessions {
            let key = format!("locomo_plus:{i}:{sample_key}:{sid}");
            let mem_id = stable_id("mem", &key);
            let chunk_id = stable_id("chunk", &key);
            let m = Memory {
                id: mem_id,
                content: text.clone(),
                created_at: chrono_epoch,
                event_time: None,
                importance: 5,
                emotional_weight: 0,
                access_count: 0,
                last_accessed: None,
                stability: 1.0,
                source: MemorySource::Conversation {
                    thread: sid.clone(),
                    turn: 0,
                },
                diary: "locomo_plus_qa".into(),
                valid_from: None,
                valid_until: None,
                tags: vec![],
            };
            store.insert_memory(&m)?;
            store.insert_chunk(chunk_id, mem_id, text, 0, None)?;
            chunk_to_session.insert(chunk_id, sid.clone());
            pending_chunks.push((chunk_id, text.clone(), sid.clone()));
        }

        let texts: Vec<&str> = pending_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
        if !texts.is_empty() {
            let vecs = embedder
                .embed_batch(&texts, TaskMode::RetrievalDocument)
                .await?;
            for ((cid, _, _), v) in pending_chunks.iter().zip(vecs.into_iter()) {
                chunk_embeddings.insert(*cid, v);
            }
        }

        let q_emb = embedder
            .embed_one(&question, TaskMode::RetrievalQuery)
            .await?;
        let mut dense_scored: Vec<(Uuid, f32)> = chunk_to_session
            .keys()
            .map(|cid| {
                let emb = chunk_embeddings.get(cid).expect("seeded");
                (*cid, cosine_sim(&q_emb, emb))
            })
            .collect();
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run: Vec<RankedHit> = dense_scored
            .iter()
            .take(50)
            .enumerate()
            .map(|(rank, (id, score))| RankedHit {
                chunk_id: *id,
                rank: rank + 1,
                raw_score: *score,
                source: RetrievalSource::Dense,
            })
            .collect();

        let fts_q = build_fts_query(&question);
        let fts_hits = if fts_q.is_empty() {
            Vec::new()
        } else {
            store.fts_search(&fts_q, 50).unwrap_or_default()
        };
        let lexical_run: Vec<RankedHit> = fts_hits
            .iter()
            .enumerate()
            .map(|(rank, (id, score))| RankedHit {
                chunk_id: *id,
                rank: rank + 1,
                raw_score: *score,
                source: RetrievalSource::Lexical,
            })
            .collect();

        let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);
        let top_ids: Vec<Uuid> = if let Some(r) = reranker {
            let cands: Vec<RerankCandidate> = fused
                .iter()
                .take(50)
                .filter_map(|(id, _)| {
                    pending_chunks
                        .iter()
                        .find(|(cid, _, _)| cid == id)
                        .map(|(cid, text, _)| RerankCandidate {
                            id: cid.to_string(),
                            text: text.clone(),
                        })
                })
                .collect();
            if cands.is_empty() {
                Vec::new()
            } else {
                r.rerank(&question, &cands, top_k)
                    .await?
                    .into_iter()
                    .filter_map(|rr| Uuid::parse_str(&rr.id).ok())
                    .collect()
            }
        } else {
            fused.iter().take(top_k).map(|(id, _)| *id).collect()
        };

        let retrieved_sessions: Vec<String> = top_ids
            .iter()
            .filter_map(|id| chunk_to_session.get(id).cloned())
            .collect();
        let context: String = top_ids
            .iter()
            .filter_map(|id| {
                pending_chunks
                    .iter()
                    .find(|(cid, _, _)| cid == id)
                    .map(|(_, text, _)| text.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        use engram_llm::ChatMessage;
        let answerer_msgs = vec![
            ChatMessage::system(ANSWERER_SYSTEM),
            ChatMessage::user(build_answerer_user(&question, &context)),
        ];
        let answer_resp = answerer
            .chat(&answerer_msgs)
            .await
            .map_err(|e| BenchError::InvalidDataset(format!("answerer LLM: {e}")))?;
        let candidate_answer = extract_answer_line(&answer_resp.content);
        answerer_prompt_tokens += answer_resp.prompt_tokens.unwrap_or(0) as u64;
        answerer_completion_tokens += answer_resp.completion_tokens.unwrap_or(0) as u64;

        let verdict = judge_answer_cognitive(judge, &evidence, &candidate_answer)
            .await
            .map_err(|e| BenchError::InvalidDataset(format!("judge LLM: {e}")))?;
        judge_prompt_tokens += verdict.prompt_tokens.unwrap_or(0) as u64;
        judge_completion_tokens += verdict.completion_tokens.unwrap_or(0) as u64;
        if verdict.correct {
            total_correct += 1;
        }

        let answer_session_ids: Vec<String> = stitched
            .sessions
            .iter()
            .filter(|(sid, _)| sid.starts_with("cue_for_"))
            .map(|(sid, _)| sid.clone())
            .collect();
        let r5 = recall_at_k(&retrieved_sessions, &answer_session_ids, 5);
        let rr = reciprocal_rank(&retrieved_sessions, &answer_session_ids, 10);
        let cue_retrieved = r5 > 0.0 || answer_session_ids.is_empty();
        if !cue_retrieved {
            cue_missed += 1;
        }
        total_recall5 += r5;
        total_mrr += rr;

        let ragas = if enable_ragas {
            compute_all(judge, &question, &evidence, &candidate_answer, &context)
                .await
                .ok()
        } else {
            None
        };
        if let Some(ref r) = ragas {
            ragas_accum.faithfulness += r.faithfulness;
            ragas_accum.answer_relevance += r.answer_relevance;
            ragas_accum.context_precision += r.context_precision;
            ragas_accum.context_recall += r.context_recall;
            ragas_count += 1;
        }

        let latency_ms = run_start.elapsed().as_millis() as u64;
        latencies.push(latency_ms);
        tracing::info!(
            "[locomo-plus {} / {}] {} correct={} cue_retrieved={} r5={:.2} latency={}ms",
            i + 1,
            n,
            entry.relation_type,
            verdict.correct,
            cue_retrieved,
            r5,
            latency_ms
        );

        let result = QaRunResult {
            question_id: format!("locomo_plus:{i}:host={sample_key}"),
            question_type: entry.relation_type.clone(),
            question,
            gold_answer: entry.cue_dialogue.clone(),
            candidate_answer,
            correct: verdict.correct,
            recall_at_5: r5,
            mrr: rr,
            retrieved_sessions,
            answer_session_ids,
            ragas,
            latency_ms,
            answerer_prompt_tokens: answer_resp.prompt_tokens.unwrap_or(0),
            answerer_completion_tokens: answer_resp.completion_tokens.unwrap_or(0),
            judge_prompt_tokens: verdict.prompt_tokens.unwrap_or(0),
            judge_completion_tokens: verdict.completion_tokens.unwrap_or(0),
        };

        if let Some(ref mut f) = checkpoint_file {
            let line = serde_json::to_string(&result).unwrap_or_default();
            if let Err(e) = writeln!(f, "{}", line).and_then(|_| f.sync_data()) {
                tracing::warn!("checkpoint write failed (continuing): {}", e);
            }
        }
        results.push(result);
    }

    let scored = results.len();
    let nf = scored as f32;
    let accuracy = total_correct as f32 / nf.max(1.0);
    let r5 = total_recall5 / nf.max(1.0);
    let mrr = total_mrr / nf.max(1.0);
    let mean_lat = if latencies.is_empty() {
        0.0
    } else {
        latencies.iter().sum::<u64>() as f32 / nf.max(1.0)
    };
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let p50 = if sorted.is_empty() {
        0.0
    } else {
        sorted[sorted.len() / 2] as f32
    };
    let p95 = if sorted.is_empty() {
        0.0
    } else {
        sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32
    };

    let ragas = if ragas_count > 0 {
        let c = ragas_count as f32;
        Some(RagasMetrics {
            faithfulness: ragas_accum.faithfulness / c,
            answer_relevance: ragas_accum.answer_relevance / c,
            context_precision: ragas_accum.context_precision / c,
            context_recall: ragas_accum.context_recall / c,
        })
    } else {
        None
    };

    let mut by_type: HashMap<String, QaTypeStats> = HashMap::new();
    for r in &results {
        let entry = by_type.entry(r.question_type.clone()).or_default();
        entry.total += 1;
        if r.correct {
            entry.correct += 1;
        }
    }
    for stats in by_type.values_mut() {
        stats.accuracy = stats.correct as f32 / stats.total.max(1) as f32;
    }

    let cue_miss_pct = if scored > 0 {
        cue_missed as f32 / scored as f32 * 100.0
    } else {
        0.0
    };
    tracing::info!(
        "locomo-plus summary: cue_missed={}/{} ({:.1}%)",
        cue_missed,
        scored,
        cue_miss_pct
    );
    let mut notes = Vec::new();
    if cue_missed > 0 {
        notes.push(format!(
            "cue_missed={}/{} ({:.1}%) — reranker failed to surface cue pseudo-session",
            cue_missed, scored, cue_miss_pct
        ));
    }

    Ok(QaReport {
        suite: "locomo_plus_qa".into(),
        questions_evaluated: scored,
        correct_count: total_correct,
        accuracy,
        recall_at_5: r5,
        mrr,
        ragas,
        mean_latency_ms: mean_lat,
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        answerer_total_prompt_tokens: answerer_prompt_tokens,
        answerer_total_completion_tokens: answerer_completion_tokens,
        judge_total_prompt_tokens: judge_prompt_tokens,
        judge_total_completion_tokens: judge_completion_tokens,
        per_question: results,
        by_question_type: by_type,
        by_source: HashMap::new(),
        unscored_count: 0,
        notes,
    })
}

/// Run MemoryAgentBench QA over one JSONL split. Each row gets one temporary
/// memory state seeded with ~1KB context chunks, then all questions for that row
/// reuse the same embeddings and store.
#[allow(clippy::too_many_arguments)]
pub async fn run_memoryagentbench_qa<E, R, A, J>(
    dataset: &crate::memoryagentbench::MabDataset,
    embedder: &E,
    reranker: Option<&R>,
    answerer: &A,
    judge: &J,
    rrf_k: f32,
    top_k: usize,
    limit: Option<usize>,
    checkpoint_path: Option<std::path::PathBuf>,
) -> Result<QaReport, BenchError>
where
    E: Embedder + ?Sized,
    R: Reranker + ?Sized,
    A: ChatLlm + ?Sized,
    J: ChatLlm + ?Sized,
{
    use std::io::Write as _;

    let max_questions = limit.unwrap_or(usize::MAX);
    let mut questions_seen = 0usize;
    let mut unscored_count = 0usize;
    let mut notes = Vec::new();
    let mut checkpoint_file: Option<std::fs::File> = if let Some(ref path) = checkpoint_path {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        Some(f)
    } else {
        None
    };

    let chrono_epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();
    use chrono::TimeZone;

    let mut results: Vec<QaRunResult> = Vec::new();
    let mut latencies: Vec<u64> = Vec::new();
    let mut total_correct = 0usize;
    let mut total_recall5 = 0f32;
    let mut total_mrr = 0f32;
    let mut answerer_prompt_tokens: u64 = 0;
    let mut answerer_completion_tokens: u64 = 0;
    let mut judge_prompt_tokens: u64 = 0;
    let mut judge_completion_tokens: u64 = 0;
    let mut by_source: HashMap<String, QaTypeStats> = HashMap::new();

    'rows: for (row_idx, row) in dataset.rows.iter().enumerate() {
        if questions_seen >= max_questions {
            break;
        }

        if row
            .metadata
            .keypoints
            .as_ref()
            .map(|k| !k.is_empty())
            .unwrap_or(false)
        {
            let remaining = max_questions.saturating_sub(questions_seen);
            let skipped = row.questions.len().min(remaining);
            questions_seen += skipped;
            unscored_count += skipped;
            if !notes.iter().any(|n| n == "Long_Range_Understanding keypoint-summary rows are unscored; F1 over keypoints is not implemented yet.") {
                notes.push("Long_Range_Understanding keypoint-summary rows are unscored; F1 over keypoints is not implemented yet.".to_string());
            }
            continue;
        }

        let chunks = chunk_mab_context(&row.context);
        if chunks.is_empty() {
            continue;
        }

        let store = SqliteStore::open_in_memory()?;
        let mut chunk_to_session: HashMap<Uuid, String> = HashMap::new();
        let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();
        let mut pending_chunks: Vec<(Uuid, String, String)> = Vec::new();
        let row_key = format!("{}:row{row_idx}", dataset.split.name());

        for (chunk_idx, text) in chunks.iter().enumerate() {
            let sid = format!("row{row_idx}_chunk{chunk_idx}");
            let key = format!("{row_key}:{sid}");
            let mem_id = stable_id("mem", &key);
            let chunk_id = stable_id("chunk", &key);
            let m = Memory {
                id: mem_id,
                content: text.clone(),
                created_at: chrono_epoch,
                event_time: None,
                importance: 5,
                emotional_weight: 0,
                access_count: 0,
                last_accessed: None,
                stability: 1.0,
                source: MemorySource::Conversation {
                    thread: sid.clone(),
                    turn: 0,
                },
                diary: "memoryagentbench_qa".into(),
                valid_from: None,
                valid_until: None,
                tags: vec![],
            };
            store.insert_memory(&m)?;
            store.insert_chunk(chunk_id, mem_id, text, chunk_idx as u32, None)?;
            chunk_to_session.insert(chunk_id, sid.clone());
            pending_chunks.push((chunk_id, text.clone(), sid));
        }

        let texts: Vec<&str> = pending_chunks.iter().map(|(_, t, _)| t.as_str()).collect();
        if !texts.is_empty() {
            let vecs = embedder
                .embed_batch(&texts, TaskMode::RetrievalDocument)
                .await?;
            for ((cid, _, _), v) in pending_chunks.iter().zip(vecs.into_iter()) {
                chunk_embeddings.insert(*cid, v);
            }
        }

        for (q_idx, question) in row.questions.iter().enumerate() {
            if questions_seen >= max_questions {
                break 'rows;
            }
            questions_seen += 1;
            let run_start = Instant::now();
            let question_type = row
                .metadata
                .question_types
                .as_ref()
                .and_then(|v| v.get(q_idx))
                .cloned()
                .unwrap_or_else(|| "factual".to_string());
            let source = row
                .metadata
                .source
                .clone()
                .unwrap_or_else(|| dataset.split.name().to_string());
            let question_id = row
                .metadata
                .question_ids
                .as_ref()
                .and_then(|v| v.get(q_idx))
                .or_else(|| row.metadata.qa_pair_ids.as_ref().and_then(|v| v.get(q_idx)))
                .cloned()
                .unwrap_or_else(|| format!("{}:row{row_idx}:q{q_idx}", dataset.split.name()));
            let answers = row.answers.get(q_idx).cloned().unwrap_or_default();

            let q_emb = embedder
                .embed_one(question, TaskMode::RetrievalQuery)
                .await?;
            let mut dense_scored: Vec<(Uuid, f32)> = chunk_to_session
                .keys()
                .map(|cid| {
                    let emb = chunk_embeddings.get(cid).expect("seeded");
                    (*cid, cosine_sim(&q_emb, emb))
                })
                .collect();
            dense_scored.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            let dense_run: Vec<RankedHit> = dense_scored
                .iter()
                .take(50)
                .enumerate()
                .map(|(rank, (id, score))| RankedHit {
                    chunk_id: *id,
                    rank: rank + 1,
                    raw_score: *score,
                    source: RetrievalSource::Dense,
                })
                .collect();

            let fts_q = build_fts_query(question);
            let fts_hits = if fts_q.is_empty() {
                Vec::new()
            } else {
                store.fts_search(&fts_q, 50).unwrap_or_default()
            };
            let lexical_run: Vec<RankedHit> = fts_hits
                .iter()
                .enumerate()
                .map(|(rank, (id, score))| RankedHit {
                    chunk_id: *id,
                    rank: rank + 1,
                    raw_score: *score,
                    source: RetrievalSource::Lexical,
                })
                .collect();

            let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);
            let top_ids: Vec<Uuid> = if let Some(r) = reranker {
                let cands: Vec<RerankCandidate> = fused
                    .iter()
                    .take(50)
                    .filter_map(|(id, _)| {
                        pending_chunks
                            .iter()
                            .find(|(cid, _, _)| cid == id)
                            .map(|(cid, text, _)| RerankCandidate {
                                id: cid.to_string(),
                                text: text.clone(),
                            })
                    })
                    .collect();
                if cands.is_empty() {
                    Vec::new()
                } else {
                    r.rerank(question, &cands, top_k)
                        .await?
                        .into_iter()
                        .filter_map(|rr| Uuid::parse_str(&rr.id).ok())
                        .collect()
                }
            } else {
                fused.iter().take(top_k).map(|(id, _)| *id).collect()
            };

            let retrieved_sessions: Vec<String> = top_ids
                .iter()
                .filter_map(|id| chunk_to_session.get(id).cloned())
                .collect();
            let context: String = top_ids
                .iter()
                .enumerate()
                .filter_map(|(rank, id)| {
                    pending_chunks
                        .iter()
                        .find(|(cid, _, _)| cid == id)
                        .map(|(_, text, sid)| format!("[chunk {} — {}]\n{}", rank + 1, sid, text))
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            use engram_llm::ChatMessage;
            let answerer_msgs = vec![
                ChatMessage::system(ANSWERER_SYSTEM),
                ChatMessage::user(build_answerer_user(question, &context)),
            ];
            let answer_resp = answerer
                .chat(&answerer_msgs)
                .await
                .map_err(|e| BenchError::InvalidDataset(format!("answerer LLM: {e}")))?;
            let candidate_answer = extract_answer_line(&answer_resp.content);
            answerer_prompt_tokens += answer_resp.prompt_tokens.unwrap_or(0) as u64;
            answerer_completion_tokens += answer_resp.completion_tokens.unwrap_or(0) as u64;

            let verdict =
                judge_answer_mab(judge, question, &answers, &candidate_answer, &question_type)
                    .await
                    .map_err(|e| BenchError::InvalidDataset(format!("judge LLM: {e}")))?;
            judge_prompt_tokens += verdict.prompt_tokens.unwrap_or(0) as u64;
            judge_completion_tokens += verdict.completion_tokens.unwrap_or(0) as u64;
            if verdict.correct {
                total_correct += 1;
            }

            let r5 = 0.0;
            let rr = 0.0;
            total_recall5 += r5;
            total_mrr += rr;
            let latency_ms = run_start.elapsed().as_millis() as u64;
            latencies.push(latency_ms);

            let source_entry = by_source.entry(source.clone()).or_default();
            source_entry.total += 1;
            if verdict.correct {
                source_entry.correct += 1;
            }

            tracing::info!(
                "[mab {} {}] row={} q={} type={} source={} correct={} latency={}ms",
                dataset.split.name(),
                results.len() + 1,
                row_idx,
                q_idx,
                question_type,
                source,
                verdict.correct,
                latency_ms
            );

            let result = QaRunResult {
                question_id,
                question_type,
                question: question.clone(),
                gold_answer: answers.join(" OR "),
                candidate_answer,
                correct: verdict.correct,
                recall_at_5: r5,
                mrr: rr,
                retrieved_sessions,
                answer_session_ids: Vec::new(),
                ragas: None,
                latency_ms,
                answerer_prompt_tokens: answer_resp.prompt_tokens.unwrap_or(0),
                answerer_completion_tokens: answer_resp.completion_tokens.unwrap_or(0),
                judge_prompt_tokens: verdict.prompt_tokens.unwrap_or(0),
                judge_completion_tokens: verdict.completion_tokens.unwrap_or(0),
            };

            if let Some(ref mut f) = checkpoint_file {
                let line = serde_json::to_string(&result).unwrap_or_default();
                if let Err(e) = writeln!(f, "{}", line).and_then(|_| f.sync_data()) {
                    tracing::warn!("checkpoint write failed (continuing): {}", e);
                }
            }
            results.push(result);
        }
    }

    let scored = results.len();
    let nf = scored as f32;
    let accuracy = total_correct as f32 / nf.max(1.0);
    let r5 = total_recall5 / nf.max(1.0);
    let mrr = total_mrr / nf.max(1.0);
    let mean_lat = if latencies.is_empty() {
        0.0
    } else {
        latencies.iter().sum::<u64>() as f32 / nf.max(1.0)
    };
    let mut sorted = latencies.clone();
    sorted.sort_unstable();
    let p50 = if sorted.is_empty() {
        0.0
    } else {
        sorted[sorted.len() / 2] as f32
    };
    let p95 = if sorted.is_empty() {
        0.0
    } else {
        sorted[(sorted.len() * 95 / 100).min(sorted.len() - 1)] as f32
    };

    let mut by_type: HashMap<String, QaTypeStats> = HashMap::new();
    for r in &results {
        let entry = by_type.entry(r.question_type.clone()).or_default();
        entry.total += 1;
        if r.correct {
            entry.correct += 1;
        }
    }
    for stats in by_type.values_mut() {
        stats.accuracy = stats.correct as f32 / stats.total.max(1) as f32;
    }
    for stats in by_source.values_mut() {
        stats.accuracy = stats.correct as f32 / stats.total.max(1) as f32;
    }

    Ok(QaReport {
        suite: "memoryagentbench_qa".into(),
        questions_evaluated: scored,
        correct_count: total_correct,
        accuracy,
        recall_at_5: r5,
        mrr,
        ragas: None,
        mean_latency_ms: mean_lat,
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        answerer_total_prompt_tokens: answerer_prompt_tokens,
        answerer_total_completion_tokens: answerer_completion_tokens,
        judge_total_prompt_tokens: judge_prompt_tokens,
        judge_total_completion_tokens: judge_completion_tokens,
        per_question: results,
        by_question_type: by_type,
        by_source,
        unscored_count,
        notes,
    })
}

fn chunk_mab_context(text: &str) -> Vec<String> {
    const TARGET_CHARS: usize = 1024;

    fn flush(chunks: &mut Vec<String>, buf: &mut String) {
        let trimmed = buf.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
        buf.clear();
    }

    fn split_long(chunks: &mut Vec<String>, text: &str, target: usize) {
        let mut start = 0usize;
        let mut count = 0usize;
        for (idx, _) in text.char_indices() {
            if count == target {
                let piece = text[start..idx].trim();
                if !piece.is_empty() {
                    chunks.push(piece.to_string());
                }
                start = idx;
                count = 0;
            }
            count += 1;
        }
        let piece = text[start..].trim();
        if !piece.is_empty() {
            chunks.push(piece.to_string());
        }
    }

    fn append_piece(chunks: &mut Vec<String>, buf: &mut String, piece: &str, target: usize) {
        let piece = piece.trim();
        if piece.is_empty() {
            return;
        }
        if piece.chars().count() > target {
            if piece.contains('\n') {
                for line in piece.lines() {
                    append_piece(chunks, buf, line, target);
                }
            } else {
                flush(chunks, buf);
                split_long(chunks, piece, target);
            }
            return;
        }
        if !buf.is_empty() && buf.chars().count() + piece.chars().count() + 2 > target {
            flush(chunks, buf);
        }
        if !buf.is_empty() {
            buf.push_str("\n\n");
        }
        buf.push_str(piece);
    }

    let mut chunks = Vec::new();
    let mut buf = String::new();
    for paragraph in text.split("\n\n") {
        append_piece(&mut chunks, &mut buf, paragraph, TARGET_CHARS);
    }
    flush(&mut chunks, &mut buf);
    chunks
}
