#!/usr/bin/env python3
"""validate_mlx_reranker.py — correctness gate for zerank_server_mlx.py.

Loads the PyTorch reference (zeroentropy/zerank-2 via
AutoModelForSequenceClassification, trust_remote_code=True) and the MLX port
(`zerank_server_mlx.score_pairs`), runs both on a fixed set of (query, doc)
pairs, and asserts max score difference < TOL.

Exits 0 on pass, 1 on fail. Used as:
  a) the RED test before writing zerank_server_mlx.py
  b) the correctness gate for the autoresearch speed-optimisation loop
     (gate: this script must exit 0 before any speed experiment can be kept)

Run via uv so both envs resolve independently:
  uv run --with 'transformers<5.0,>=4.45' --with torch \
         --with mlx --with 'mlx-lm>=0.21' --with safetensors \
         crates/engram-rerank/python/validate_mlx_reranker.py
"""

import os
import sys
import time

# 20 diverse pairs: the first 5 are positive (query matches doc), the next 15
# mix positives with negatives and domain shifts so per-pair diffs surface any
# numerical drift that would be invisible on a single happy-path pair.
TEST_PAIRS = [
    ("What is the capital of France?", "Paris is the capital and most populous city of France."),
    ("What is the capital of France?", "Berlin is the capital of Germany."),
    ("Who wrote Hamlet?", "William Shakespeare wrote the tragedy Hamlet around 1600."),
    ("Who wrote Hamlet?", "Charles Dickens authored Oliver Twist in 1838."),
    ("How does photosynthesis work?",
     "Photosynthesis converts light energy into chemical energy in plants via chlorophyll."),
    ("How does photosynthesis work?", "The mitochondria is the powerhouse of the cell."),
    ("What causes rain?",
     "Rain forms when water vapor in the atmosphere condenses into droplets heavy enough to fall."),
    ("What causes rain?", "A magnet attracts iron filings."),
    ("List primary colors.", "Red, blue, and yellow are the three primary colors in the RYB model."),
    ("List primary colors.", "Python is a high-level programming language."),
    ("Define recursion.",
     "Recursion is a method where the solution depends on solutions to smaller instances of the same problem."),
    ("Define recursion.", "The Pacific is the largest of Earth's oceans."),
    ("Who was the first person on the moon?",
     "Neil Armstrong became the first human to walk on the Moon on July 20, 1969."),
    ("Who was the first person on the moon?", "Cats are commonly kept as household pets."),
    ("What is HTTPS?",
     "HTTPS is HTTP over TLS; it encrypts data between a browser and a server."),
    ("What is HTTPS?", "Mount Everest is the tallest mountain above sea level."),
    ("Explain gradient descent.",
     "Gradient descent is an optimization algorithm that iteratively moves parameters "
     "in the direction of steepest descent of a loss function."),
    ("Explain gradient descent.", "The ancient Romans built aqueducts to carry water."),
    ("What does a compiler do?",
     "A compiler translates source code written in a programming language into machine code."),
    ("What does a compiler do?", "Soccer is played with a round ball on a rectangular field."),
]

TOL = 5e-2  # bf16 vs bf16 cross-runtime; diffs come in 1/64 grains (bf16 LSB)
TOP_K_CHECK = 10  # top-K ranks must match exactly; deeper ranks may flip on tied scores
MODEL_NAME = "zeroentropy/zerank-2"
MODEL_REVISION = "refs/pr/2"


