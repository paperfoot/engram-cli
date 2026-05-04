#!/usr/bin/env python3
"""benchmark_mlx_reranker.py — throughput benchmark for zerank_server_mlx.

Measures reranker throughput on a fixed synthetic workload:
  N_QUERIES distinct queries × DOCS_PER_QUERY candidate documents
= N_QUERIES * DOCS_PER_QUERY total pair scores.

Runs three phases:
  1) warm-up pass (1 query, DOCS_PER_QUERY docs) to prime the graph / caches
  2) measured pass over all N_QUERIES queries, each issued as one score_pairs
     call of DOCS_PER_QUERY pairs — matches the real rerank call pattern
  3) per-request latency summary and peak memory via mx.get_peak_memory()

The goal is to compare successive optimisation variants of
`zerank_server_mlx.py` and produce a single JSON result file per variant:
  bench-results/<variant>.json

Each JSON has: variant, n_queries, docs_per_query, total_pairs,
total_time_s, pairs_per_sec, latency_ms { p50, p90, p95, p99, max },
peak_ram_gb, commit, timestamp.

Usage:
  uv run --with mlx --with 'mlx-lm>=0.21' --with 'transformers<5.0,>=4.45' \
         --with safetensors --with huggingface_hub --with numpy \
         crates/engram-rerank/python/benchmark_mlx_reranker.py \
         --variant v0-baseline
"""

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from statistics import mean

# Deterministic-ish synthetic workload: 20 queries × 30 docs = 600 pair scores.
# Queries are phrased from the same template to make token lengths similar
# across runs (stable latencies). Docs mix topic-matched with distractors.
N_QUERIES = 20
DOCS_PER_QUERY = 30

QUERIES = [
    "What are the main health benefits of regular exercise?",
    "How does the human immune system fight viral infections?",
    "Explain the causes of the 2008 financial crisis.",
    "Describe the process of cellular respiration in eukaryotic cells.",
    "What are the primary drivers of climate change?",
    "How does quantum entanglement work?",
    "Summarize the plot of Shakespeare's Hamlet.",
    "What is the role of mitochondria in a cell?",
    "How do neural networks learn from training data?",
    "Explain the theory of plate tectonics.",
    "What are the key differences between RNA and DNA?",
    "How did the Roman Empire rise and fall?",
    "Describe how gradient descent optimizes a loss function.",
    "What is the purpose of the Krebs cycle?",
    "How does a nuclear reactor generate electricity?",
    "Explain how GPS satellites determine location on Earth.",
    "What causes the phases of the moon?",
    "How does the internet route packets between computers?",
    "Describe the process of natural selection in evolution.",
    "What are the main principles of supply and demand?",
]

