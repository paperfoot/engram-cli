#!/usr/bin/env bash
# bench_eta.sh — quick ETA snapshot for the LoCoMo checkpoint JSONL.
#
# Usage:
#   scripts/bench_eta.sh [checkpoint_path] [total_questions] [bench_start_epoch]
#
# Defaults:
#   checkpoint_path   = benchmarks/locomo-full-mini-zerank2-mlx.checkpoint.jsonl
#   total_questions   = 1542
#   bench_start_epoch = mtime of the first checkpoint line or `stat` on the file
#
# Prints: completed / total, elapsed, rate, ETA to finish, per-category so far.

set -euo pipefail

CKPT="${1:-benchmarks/locomo-full-mini-zerank2-mlx.checkpoint.jsonl}"
TOTAL="${2:-1542}"
START_EPOCH="${3:-}"

if [[ ! -f "$CKPT" ]]; then
    echo "checkpoint not found: $CKPT"
    exit 1
fi

DONE=$(wc -l < "$CKPT" | tr -d ' ')

# Use explicit start if provided, otherwise creation time of the file.
if [[ -z "$START_EPOCH" ]]; then
    START_EPOCH=$(stat -f %B "$CKPT")  # macOS birth time
fi

NOW=$(date +%s)
ELAPSED=$(( NOW - START_EPOCH ))

# Rate (questions per second). Use float via awk.
if [[ "$ELAPSED" -gt 0 && "$DONE" -gt 0 ]]; then
    RATE=$(awk -v d="$DONE" -v e="$ELAPSED" 'BEGIN { printf "%.3f", d / e }')
    REMAINING=$(( TOTAL - DONE ))
    ETA_SEC=$(awk -v r="$REMAINING" -v rt="$RATE" 'BEGIN { if (rt > 0) printf "%d", r / rt; else print "inf" }')
else
    RATE="0.000"
    ETA_SEC=0
fi

# Human-readable elapsed and ETA.
human() {
    local s=$1
    if [[ "$s" == "inf" ]]; then echo "∞"; return; fi
    printf "%02d:%02d:%02d" $(( s / 3600 )) $(( (s % 3600) / 60 )) $(( s % 60 ))
}

ELAPSED_HMS=$(human "$ELAPSED")
ETA_HMS=$(human "$ETA_SEC")
FINISH_EPOCH=$(( NOW + ETA_SEC ))
FINISH_HMS=$(date -r "$FINISH_EPOCH" +%H:%M:%S 2>/dev/null || echo "?")

PCT=$(awk -v d="$DONE" -v t="$TOTAL" 'BEGIN { printf "%.1f", (d / t) * 100 }')

echo "=== LoCoMo-QA bench status ==="
echo "checkpoint:  $CKPT"
echo "progress:    $DONE / $TOTAL   ($PCT%)"
echo "elapsed:     $ELAPSED_HMS"
echo "rate:        $RATE q/s"
echo "eta:         $ETA_HMS   (finishes ~$FINISH_HMS local time)"
echo

# By-category quick tally using jq if present.
if command -v jq >/dev/null 2>&1 && [[ "$DONE" -gt 0 ]]; then
    echo "=== by category (completed so far) ==="
    jq -s 'group_by(.question_type // .category // "unknown")
           | map({category: .[0].question_type // .[0].category // "unknown",
                  n: length,
                  correct: ([.[] | select(.correct == true)] | length)})
           | .[]
           | "\(.category): \(.correct)/\(.n)"' \
       "$CKPT" -r 2>/dev/null || echo "(checkpoint schema different from expected — keep running)"
fi
