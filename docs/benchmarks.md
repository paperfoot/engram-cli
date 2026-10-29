# engram benchmarks

engram is built to make agents *hallucination-resistant* over a **specialized corpus** — you ingest the papers/protocols for your domain and query *those*, not the entire literature. The benchmarks below test that realistic setup: precision retrieval, grounded answering, and **abstention instead of guessing**, over a bounded domain corpus.

> No single benchmark proves hallucination-resistance — read the axes together: **retrieval quality**, **answer accuracy**, **abstention/precision** (refuse rather than guess), and **citation faithfulness**. A system strong on one axis but weak on another is not hallucination-resistant; treat any single number in isolation as noise.

Numbers below were produced with answerer `anthropic/claude-opus-4.8`, grader `google/gemini-3.1-pro-preview`, embeddings `gemini-embedding-2`, reranker `cohere rerank-v4.0-pro` (June 2026). Full per-question detail, tokens, and model IDs are logged to `benchmarks/` for audit.

## Axis 1 — Scientific-QA over a specialized corpus (headline): LitQA2

**Setup.** The 199-question **LitQA2** biomedical literature set (`futurehouse/lab-bench`, ungated — the benchmark PaperQA2 reports on, peer-anchored to **AstaBench** [arXiv:2510.21652](https://arxiv.org/abs/2510.21652); PaperQA2 itself [arXiv:2409.13740](https://arxiv.org/abs/2409.13740)). We ingest the answer-bearing passages as a **bounded specialized corpus** (181 docs) and run retrieve-then-answer over it — engram's actual use pattern — against a **closed-book** control (no retrieval, the model's own knowledge).

| Pipeline | Accuracy | Precision (abstention-adjusted) | Precision (strict) | Coverage | Retrieval-success |
|---|---:|---:|---:|---:|---:|
| Closed-book (no RAG) | 43.7% | 49.7% | — | 87.9% | — |
| **Full-text RAG (specialized corpus)** | **87.9%** | **99.4%** | **92.1%** | 88.4% | **97.0%** |
| *ref — PaperQA2 (full literature)* | *66.0%* | *85.2%* | — | — | — |
| *ref — human experts* | *67.7%* | — | — | — | — |

Retrieval adds **+44 points of accuracy**. Two precision numbers:

- **Precision, abstention-adjusted = 99.4%** — of the answers where the model *committed* to an option, it was almost never wrong. This excludes both principled abstentions and empty/unparseable API responses from the denominator (the PaperQA2/LitQA2 convention).
- **Precision, strict = 92.1%** — the same, but counting empty/unparseable answerer responses as failed commitments rather than safe declines. In the logged 199-question run, 14 of 23 "abstentions" were zero-token API responses, not principled declines; the strict number is what you get when those count against you. The bench records `no_response_rate` and both precision figures.

**Where it wins / where it doesn't** (199-question decomposition):

- **Retrieval is not the bottleneck** — the right passage is surfaced for **193/199 (97%)** questions.
- **Genuine hallucination resistance** — on all **6/6** true retrieval misses it selected "Insufficient information" (real, token-bearing responses), with **0 wrong answers**.
- **The remaining gap is the answerer** — of the non-correct questions, most are answer errors on correctly-retrieved evidence (accuracy given a retrieval hit ≈ 90.7%). The lever is answer interpretation, not retrieval.

> *Comparability:* the 87.9% is over a curated specialized corpus (answer-bearing passages present) — engram's design point — and is **not** apples-to-apples with PaperQA2's 66% (which searches a far larger, noisier corpus). Retrieval-success 97% is likewise an easy find-1-of-181 task, not open-literature retrieval. The robust reads are: the +44-pt accuracy lift over closed-book (same model, same prompt — a fair comparison), and strict precision 92.1%.

```bash
engram bench litqa2 --mode fulltext --corpus data/litqa2/corpus.json --limit 199 --json  # RAG over your corpus
engram bench litqa2 --mode closed-book --limit 199 --json                                 # no-retrieval control
```

## Axis 2 — Retrieval quality: BEIR-scientific nDCG@10

**What it measures.** Pure retrieval ranking on scientific IR, no LLM involved: does the hybrid pipeline (Gemini Embed 2 + FTS5 + RRF + Cohere rerank) put the relevant documents at the top? Primary metric **nDCG@10**. Measured on **SciFact** (300 test queries over a 5,183-doc corpus); anchor BEIR — [arXiv:2104.08663](https://arxiv.org/abs/2104.08663).

| Pipeline | nDCG@10 | Recall@10 | Recall@100 |
|---|---:|---:|---:|
| BM25 (lexical only) | 0.689 | 0.814 | 0.923 |
| **hybrid + Cohere rerank** | **0.817** | **0.935** | **0.990** |
| *ref — BM25 (BEIR paper)* | *~0.665* | — | — |
| *ref — strong dense retriever* | *~0.74* | — | — |

engram's full hybrid stack reaches **nDCG@10 = 0.817 on SciFact — above the strong-dense reference (~0.74)** — and Recall@100 = 0.99 (the relevant document is almost always retrievable). nDCG uses **linear gain**, matching `pytrec_eval`'s `ndcg_cut` (the tool the published BEIR leaderboard is computed with), so graded datasets are comparable to the paper; SciFact qrels are binary, so the 0.817 is identical under either gain convention. NFCorpus / TREC-COVID / SCIDOCS wire identically via `--dataset`.

```bash
engram bench beir --dataset scifact --rerank on --json
```

## Axis 3 — Citation faithfulness (implemented; no published score yet): ALCE citation P/R + repaired RAGAS

**What it measures.** Whether each answer sentence is *actually supported by the source it cites* — over **your own retrieved corpus**, not a fixed external answer set.

- **ALCE citation precision / recall** ([arXiv:2305.14627](https://arxiv.org/abs/2305.14627)) — recall: is a statement's cited evidence sufficient to entail it; precision: how many carried citations are load-bearing. The denominator is aligned to the reference `eval.py` (an early divergence was caught and fixed in review). Implemented in [`citation.rs`](../crates/engram-bench/src/citation.rs); scores once a domain corpus + held-out QA set is configured.
- **Repaired RAGAS faithfulness** ([arXiv:2309.15217](https://arxiv.org/abs/2309.15217)) — claim-decomposition + per-claim entailment to the retrieved context. Runs **live today** as an add-on on any QA suite:

```bash
engram bench litqa2 --mode fulltext --corpus data/litqa2/corpus.json --limit 50 --ragas --json
```

> *Scope — TREC-BioGen citation-support:* the `biogen` suite scores citation support over a loaded corpus, not the full PubMed index the original task assumes, so its numbers are not comparable to published TREC results. ALCE-over-your-own-corpus is the attribution check that matches engram's model; the abstention behaviour in Axis 1 (0 wrong answers on 6 retrieval misses) is the live hallucination-resistance evidence.

## Supporting retrieval result — LongMemEval S (500 questions, 96% distractors)

Retrieval-only sanity check on the full 500-question **[LongMemEval S split](https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned)** — 48 sessions per question, 96% distractors. Not part of the hallucination-resistance triangulation (it is conversational memory, not science), but it shows the hybrid pipeline is near-ceiling on retrieval:

| Pipeline | R@1 | R@5 | R@10 | MRR |
|---|---|---|---|---|
| **engram — hybrid only** (Gemini Embed 2 + FTS5 + RRF) | 0.910 | **0.990** | 0.998 | 0.946 |
| **engram — hybrid + Cohere Rerank** (first 100 Qs) | 0.930 | 0.980 | 1.000 | 0.957 |

```bash
engram bench longmemeval --json                          # full 500, retrieval only
engram bench longmemeval --limit 50 --json               # first 50
engram bench mini --json                                 # 10-question offline smoke
engram bench scientific-mini --json                      # cited-evidence compiler/retrieval smoke (offline, deterministic)
```

All runs log full per-question detail, token counts, model IDs, retrieval metrics, and judge verdicts to [`benchmarks/`](../benchmarks/) for audit. Datasets must be downloaded first (the CLI prints the exact `curl` command on first run).

## Additional benchmarks

| Suite | Status | Source | Notes |
|---|---|---|---|
| `locomo-plus` | wired | [xjtuleeyf/Locomo-Plus](https://github.com/xjtuleeyf/Locomo-Plus) | 401 cognitive cue/trigger pairs. SOTA: gemini-2.5-pro 26%. Binary judge. |
| `mab` (`accurate_retrieval`) | wired | [HUST-AI-HYZ/MemoryAgentBench](https://github.com/HUST-AI-HYZ/MemoryAgentBench) | 22 rows, ~1500 retrieval questions. |
| `mab` (`test_time_learning`) | wired | same | 6 rows, MCC + movie rec |
| `mab` (`long_range_understanding`) | wired | same | DetectiveQA LLM-judge + ∞Bench-Sum keypoint-F1 |
| `mab` (`conflict_resolution`) | wired | same | Selective forgetting |
| `scientific-mini` | wired | local fixture | Tiny cited evidence compiler + retrieval smoke. Offline/stub deterministic. |

Run any of them with `engram bench <suite> --limit N`.
