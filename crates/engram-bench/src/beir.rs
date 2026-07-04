//! BEIR retrieval benchmark track.
//!
//! BEIR ("Benchmarking-IR", Thakur et al., arXiv:2104.08663) is a heterogeneous
//! suite of IR datasets in a single on-disk layout:
//!
//! ```text
//! <dataset>/
//!   corpus.jsonl        {"_id": "...", "title": "...", "text": "..."}   (one doc per line)
//!   queries.jsonl       {"_id": "...", "text": "..."}                   (one query per line)
//!   qrels/<split>.tsv   query-id <TAB> corpus-id <TAB> score            (header row required)
//! ```
//!
//! Relevance judgements (`qrels`) are graded integers; following the BEIR
//! convention a document counts as *relevant* for recall iff its grade is `> 0`,
//! while nDCG uses the graded value directly.
//!
//! The loader and the per-query scoring/aggregation are dependency-free and
//! deterministically unit-testable (inline fixtures + a fake ranking — no
//! network, no embeddings). The live hybrid-retrieval runner reuses the same
//! store/embedder/fusion plumbing as the other tracks but is never exercised by
//! the unit tests; the CLI wires the real embedder in the integration phase.
//!
//! License note: the default SciFact dataset is distributed under CC BY-NC.

use crate::error::BenchError;
use crate::metrics::ndcg_at_10;
use engram_core::fusion::{reciprocal_rank_fusion, RankedHit};
use engram_core::types::{Memory, MemorySource, RetrievalSource};
use engram_embed::{Embedder, TaskMode};
use engram_storage::SqliteStore;
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A loaded BEIR dataset.
///
/// - `corpus`: doc id -> `(title, text)`.
/// - `queries`: `(query id, query text)`, in file order.
/// - `qrels`: query id -> (corpus id -> relevance grade).
pub struct BeirDataset {
    pub corpus: HashMap<String, (String, String)>,
    pub queries: Vec<(String, String)>,
    pub qrels: HashMap<String, HashMap<String, u32>>,
}

/// Aggregate retrieval scores over all judged queries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BeirReport {
    pub ndcg_at_10: f32,
    pub recall_at_10: f32,
    pub recall_at_100: f32,
    pub n_queries: usize,
}

/// One row of `corpus.jsonl`. `title` is optional in some BEIR datasets.
#[derive(Debug, Deserialize)]
struct CorpusRow {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
}

/// One row of `queries.jsonl`.
#[derive(Debug, Deserialize)]
struct QueryRow {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    text: String,
}

impl BeirDataset {
    /// Load a BEIR dataset from a directory laid out as documented above.
    ///
    /// The qrels split defaults to `test` (the BEIR evaluation split); if
    /// `qrels/test.tsv` is absent, `qrels/dev.tsv` then `qrels/train.tsv` are
    /// tried so the loader works on datasets that ship only a dev split.
    pub fn load_from_dir(dir: &Path) -> Result<Self, BenchError> {
        let corpus = load_corpus(&dir.join("corpus.jsonl"))?;
        let queries = load_queries(&dir.join("queries.jsonl"))?;

        let qrels_dir = dir.join("qrels");
        let qrels_path = ["test", "dev", "train"]
            .iter()
            .map(|split| qrels_dir.join(format!("{split}.tsv")))
            .find(|p| p.exists())
            .ok_or_else(|| {
                BenchError::InvalidDataset(format!(
                    "no qrels split found under {}",
                    qrels_dir.display()
                ))
            })?;
        let qrels = load_qrels(&qrels_path)?;

        if corpus.is_empty() {
            return Err(BenchError::InvalidDataset("empty BEIR corpus".into()));
        }
        if queries.is_empty() {
            return Err(BenchError::InvalidDataset("empty BEIR queries".into()));
        }
        if qrels.is_empty() {
            return Err(BenchError::InvalidDataset("empty BEIR qrels".into()));
        }

        Ok(Self {
            corpus,
            queries,
            qrels,
        })
    }

    /// Score a set of per-query rankings against the loaded qrels.
    ///
    /// `rankings` maps query id -> ranked Vec of retrieved corpus ids (best
    /// first). See [`score_rankings`] for the metric definitions. Only queries
    /// present in `qrels` contribute to the aggregate.
    pub fn score(&self, rankings: &HashMap<String, Vec<String>>) -> BeirReport {
        score_rankings(&self.qrels, rankings)
    }
}

