# What recall prints, measured — 30 Oct 2026

Two changes to what `engram recall` prints, each one commit, measured with
`benchmarks/longmemeval_cli.py`, which drives the real binary as an agent does.

## Method
- LongMemEval: all 78 knowledge-update and 133 temporal-reasoning questions, plus 25 of each
  other type (seed 7), 311 in all. Each question's evidence sessions come from the S split,
  together with 4 distractor sessions from the same record that fall no later than the
  question's day.
- One store per question, built once with the baseline binary. Each session is written with
  `engram remember --event-time <session date> --autonomy full`, with fact extraction on.
- Each arm copies the frozen store and runs `<binary> recall "<question>" --json` with the
  relevance judge off and no reranker. All 311 questions retrieved identical result ids, in the
  same order, in every arm, so the arms differ only in what recall printed.
- Answerer: openai/gpt-6.1-terra, low effort, temperature 0. It gets the question date, the
  question and the recall stdout verbatim.
- Judge: anthropic/claude-haiku-5.5, using LongMemEval's own prompt templates, read from its
  `evaluate_qa.py`. No call failed.
- Noise floor: answering the baseline twice from identical recall output flipped 9 of 311
  verdicts.

## Results (accuracy %, 95% Wilson interval)
| type | n | A baseline | B one copy | C + dates (shipped) |
|---|---|---|---|---|
| knowledge-update | 78 | 70.5 (60–79) | 71.8 | **87.2 (78–93)** |
| temporal-reasoning | 133 | 43.6 (35–52) | 44.4 | **82.0 (75–88)** |
| multi-session | 25 | 80.0 | 80.0 | 76.0 |
| single-session-assistant | 25 | 100 | 100 | 100 |
| single-session-preference | 25 | 72.0 | 72.0 | 68.0 |
| single-session-user | 25 | 100 | 100 | 96.0 |
| **all** | 311 | **64.6** | 65.3 | **84.2 (80–88)** |
| answer prompt tokens (mean) | | 7,211 | 5,479 | **5,719** |

Paired gains and losses, exact McNemar:
- **B vs A:** +9 / −7 (p = 0.80). Accuracy unchanged; 24% fewer prompt tokens.
- **C vs B:** temporal-reasoning +50 / −0 (p < 0.001), knowledge-update +13 / −1 (p = 0.002).
- **C vs A:** +65 / −6 overall (p < 0.001). The shipped code is byte-identical to arm C.

On a copy of the owner's store, 40 real recall queries returned 10.8% fewer bytes, with
identical retrieval on all 40.

## Tried and not shipped
- **Present tense:** an `<engram-update>` line under a passage whose fact has since changed.
  - On LongMemEval it measured within the noise floor.
  - Six rounds of review kept finding cases where the line could state something false or
    reveal hidden text: chains, future intervals, edits, refused safety proposals.
  - Parked on the `wave/present-tense-parked` branch with every finding.
- **Background fact extraction:** it cut `remember` p50 from 3.55 s to 0.40 s with the same
  facts.
  - Reverted as unsafe: the extractor races edits and forgets, and the KB-wide sweep ignored
    opt-outs.
  - It needs a per-memory job with a content-hash guard.

## Limits
- Small stores: 5 to 10 sessions per question.
- A single answerer and a single judge.
- No reranker and no relevance judge, so presentation is isolated from retrieval.
