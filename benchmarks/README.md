# engram v2 benchmark results

Raw JSON reports from `engram bench` runs, timestamped. Each file contains the
full per-question result, model IDs, token counts, and summary stats so you can
reproduce or post-hoc analyze any run.

## File naming

```
<suite>-<YYYYMMDDTHHMMSS>-<question_count>.json
```

- `longmemeval-qa-*` — LongMemEval S split, end-to-end QA (retrieve → answer → judge)
- `longmemeval-*` (retrieval only) — early retrieval-only numbers
- `locomo-qa-*` — LoCoMo dataset, end-to-end QA
- `mini-*` — 10-question smoke bench (FTS5 only or hybrid)

## Current headline numbers

### Retrieval only (LongMemEval S full 500)
- **R@1 = 0.910**
- **R@5 = 0.990** ← beats MemPalace published 0.984
- **R@10 = 0.998**
- MRR = 0.946
- Pipeline: Gemini Embed 2 + FTS5 + RRF (k=60), no reranker
- File: main branch commit `571fc18`

### End-to-end QA (with GPT-5.4 answerer + judge)

| Suite | Sample | Correct | Accuracy | Notes |
|---|---|---|---|---|
| LongMemEval-QA | 2 | 2 | **100%** | Trivial single-session questions |
| LongMemEval-QA | 3 | 1 | **33%** | 1 interpretation error, 1 "I don't know" despite R@5=1.0 |
| LoCoMo-QA | 5 | 2 | **40%** | Short session-by-session test |
| LoCoMo-QA | 50 | 14 | **28%** | First stable QA number on LoCoMo |
| LongMemEval-QA | 10 | _pending_ | | Running with top_k=5 |

### The 17% gap made visible

These QA numbers validate [@parcadei's LongMemEval critique](https://x.com/parcadei/status/2041479166764196206)
of MemPalace: **retrieval being good isn't enough**. Our retrieval is perfect
(MRR = 1.0 on the LongMemEval-QA sample), but answer generation accuracy is
28–40% — not the 99% the retrieval number suggests.

This is a known failure mode of the whole memory-system space, not a bug in
engram's retrieval. The gap is in how the LLM interprets "daily commute" as
round-trip (90 min) vs one-way (45 min), or refuses to answer when it could
have extracted "Target" from the context.

## Reproducing

```bash
engram bench longmemeval-qa --limit 50 --top-k 5 \
  --answerer openai/gpt-5.4 --judge openai/gpt-5.4 --json

engram bench locomo-qa --limit 50 --json

# With RAGAS metrics (4 extra LLM calls/question — expensive):
engram bench longmemeval-qa --limit 20 --ragas --json

# Save to a specific path (otherwise goes to benchmarks/<auto-name>.json):
engram bench longmemeval-qa --limit 100 --save benchmarks/my-run.json --json
```

Requirements:
- `GEMINI_API_KEY` in env or `~/.config/engram/config.toml` (embeddings)
- `OPENROUTER_API_KEY` (answerer + judge LLMs)
- `COHERE_API_KEY` (optional, adds rerank)

## What the JSON reports contain

```jsonc
{
  "suite": "longmemeval_qa",
  "questions_evaluated": 10,
  "correct_count": 3,
  "accuracy": 0.3,
  "recall_at_5": 1.0,          // retrieval
  "mrr": 1.0,                   // retrieval
  "ragas": null,                // { faithfulness, answer_relevance, ... } when --ragas
  "mean_latency_ms": 159079.0,
  "p50_latency_ms": 175745.0,
  "answerer_total_prompt_tokens": 109311,
  "answerer_total_completion_tokens": 63,
  "judge_total_prompt_tokens": 1332,
  "judge_total_completion_tokens": 60,
  "per_question": [
    {
      "question_id": "...",
      "question_type": "single-session-user",
      "question": "...",
      "gold_answer": "...",
      "candidate_answer": "...",
      "correct": false,
      "recall_at_5": 1.0,
      "mrr": 1.0,
      "retrieved_sessions": [...],
      "answer_session_ids": [...],
      "ragas": null,
      "latency_ms": 175745,
      "answerer_prompt_tokens": 36437,
      "answerer_completion_tokens": 21,
      "judge_prompt_tokens": 444,
      "judge_completion_tokens": 20
    }
  ],
  "by_question_type": {
    "single-session-user": { "total": 10, "correct": 3, "accuracy": 0.3 }
  }
}
```

Every question has its full candidate answer, gold answer, retrieved session
ids, and token counts preserved — so you can audit failures, compute cost
per question, or rerun the judge with a different prompt without touching the
retrieval step.