# 30 generic candidate documents. A real rerank call typically has 20-50
# candidates retrieved from a dense index; we keep the lengths varied so
# tokenisation isn't perfectly uniform (more realistic timing).
DOCS = [
    "Regular exercise improves cardiovascular health, strengthens muscles, increases flexibility, and releases endorphins that enhance mood. Sustained activity reduces chronic disease risk.",
    "The immune system uses white blood cells, antibodies, and cytokines to identify and destroy pathogens. Viral infections trigger interferon responses and memory B-cell formation.",
    "The 2008 financial crisis was triggered by a housing bubble, subprime lending, and complex mortgage-backed securities that failed to accurately reflect underlying risk.",
    "Cellular respiration converts glucose and oxygen into ATP through glycolysis, the citric acid cycle, and oxidative phosphorylation within mitochondria.",
    "Climate change is primarily driven by greenhouse gas emissions from fossil fuel combustion, deforestation, and industrial agriculture, trapping heat in the atmosphere.",
    "Quantum entanglement links particles so that the quantum state of one instantly correlates with another, regardless of distance, a phenomenon Einstein called spooky action.",
    "Hamlet is a prince of Denmark who seeks revenge against his uncle Claudius for murdering his father, the king, and marrying his mother.",
    "Mitochondria are organelles that produce ATP via oxidative phosphorylation and are often called the powerhouses of eukaryotic cells.",
    "Neural networks learn by adjusting weights via backpropagation, minimising a loss function computed from the difference between predicted and true outputs.",
    "Plate tectonics describes how Earth's lithosphere is divided into plates that move over the asthenosphere, causing earthquakes, volcanism, and mountain-building.",
    "RNA is single-stranded with ribose sugar and uracil, while DNA is double-stranded with deoxyribose and thymine. RNA is typically short-lived; DNA is stable and heritable.",
    "Rome rose through legion conquests, strong civic institutions, and Mediterranean trade, then fell due to overexpansion, invasions, and political instability.",
    "Gradient descent iteratively updates parameters in the opposite direction of the loss gradient, converging towards a local minimum of the objective function.",
    "The Krebs cycle, or citric acid cycle, oxidises acetyl-CoA into CO2 and transfers high-energy electrons to NAD and FAD for ATP synthesis.",
    "Nuclear reactors use controlled fission of uranium or plutonium to generate heat, which boils water into steam that drives turbines coupled to electric generators.",
    "GPS satellites broadcast precise timing signals; receivers triangulate position by comparing signal arrival times from at least four satellites simultaneously.",
    "The moon appears in phases because we see different portions of its sunlit surface as it orbits Earth, transitioning between new, crescent, quarter, gibbous, and full.",
    "The internet routes packets using IP addressing and routers that make hop-by-hop forwarding decisions based on routing tables updated by BGP and IGP protocols.",
    "Natural selection favours individuals whose heritable traits improve survival and reproduction, gradually shifting population trait distributions over generations.",
    "Supply and demand describe how prices equilibrate when sellers offer quantities buyers want: higher prices incentivise more supply and dampen demand.",
    "Python is a high-level, interpreted programming language created by Guido van Rossum, popular for data science, web development, and scripting automation tasks.",
    "The Pacific Ocean is Earth's largest body of water, covering about a third of the planet's surface and containing the deepest known trench, the Mariana.",
    "Cats were domesticated from wildcats roughly 10,000 years ago and are kept as pets worldwide, known for independence and nocturnal hunting instincts.",
    "Mount Everest, located in the Himalayas, stands 8,848 metres above sea level and is the tallest mountain on Earth when measured from sea level.",
    "Espresso is a concentrated coffee brewed by forcing hot pressurised water through finely ground beans, producing a rich crema on top.",
    "The Eiffel Tower was completed in 1889 as the entrance arch for the Paris World's Fair and remains one of the most recognisable landmarks in the world.",
    "Bitcoin is a decentralised digital currency operating on a proof-of-work blockchain, created in 2009 by the pseudonymous developer Satoshi Nakamoto.",
    "Jazz originated in New Orleans in the early 20th century, blending African rhythms, blues, and ragtime into an improvisational musical form.",
    "The Great Wall of China stretches thousands of kilometres across northern China, built over centuries by successive dynasties to deter nomadic invasions.",
    "The Amazon rainforest is a massive tropical forest in South America, home to an extraordinary diversity of plant, animal, and insect species.",
]

assert len(QUERIES) == N_QUERIES, f"expected {N_QUERIES} queries, got {len(QUERIES)}"
assert len(DOCS) >= DOCS_PER_QUERY, f"need at least {DOCS_PER_QUERY} docs, have {len(DOCS)}"
DOCS = DOCS[:DOCS_PER_QUERY]


def _percentile(values, p):
    """Simple percentile without numpy dependency in the math hot path."""
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * p / 100.0
    f = int(k)
    c = min(f + 1, len(s) - 1)
    if f == c:
        return s[f]
    return s[f] + (s[c] - s[f]) * (k - f)


def _git_commit():
    try:
        out = subprocess.check_output(["git", "rev-parse", "--short", "HEAD"], text=True)
        return out.strip()
    except Exception:
        return "unknown"


