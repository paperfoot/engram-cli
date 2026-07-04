# engram benchmark results

Raw JSON reports from `engram bench` runs, timestamped `<suite>-<YYYYMMDDTHHMMSS>-<n>.json`. Each file holds the per-question result, model IDs, token counts, and summary stats, so every run is reproducible and auditable. LitQA2 and BEIR reports are gitignored (they embed canary-protected question content and the CC-BY-NC SciFact corpus).

Models: answerer `anthropic/claude-opus-4.8`, grader `google/gemini-3.1-pro-preview`, embeddings `gemini-embedding-2`, reranker `cohere rerank-v4.0-pro`. The [top-level README Benchmarks section](../README.md#benchmarks) explains what each axis measures and why.

## Headline results

**Scientific-QA over a specialized corpus — LitQA2 (199 questions).** Ingest the answer-bearing passages as a bounded domain corpus and answer over it, versus a closed-book control:

| Pipeline | Accuracy | Precision (abstention-adjusted) | Coverage | Retrieval-success |
|---|---:|---:|---:|---:|
| Closed-book (no retrieval) | 43.7% | 49.7% | 87.9% | — |
| Full-text RAG (specialized corpus) | 87.9% | 99.4% | 88.4% | 97.0% |

Retrieval adds +44 points of accuracy and pushes precision to 99.4%. On all 6 retrieval misses the system abstained ("Insufficient information") rather than answering wrong; the residual gap is the answerer (15/193 errors on correctly-retrieved evidence), not retrieval. For reference, PaperQA2 reports 66.0% accuracy / 85.2% precision over the full literature, and human experts score 67.7%.

**Retrieval ranking — BEIR-SciFact (300 test queries over 5,183 docs):**

| Pipeline | nDCG@10 | Recall@10 | Recall@100 |
|---|---:|---:|---:|
| BM25 (lexical only) | 0.689 | 0.814 | 0.923 |
| hybrid + Cohere rerank | 0.817 | 0.935 | 0.990 |

The hybrid stack reaches nDCG@10 0.817, above the strong-dense reference (~0.74) and the BEIR BM25 baseline (~0.665).

**Conversational-memory retrieval — LongMemEval S (500 questions, 96% distractors):** R@1 0.910, R@5 0.990, R@10 0.998, MRR 0.946 (Gemini Embed 2 + FTS5 + RRF). A retrieval sanity check, separate from the science evaluation.

## Reproducing

```bash
# Scientific-QA, full-text RAG over an ingested domain corpus:
engram bench litqa2 --mode fulltext --kb <your-corpus> --limit 199 --json
# Scientific-QA, closed-book control (answerer only, no retrieval):
engram bench litqa2 --mode closed-book --limit 199 --json
# Retrieval ranking:
engram bench beir --dataset scifact --rerank on --json
# Reference-free faithfulness (claim-decomposition RAGAS, add-on on any QA suite):
engram bench litqa2 --mode fulltext --kb <your-corpus> --limit 50 --ragas --json
# Conversational-memory retrieval sanity check:
engram bench longmemeval --json
# Save the report to a chosen path (it always also prints to stdout):
engram bench litqa2 --mode closed-book --limit 199 --save benchmarks/my-run.json --json
```

Set `GEMINI_API_KEY` (embeddings), `OPENROUTER_API_KEY` (answerer/grader), and optionally `COHERE_API_KEY` (rerank) in the environment, or run `engram config set keys.<provider> <value>`. Datasets download on first run; the CLI prints the exact command when one is missing.

## Report shape

```jsonc
{
  "suite": "litqa2",
  "mode": "fulltext",
  "questions_evaluated": 199,
  "accuracy": 0.879,
  "precision": 0.994,          // abstentions excluded from the denominator
  "coverage": 0.884,
  "abstain_rate": 0.116,
  "retrieval_success": 0.97,   // target passage surfaced in top-k
  "answerer_model": "anthropic/claude-opus-4.8",
  "per_question": [
    { "question_id": "...", "correct": true, "candidate_answer": "...",
      "gold_answer": "...", "recall_at_5": 1.0, "retrieved_sessions": ["10.xxxx/..."],
      "latency_ms": 1430, "answerer_prompt_tokens": 240, "answerer_completion_tokens": 7 }
  ]
}
```

Every question keeps its candidate answer, gold answer, retrieved sources, and token counts — enough to audit failures, compute per-question cost, or re-grade without re-running retrieval.
