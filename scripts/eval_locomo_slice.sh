#!/usr/bin/env bash
# Evaluate a deterministic LoCoMo-QA slice and print the accuracy as a bare float.
# Used by autoresearch as the eval_command. Sources scripts/experiment.env so
# experiments can vary env-var knobs atomically.
#
# Exits 0 + prints accuracy on success; exits non-zero on bench error.
set -euo pipefail

cd "$(dirname "$0")/.."

# Load knobs
set -a
# shellcheck source=experiment.vars
source scripts/experiment.vars
set +a

# If GEMINI_API_KEY env var is expired, unset it so the binary falls
# back to the config-file key via resolve_secret("GEMINI_API_KEY", "keys.gemini").
unset GEMINI_API_KEY 2>/dev/null || true

: "${ENGRAM_LOCOMO_LIMIT:=50}"
: "${ENGRAM_LOCOMO_TOPK:=5}"
: "${ENGRAM_ANSWERER:=openai/gpt-5.4}"
: "${ENGRAM_JUDGE:=openai/gpt-5.4}"

# Slug encoding for the output file (so experiments don't clobber each other).
SLUG="$(echo -n "$(cat scripts/experiment.vars)" | shasum -a 1 | cut -c1-8)"
OUT="benchmarks/ar/locomo-slice-${SLUG}-$(date +%H%M%S).json"
mkdir -p benchmarks/ar

# Rebuild only if Rust sources changed since the installed binary.
# Skip otherwise — recompiling the CLI is 30-90s of dead time.
if ! test -x ~/.cargo/bin/engram \
   || find crates -name '*.rs' -newer ~/.cargo/bin/engram 2>/dev/null | grep -q .; then
    cargo install --path crates/engram-cli --force --quiet 2>&1 >/dev/null || {
        echo "[eval] cargo install FAILED" >&2
        exit 2
    }
fi

# Run the bench. Silence progress chatter; keep JSON on stdout via --json.
engram --json bench locomo-qa \
    --limit "${ENGRAM_LOCOMO_LIMIT}" \
    --top-k "${ENGRAM_LOCOMO_TOPK}" \
    --answerer "${ENGRAM_ANSWERER}" \
    --judge "${ENGRAM_JUDGE}" \
    --save "${OUT}" \
    --quiet \
    > /tmp/engram-ar.out 2>&1 || {
        echo "[eval] bench FAILED, see /tmp/engram-ar.out" >&2
        tail -20 /tmp/engram-ar.out >&2
        exit 3
    }

# Extract accuracy from the saved report (the --json envelope on stdout is
# nested; reading the --save file is simpler).
jq -r '.accuracy' "${OUT}"