def run_benchmark(variant: str, out_dir: Path) -> dict:
    # Import here so the benchmark can be re-imported per variant experiment.
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import zerank_server_mlx as m

    import mlx.core as mx

    print(f"[bench] variant={variant}")
    print(f"[bench] workload: {N_QUERIES} queries × {DOCS_PER_QUERY} docs = "
          f"{N_QUERIES * DOCS_PER_QUERY} total pairs")

    print("[bench] loading model...")
    t0 = time.perf_counter()
    bundle = m.load_model()
    load_time = time.perf_counter() - t0
    print(f"[bench] loaded in {load_time:.2f}s")

    # Warm-up: one call to prime mx.compile graphs, kv cache allocations,
    # Metal kernels, etc.
    print("[bench] warm-up pass...")
    t0 = time.perf_counter()
    warm_pairs = [(QUERIES[0], d) for d in DOCS]
    _ = m.score_pairs(bundle, warm_pairs)
    warm_time = time.perf_counter() - t0
    print(f"[bench] warm-up in {warm_time:.2f}s")

    # Measured pass: each query is one rerank call with DOCS_PER_QUERY pairs.
    print("[bench] measured pass...")
    latencies_ms = []
    all_scores = []
    # Reset peak memory just before the measured pass so we only capture
    # steady-state + query-time allocations.
    try:
        mx.reset_peak_memory()
    except AttributeError:
        pass
    t_start = time.perf_counter()
    for q in QUERIES:
        pairs = [(q, d) for d in DOCS]
        t0 = time.perf_counter()
        scores = m.score_pairs(bundle, pairs)
        latencies_ms.append((time.perf_counter() - t0) * 1000)
        all_scores.append(scores)
    total_time = time.perf_counter() - t_start

    # Peak memory (in GB). mx.get_peak_memory returns bytes in recent MLX.
    peak_bytes = None
    try:
        peak_bytes = mx.get_peak_memory()
    except Exception:
        try:
            peak_bytes = mx.metal.get_peak_memory()  # legacy API
        except Exception:
            peak_bytes = None
    peak_gb = (peak_bytes / (1024**3)) if peak_bytes is not None else None

    total_pairs = N_QUERIES * DOCS_PER_QUERY
    pairs_per_sec = total_pairs / total_time if total_time > 0 else 0.0

    result = {
        "variant": variant,
        "n_queries": N_QUERIES,
        "docs_per_query": DOCS_PER_QUERY,
        "total_pairs": total_pairs,
        "load_time_s": round(load_time, 3),
        "warmup_time_s": round(warm_time, 3),
        "total_time_s": round(total_time, 3),
        "pairs_per_sec": round(pairs_per_sec, 2),
        "latency_ms_per_call": {
            "mean": round(mean(latencies_ms), 1),
            "p50": round(_percentile(latencies_ms, 50), 1),
            "p90": round(_percentile(latencies_ms, 90), 1),
            "p95": round(_percentile(latencies_ms, 95), 1),
            "p99": round(_percentile(latencies_ms, 99), 1),
            "max": round(max(latencies_ms), 1),
        },
        "peak_ram_gb": round(peak_gb, 3) if peak_gb is not None else None,
        "commit": _git_commit(),
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }

    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{variant}.json"
    with out_path.open("w") as f:
        json.dump(result, f, indent=2)
        f.write("\n")
    print(f"[bench] wrote {out_path}")

    print("\n=== Summary ===")
    print(f"variant:         {variant}")
    print(f"total time:      {total_time:.2f}s")
    print(f"pairs/sec:       {pairs_per_sec:.1f}")
    print(f"latency / call:  mean={result['latency_ms_per_call']['mean']}ms "
          f"p50={result['latency_ms_per_call']['p50']}ms "
          f"p95={result['latency_ms_per_call']['p95']}ms "
          f"max={result['latency_ms_per_call']['max']}ms")
    if peak_gb is not None:
        print(f"peak RAM:        {peak_gb:.2f} GB")
    print(f"commit:          {result['commit']}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--variant", required=True, help="variant label, e.g. v0-baseline")
    parser.add_argument(
        "--out-dir",
        default="crates/engram-rerank/python/bench-results",
        help="directory to write the variant's JSON result",
    )
    args = parser.parse_args()
    run_benchmark(args.variant, Path(args.out_dir))


if __name__ == "__main__":
    main()