def run_pytorch_reference(pairs):
    """Load zerank-2 in PyTorch MPS/CPU and score pairs. Returns list[float]."""
    import torch
    from transformers import AutoTokenizer, AutoModelForSequenceClassification

    sys.stderr.write("[ref ] loading PyTorch reference...\n")
    t0 = time.perf_counter()
    tokenizer = AutoTokenizer.from_pretrained(
        MODEL_NAME, revision=MODEL_REVISION, trust_remote_code=True
    )
    model = AutoModelForSequenceClassification.from_pretrained(
        MODEL_NAME,
        revision=MODEL_REVISION,
        trust_remote_code=True,
        dtype=torch.bfloat16,  # match native dtype of the safetensors
    )
    model.eval()

    if torch.backends.mps.is_available():
        device = torch.device("mps")
    elif torch.cuda.is_available():
        device = torch.device("cuda")
    else:
        device = torch.device("cpu")
    model.to(device)
    sys.stderr.write(f"[ref ] loaded on {device} in {time.perf_counter()-t0:.1f}s\n")

    inputs = tokenizer(
        pairs, return_tensors="pt", padding=True, truncation=True, max_length=4096
    )
    inputs = {k: v.to(device) for k, v in inputs.items()}

    t0 = time.perf_counter()
    with torch.inference_mode():
        out = model(**inputs)
    scores = out.logits.squeeze(-1).detach().float().cpu().tolist()
    sys.stderr.write(f"[ref ] scored {len(pairs)} pairs in {time.perf_counter()-t0:.2f}s\n")

    # Free GPU memory immediately — the MLX run comes next and we don't want
    # both models sitting in unified memory at the same time.
    del model
    del inputs
    import gc
    gc.collect()
    if torch.backends.mps.is_available():
        torch.mps.empty_cache()

    return scores


def run_mlx_candidate(pairs):
    """Load the MLX port and score pairs. Returns list[float]."""
    sys.stderr.write("[mlx ] loading MLX candidate...\n")
    t0 = time.perf_counter()
    # Import here so that a missing / broken zerank_server_mlx doesn't crash
    # the PyTorch reference run.
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import zerank_server_mlx as mlx_mod  # noqa: E402

    model_bundle = mlx_mod.load_model()
    sys.stderr.write(f"[mlx ] loaded in {time.perf_counter()-t0:.1f}s\n")

    t0 = time.perf_counter()
    scores = mlx_mod.score_pairs(model_bundle, pairs)
    sys.stderr.write(f"[mlx ] scored {len(pairs)} pairs in {time.perf_counter()-t0:.2f}s\n")
    return scores


def main():
    pt_scores = run_pytorch_reference(TEST_PAIRS)
    mlx_scores = run_mlx_candidate(TEST_PAIRS)

    assert len(pt_scores) == len(mlx_scores) == len(TEST_PAIRS), (
        f"length mismatch: pt={len(pt_scores)} mlx={len(mlx_scores)} "
        f"pairs={len(TEST_PAIRS)}"
    )

    diffs = [abs(a - b) for a, b in zip(pt_scores, mlx_scores)]
    max_diff = max(diffs)
    mean_diff = sum(diffs) / len(diffs)

    # Per-pair breakdown so we can see where drift accumulates.
    print("idx  pytorch      mlx          diff")
    print("-" * 40)
    for i, (pt, mlx, d) in enumerate(zip(pt_scores, mlx_scores, diffs)):
        marker = " " if d < TOL else "!"
        print(f"{i:3d}  {pt:+.6f}  {mlx:+.6f}  {d:.2e} {marker}")
    print("-" * 40)
    print(f"mean diff: {mean_diff:.2e}")
    print(f"max  diff: {max_diff:.2e}  (tol={TOL:.0e})")

    # Ranking-level agreement: the two runtimes should rank the pairs
    # identically at the top. Below the top-K, tied scores can flip harmlessly.
    pt_rank = sorted(range(len(pt_scores)), key=lambda i: pt_scores[i], reverse=True)
    mlx_rank = sorted(range(len(mlx_scores)), key=lambda i: mlx_scores[i], reverse=True)
    top_k_match = pt_rank[:TOP_K_CHECK] == mlx_rank[:TOP_K_CHECK]
    full_match = pt_rank == mlx_rank
    print(f"top-{TOP_K_CHECK} ranking identical: {top_k_match}")
    print(f"full ranking identical:  {full_match}")
    if not top_k_match:
        print(f"  pt_rank[:K]:  {pt_rank[:TOP_K_CHECK]}")
        print(f"  mlx_rank[:K]: {mlx_rank[:TOP_K_CHECK]}")

    ok = max_diff < TOL and top_k_match
    print("PASS" if ok else "FAIL")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