/// Default on-disk location for the BEIR dataset (SciFact, CC BY-NC).
pub fn default_dir() -> PathBuf {
    PathBuf::from("data/beir/scifact")
}

fn load_corpus(path: &Path) -> Result<HashMap<String, (String, String)>, BenchError> {
    let contents = std::fs::read_to_string(path)?;
    let mut corpus = HashMap::new();
    for (idx, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: CorpusRow = serde_json::from_str(line).map_err(|e| {
            BenchError::InvalidDataset(format!(
                "corpus.jsonl line {}: {}",
                idx + 1,
                e
            ))
        })?;
        corpus.insert(row.id, (row.title, row.text));
    }
    Ok(corpus)
}

fn load_queries(path: &Path) -> Result<Vec<(String, String)>, BenchError> {
    let contents = std::fs::read_to_string(path)?;
    let mut queries = Vec::new();
    for (idx, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: QueryRow = serde_json::from_str(line).map_err(|e| {
            BenchError::InvalidDataset(format!(
                "queries.jsonl line {}: {}",
                idx + 1,
                e
            ))
        })?;
        queries.push((row.id, row.text));
    }
    Ok(queries)
}

/// Parse a `qrels/<split>.tsv` file. The first line is the
/// `query-id\tcorpus-id\tscore` header and is skipped. Rows with a
/// non-positive score are kept (grade 0 is a valid "judged not relevant"
/// signal), but rows that fail to parse are an error.
fn load_qrels(path: &Path) -> Result<HashMap<String, HashMap<String, u32>>, BenchError> {
    let contents = std::fs::read_to_string(path)?;
    let mut qrels: HashMap<String, HashMap<String, u32>> = HashMap::new();
    for (idx, line) in contents.lines().enumerate() {
        // Skip the mandatory header row.
        if idx == 0 {
            continue;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let (Some(qid), Some(cid), Some(score)) = (cols.next(), cols.next(), cols.next()) else {
            return Err(BenchError::InvalidDataset(format!(
                "qrels {} line {}: expected 3 tab-separated columns",
                path.display(),
                idx + 1
            )));
        };
        let grade: u32 = score.trim().parse().map_err(|_| {
            BenchError::InvalidDataset(format!(
                "qrels {} line {}: bad score {:?}",
                path.display(),
                idx + 1,
                score
            ))
        })?;
        qrels
            .entry(qid.trim().to_string())
            .or_default()
            .insert(cid.trim().to_string(), grade);
    }
    Ok(qrels)
}

/// Recall@k in the BEIR sense: of the documents judged relevant (grade `> 0`)
/// for a query, the fraction that appear in the top-`k` retrieved ids.
fn recall_at_k(retrieved: &[String], qrels_q: &HashMap<String, u32>, k: usize) -> f32 {
    let relevant: std::collections::HashSet<&String> = qrels_q
        .iter()
        .filter(|(_, &grade)| grade > 0)
        .map(|(id, _)| id)
        .collect();
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = retrieved
        .iter()
        .take(k)
        .filter(|id| relevant.contains(id))
        .count();
    hits as f32 / relevant.len() as f32
}

/// Aggregate per-query rankings into a [`BeirReport`].
///
/// For every query that has a qrels entry, compute nDCG@10 (graded, via
/// [`crate::metrics::ndcg_at_10`]), recall@10 and recall@100, then take the
/// unweighted mean across those queries. A query with no retrieved ids simply
/// scores 0 on every metric (it still counts toward the denominator), matching
/// BEIR's treatment of an empty run. Queries absent from `rankings` are scored
/// against an empty ranking.
pub fn score_rankings(
    qrels: &HashMap<String, HashMap<String, u32>>,
    rankings: &HashMap<String, Vec<String>>,
) -> BeirReport {
    let empty: Vec<String> = Vec::new();
    let mut ndcg_sum = 0f32;
    let mut r10_sum = 0f32;
    let mut r100_sum = 0f32;
    let n = qrels.len();

    for (qid, qrels_q) in qrels {
        let retrieved = rankings.get(qid).unwrap_or(&empty);
        ndcg_sum += ndcg_at_10(retrieved, qrels_q);
        r10_sum += recall_at_k(retrieved, qrels_q, 10);
        r100_sum += recall_at_k(retrieved, qrels_q, 100);
    }

    if n == 0 {
        return BeirReport::default();
    }
    let nf = n as f32;
    BeirReport {
        ndcg_at_10: ndcg_sum / nf,
        recall_at_10: r10_sum / nf,
        recall_at_100: r100_sum / nf,
        n_queries: n,
    }
}

/// Deterministic UUID for a corpus id so the same doc maps to the same chunk
/// across runs (matches the `stable_id` pattern in the other tracks).
fn stable_id(prefix: &str, key: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_DNS, format!("{prefix}:{key}").as_bytes())
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

/// Concatenate a corpus document's title and text into a single blob suitable
/// for both FTS5 and embedding.
fn doc_text(title: &str, text: &str) -> String {
    if title.trim().is_empty() {
        text.to_string()
    } else {
        format!("{title}\n{text}")
    }
}

/// Run hybrid retrieval (FTS5 lexical + dense cosine, fused with RRF) over the
/// whole BEIR corpus and score the result.
///
/// The corpus is indexed once into a single in-memory store, then every query
/// is retrieved against it — this mirrors BEIR's "index once, query many"
/// protocol and the inline retrieval loops used by the other tracks
/// ([`crate::longmemeval`], [`crate::mini`]). Reuse-friendly but **not**
/// exercised by the unit tests, which would otherwise need live embeddings.
///
/// `limit` caps the number of queries (useful for smoke tests); `rrf_k` is the
/// RRF smoothing constant.
pub async fn run_hybrid<E>(
    dataset: &BeirDataset,
    embedder: &E,
    rrf_k: f32,
    limit: Option<usize>,
) -> Result<BeirReport, BenchError>
where
    E: Embedder + ?Sized,
{
    let store = SqliteStore::open_in_memory()?;
    let epoch = chrono::Utc.timestamp_opt(0, 0).single().unwrap();

    // Index the whole corpus once. Keep a parallel map of chunk uuid ->
    // (corpus id, embedding) so we can score dense retrieval in memory and map
    // fused hits back to BEIR ids.
    let mut chunk_to_doc: HashMap<Uuid, String> = HashMap::new();
    let mut chunk_embeddings: HashMap<Uuid, Vec<f32>> = HashMap::new();

    // Stable iteration order so embedding batches and tie-breaks are
    // deterministic across runs.
    let mut doc_ids: Vec<&String> = dataset.corpus.keys().collect();
    doc_ids.sort_unstable();

    let mut texts: Vec<String> = Vec::with_capacity(doc_ids.len());
    let mut chunk_ids: Vec<Uuid> = Vec::with_capacity(doc_ids.len());
    for doc_id in &doc_ids {
        let (title, text) = &dataset.corpus[*doc_id];
        let blob = doc_text(title, text);
        let mem_id = stable_id("beir-mem", doc_id);
        let chunk_id = stable_id("beir-chunk", doc_id);
        let m = Memory {
            id: mem_id,
            content: blob.clone(),
            created_at: epoch,
            event_time: None,
            importance: 5,
            emotional_weight: 0,
            access_count: 0,
            last_accessed: None,
            stability: 1.0,
            source: MemorySource::Conversation {
                thread: (*doc_id).clone(),
                turn: 0,
            },
            diary: "beir".into(),
            valid_from: None,
            valid_until: None,
            tags: vec![],
        };
        store.insert_memory(&m)?;
        store.insert_chunk(chunk_id, mem_id, &blob, 0, None)?;
        chunk_to_doc.insert(chunk_id, (*doc_id).clone());
        texts.push(blob);
        chunk_ids.push(chunk_id);
    }

    // Embed the corpus in one batched call (split internally into sub-batches).
    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
    let doc_vecs = embedder
        .embed_batch(&text_refs, TaskMode::RetrievalDocument)
        .await?;
    for (chunk_id, vec) in chunk_ids.iter().zip(doc_vecs) {
        chunk_embeddings.insert(*chunk_id, vec);
    }

    let n = limit
        .map(|l| l.min(dataset.queries.len()))
        .unwrap_or(dataset.queries.len());

    let mut rankings: HashMap<String, Vec<String>> = HashMap::new();
    for (qid, qtext) in dataset.queries.iter().take(n) {
        // Lexical run.
        let fts_q = crate::mini::build_fts_query(qtext);
        let fts_hits = if fts_q.is_empty() {
            Vec::new()
        } else {
            store.fts_search(&fts_q, 100).unwrap_or_default()
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

        // Dense run.
        let q_emb = embedder.embed_one(qtext, TaskMode::RetrievalQuery).await?;
        let mut dense_scored: Vec<(Uuid, f32)> = chunk_embeddings
            .iter()
            .map(|(cid, emb)| (*cid, cosine_sim(&q_emb, emb)))
            .collect();
        dense_scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let dense_run: Vec<RankedHit> = dense_scored
            .iter()
            .take(100)
            .enumerate()
            .map(|(i, (id, score))| RankedHit {
                chunk_id: *id,
                rank: i + 1,
                raw_score: *score,
                source: RetrievalSource::Dense,
            })
            .collect();

        let fused = reciprocal_rank_fusion(&[lexical_run, dense_run], rrf_k);
        let ranked: Vec<String> = fused
            .iter()
            .filter_map(|(id, _)| chunk_to_doc.get(id).cloned())
            .collect();
        rankings.insert(qid.clone(), ranked);
    }

    Ok(dataset.score(&rankings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(dir: &Path) {
        std::fs::create_dir_all(dir.join("qrels")).unwrap();
        std::fs::write(
            dir.join("corpus.jsonl"),
            // Two real docs + a blank line to prove blank lines are skipped.
            "{\"_id\":\"d1\",\"title\":\"Rapamycin\",\"text\":\"mTOR inhibitor extends lifespan\"}\n\
             \n\
             {\"_id\":\"d2\",\"title\":\"\",\"text\":\"unrelated cooking recipe\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("queries.jsonl"),
            "{\"_id\":\"q1\",\"text\":\"does rapamycin extend lifespan\"}\n\
             {\"_id\":\"q2\",\"text\":\"how to bake bread\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("qrels").join("test.tsv"),
            "query-id\tcorpus-id\tscore\n\
             q1\td1\t1\n\
             q2\td2\t2\n",
        )
        .unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("beir_test_{}_{}", tag, std::process::id()))
    }

    #[test]
    fn loads_inline_fixture() {
        let dir = temp_dir("load");
        let _ = std::fs::remove_dir_all(&dir);
        write_fixture(&dir);

        let ds = BeirDataset::load_from_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(ds.corpus.len(), 2);
        assert_eq!(ds.corpus["d1"].0, "Rapamycin");
        assert_eq!(ds.corpus["d1"].1, "mTOR inhibitor extends lifespan");
        // Untitled doc keeps an empty title.
        assert_eq!(ds.corpus["d2"].0, "");

        assert_eq!(ds.queries.len(), 2);
        assert_eq!(ds.queries[0], ("q1".into(), "does rapamycin extend lifespan".into()));

        assert_eq!(ds.qrels.len(), 2);
        assert_eq!(ds.qrels["q1"]["d1"], 1);
        assert_eq!(ds.qrels["q2"]["d2"], 2);
    }

    #[test]
    fn missing_dir_is_error() {
        let r = BeirDataset::load_from_dir(Path::new("/tmp/beir-does-not-exist-xyz"));
        assert!(r.is_err());
    }

    #[test]
    fn qrels_requires_three_columns() {
        let dir = temp_dir("badqrels");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("qrels")).unwrap();
        std::fs::write(dir.join("corpus.jsonl"), "{\"_id\":\"d1\",\"text\":\"x\"}\n").unwrap();
        std::fs::write(dir.join("queries.jsonl"), "{\"_id\":\"q1\",\"text\":\"y\"}\n").unwrap();
        std::fs::write(
            dir.join("qrels").join("test.tsv"),
            "query-id\tcorpus-id\tscore\nq1\td1\n",
        )
        .unwrap();
        let r = BeirDataset::load_from_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(r, Err(BenchError::InvalidDataset(_))));
    }

    fn qrels(pairs: &[(&str, &[(&str, u32)])]) -> HashMap<String, HashMap<String, u32>> {
        pairs
            .iter()
            .map(|(q, docs)| {
                (
                    q.to_string(),
                    docs.iter().map(|(d, g)| (d.to_string(), *g)).collect(),
                )
            })
            .collect()
    }

    fn ranking(pairs: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(q, docs)| (q.to_string(), docs.iter().map(|d| d.to_string()).collect()))
            .collect()
    }

    #[test]
    fn recall_at_k_is_fraction_of_relevant_found() {
        // 2 relevant docs (grade>0), one grade-0 (judged non-relevant).
        let q: HashMap<String, u32> =
            [("a", 1u32), ("b", 2), ("c", 0)].iter().map(|(d, g)| (d.to_string(), *g)).collect();
        // Top-2 contains a (relevant) and c (non-relevant) -> 1/2 of relevant.
        let retrieved: Vec<String> = ["a", "c", "z", "b"].iter().map(|s| s.to_string()).collect();
        assert!((recall_at_k(&retrieved, &q, 2) - 0.5).abs() < 1e-6);
        // Top-4 captures both a and b -> full recall.
        assert!((recall_at_k(&retrieved, &q, 4) - 1.0).abs() < 1e-6);
        // No relevant docs at all -> 0.
        let none: HashMap<String, u32> = [("a", 0u32)].iter().map(|(d, g)| (d.to_string(), *g)).collect();
        assert_eq!(recall_at_k(&retrieved, &none, 4), 0.0);
    }

    #[test]
    fn perfect_ranking_scores_one() {
        let q = qrels(&[
            ("q1", &[("d1", 2), ("d2", 1)]),
            ("q2", &[("d3", 1)]),
        ]);
        let r = ranking(&[
            ("q1", &["d1", "d2"]),
            ("q2", &["d3"]),
        ]);
        let report = score_rankings(&q, &r);
        assert_eq!(report.n_queries, 2);
        assert!((report.ndcg_at_10 - 1.0).abs() < 1e-6, "ndcg {}", report.ndcg_at_10);
        assert!((report.recall_at_10 - 1.0).abs() < 1e-6);
        assert!((report.recall_at_100 - 1.0).abs() < 1e-6);
    }

    #[test]
    fn missing_and_empty_rankings_score_zero_but_count() {
        let q = qrels(&[
            ("q1", &[("d1", 1)]),
            ("q2", &[("d2", 1)]),
        ]);
        // q1 retrieves its relevant doc; q2 is absent from the run entirely.
        let r = ranking(&[("q1", &["d1"])]);
        let report = score_rankings(&q, &r);
        assert_eq!(report.n_queries, 2);
        // q1 = 1.0, q2 = 0.0 -> mean 0.5 on every metric.
        assert!((report.recall_at_10 - 0.5).abs() < 1e-6);
        assert!((report.recall_at_100 - 0.5).abs() < 1e-6);
        assert!((report.ndcg_at_10 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn empty_qrels_yields_default_report() {
        let report = score_rankings(&HashMap::new(), &HashMap::new());
        assert_eq!(report, BeirReport::default());
    }

    #[test]
    fn dataset_score_matches_score_rankings() {
        let dir = temp_dir("score");
        let _ = std::fs::remove_dir_all(&dir);
        write_fixture(&dir);
        let ds = BeirDataset::load_from_dir(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        // Fake ranking: q1 nails d1 at rank 1, q2 puts d2 second.
        let r = ranking(&[("q1", &["d1"]), ("q2", &["zzz", "d2"])]);
        let report = ds.score(&r);
        assert_eq!(report.n_queries, 2);
        // q1: recall@10 = 1.0. q2: d2 is within top-100 -> recall 1.0. Mean 1.0.
        assert!((report.recall_at_10 - 1.0).abs() < 1e-6);
        // nDCG: q1 perfect (1.0); q2 has its single grade-2 doc at rank 2 ->
        // DCG = 3/log2(3) = 1.8927, IDCG = 3/log2(2) = 3 -> 0.63092975.
        // Mean = (1.0 + 0.63092975)/2 = 0.81546487.
        assert!((report.ndcg_at_10 - 0.815_464_9).abs() < 1e-5, "ndcg {}", report.ndcg_at_10);
    }
}
