# engram — research direction

## Goal (2026-06-16)

Maximise hallucination-resistance over a **specialized corpus** — ingest a domain's papers and query *those*, not the whole literature — measured across retrieval quality, answer accuracy, abstention/precision, and citation faithfulness rather than any single number (see the framing rule in `README.md`). Default scored models: answerer `anthropic/claude-opus-4.8`, grader `google/gemini-3.1-pro-preview`.

## Where we stand

| Axis | Result |
|---|---|
| Scientific-QA, full-text RAG (LitQA2, 199q over a specialized corpus) | 87.9% accuracy · 99.4% precision-with-abstention · 97% retrieval-success |
| Scientific-QA, closed-book control (no retrieval) | 43.7% accuracy · 49.7% precision |
| Retrieval ranking (BEIR-SciFact, 300 queries) | nDCG@10 0.817 (hybrid+rerank) vs 0.689 (BM25) |
| Conversational-memory retrieval (LongMemEval S, 500q) | R@5 0.99 |

Retrieval and abstention are strong: on all 6 LitQA2 retrieval misses the system abstained ("Insufficient information") rather than answering wrong.

## The weak link: the answerer

`accuracy | retrieval-hit = 90.7%` — 15/193 questions had the right passage retrieved and were still answered wrong. Retrieval (97% hit rate) and hallucination-resistance (99.4% precision) are not the bottleneck; answer interpretation is. Priority order:

1. **Answerer interpretation.** Diagnose the 15 answer-errors-on-good-retrieval; categorise (reasoning vs passage-misread vs distractor-confusion); test chain-of-thought, quote-the-supporting-span-first, and self-verify prompts in `qa.rs`; run head-to-head answerer models (`openai/gpt-5.5`, `google/gemini-3-pro`). Target `accuracy|hit` → 95%+.
2. **Honest retrieval test.** The 97% retrieval-success is over a curated key-passage corpus (answers present). Build a full open-access-paper corpus (Europe PMC full-text by DOI) and re-run to measure the retriever's real ceiling under realistic haystack.
3. **Complete the picture.** BEIR `nfcorpus`/`trec-covid`/`scidocs`; RAGAS faithfulness; a runnable `bench citation` suite (the ALCE scorer exists in `citation.rs` but has no CLI command yet).

## Eval mechanics

- `engram bench mini` is the offline, CI-stable smoke (`autoresearch.toml` `eval_command`): deterministic, no API keys, prints `recall_at_5` as a bare float. It saturates near 1.0 — it keeps the loop green and catches regressions; it is not a quality signal, so do not optimise it.
- The scored axes run with the default opus-4.8 answerer + gemini-3.1-pro grader (commands in `README.md` → Benchmarks). Validate any win at full suite size before declaring a milestone.

## Reward-hacking watch

- **Precision-with-abstention is the LitQA2 target, not raw accuracy.** Answering more questions while lowering precision (guessing when unsure) games the wrong metric; abstaining correctly is the goal.
- On citation faithfulness, do not trade recall for precision by emitting fewer, safer citations — report both.
- Latency is secondary; wins must not cost accuracy or precision on the scored axes.

## Prior milestones (for context)

- LongMemEval S 500-q: R@5 0.99 with plain hybrid (Gemini + FTS + RRF), no rerank — retrieval is not the bottleneck.
- Mini bench saturated at R@1 1.0 once Gemini dense retrieval was added.
