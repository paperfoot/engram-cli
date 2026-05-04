#!/usr/bin/env python3
"""jina_reranker_v3_server.py — MLX sidecar for jina-reranker-v3.

Loads jinaai/jina-reranker-v3-mlx (0.6B listwise reranker, Qwen3-0.6B
backbone, cosine-similarity scoring via MLP projector) and exposes the
same HTTP contract as the zerank-2 sidecar:

  GET  /health  → {"status": "ok", "model": "jina-reranker-v3-mlx"}
  POST /rerank  → body {"query": ..., "documents": [...], "top_k": N}
                  resp {"results": [{"index": i, "score": s}, ...],
                        "elapsed_ms": int}

jina-reranker-v3 is fundamentally different from zerank-2:
  - Listwise: all documents + query go into ONE prompt
  - Special tokens: <|embed_token|> after each doc, <|rerank_token|> after query
  - Scoring: extract hidden states at special token positions, project via
    2-layer MLP, cosine-similarity between query and doc embeddings
  - Max 64 documents per call, 131K context window

Port: 8766 (default, same as zerank-2 — only run one at a time).
"""

import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Dict, List, Optional

import mlx.core as mx
import mlx.nn as nn
import numpy as np
from safetensors import safe_open

MODEL_ID = os.environ.get(
    "ENGRAM_JINA_V3_PATH", "jinaai/jina-reranker-v3-mlx"
)
PORT = int(os.environ.get("ENGRAM_ZERANK_MLX_PORT", "8766"))
HOST = os.environ.get("ENGRAM_ZERANK_MLX_HOST", "127.0.0.1")
MAX_DOCS = 64  # jina-v3 hard limit

DOC_EMBED_TOKEN_ID = 151670   # <|embed_token|>
QUERY_EMBED_TOKEN_ID = 151671  # <|rerank_token|>


# ---------------------------------------------------------------------------
# MLP Projector (2-layer, no bias, ReLU)
# ---------------------------------------------------------------------------

class MLPProjector(nn.Module):
    def __init__(self, hidden_size: int = 1024, proj_size: int = 512):
        super().__init__()
        self.linear1 = nn.Linear(hidden_size, proj_size, bias=False)
        self.linear2 = nn.Linear(proj_size, proj_size, bias=False)

    def __call__(self, x):
        return self.linear2(nn.relu(self.linear1(x)))


def load_projector(path: str, hidden_size: int = 1024, proj_size: int = 512) -> MLPProjector:
    proj = MLPProjector(hidden_size, proj_size)
    with safe_open(path, framework="numpy") as f:
        proj.linear1.weight = mx.array(f.get_tensor("linear1.weight"))
        proj.linear2.weight = mx.array(f.get_tensor("linear2.weight"))
    return proj


# ---------------------------------------------------------------------------
# Model loading
# ---------------------------------------------------------------------------

def load_model() -> Dict[str, Any]:
    from huggingface_hub import snapshot_download
    import mlx_lm

    local_path = MODEL_ID
    if not os.path.isdir(local_path):
        local_path = snapshot_download(MODEL_ID)

    model, tokenizer = mlx_lm.load(local_path)
    model.eval()

    projector_path = os.path.join(local_path, "projector.safetensors")
    if not os.path.exists(projector_path):
        from huggingface_hub import hf_hub_download
        projector_path = hf_hub_download(MODEL_ID, "projector.safetensors")

    # Read hidden_size from config
    cfg_path = os.path.join(local_path, "config.json")
    hidden_size = 1024
    if os.path.exists(cfg_path):
        with open(cfg_path) as f:
            cfg = json.load(f)
        hidden_size = cfg.get("hidden_size", 1024)

    projector = load_projector(projector_path, hidden_size, 512)

    return {
        "model": model,
        "tokenizer": tokenizer,
        "projector": projector,
        "local_path": local_path,
    }


# ---------------------------------------------------------------------------
# Prompt formatting (listwise chat template)
# ---------------------------------------------------------------------------

SYSTEM_PROMPT = (
    "You are a search relevance expert who can determine a ranking of the "
    "passages based on how relevant they are to the query. "
    "If the query is a question, how relevant a passage is depends on how "
    "well it answers the question. "
    "If not, try to analyze the intent of the query and assess how well "
    "each passage satisfies the intent. "
    "If an instruction is provided, you should follow the instruction when "
    "determining the ranking."
)

SPECIAL_TOKENS = {"<|embed_token|>", "<|rerank_token|>"}


def _sanitize(text: str) -> str:
    for tok in SPECIAL_TOKENS:
        text = text.replace(tok, "")
    return text


def build_prompt(query: str, docs: List[str]) -> str:
    query = _sanitize(query)
    docs = [_sanitize(d) for d in docs]

    prefix = (
        f"<|im_start|>system\n{SYSTEM_PROMPT}<|im_end|>\n"
        f"<|im_start|>user\n"
    )
    suffix = (
        "<|im_end|>\n<|im_start|>assistant\n"
        "<think>\n\n</think>\n\n"
    )

    body = (
        f"I will provide you with {len(docs)} passages, each indicated by "
        f"a numerical identifier. "
        f"Rank the passages based on their relevance to query: {query}\n"
    )
    for i, doc in enumerate(docs):
        body += f'<passage id="{i}">\n{doc}<|embed_token|>\n</passage>\n'
    body += f"<query>\n{query}<|rerank_token|>\n</query>"

    return prefix + body + suffix


# ---------------------------------------------------------------------------
# Scoring
# ---------------------------------------------------------------------------

_SCORE_LOCK = threading.Lock()


def score_rerank(bundle: Dict[str, Any], query: str, docs: List[str], top_k: int) -> List[Dict]:
    if not docs:
        return []

    model = bundle["model"]
    tokenizer = bundle["tokenizer"]
    projector = bundle["projector"]

    # Truncate to max docs
    docs = docs[:MAX_DOCS]

    prompt = build_prompt(query, docs)
    input_ids = tokenizer.encode(prompt)

    # Forward pass — get hidden states
    hidden = model.model(mx.array([input_ids]))  # [1, S, H]
    hidden = hidden[0]  # [S, H]
    mx.eval(hidden)

    # Find special token positions
    ids_np = np.array(input_ids)
    query_pos = np.where(ids_np == QUERY_EMBED_TOKEN_ID)[0]
    doc_pos = np.where(ids_np == DOC_EMBED_TOKEN_ID)[0]

    if len(query_pos) == 0 or len(doc_pos) == 0:
        sys.stderr.write(
            f"[jina-v3] token positions missing: query={len(query_pos)} doc={len(doc_pos)}\n"
        )
        # Fallback: return uniform scores
        return [{"index": i, "score": 0.0} for i in range(min(top_k, len(docs)))]

    # Extract embeddings at special token positions
    q_hidden = hidden[int(query_pos[0])]  # [H]
    q_embed = projector(mx.expand_dims(q_hidden, 0))  # [1, 512]

    doc_hiddens = mx.stack([hidden[int(p)] for p in doc_pos[:len(docs)]])  # [N, H]
    doc_embeds = projector(doc_hiddens)  # [N, 512]

    # Cosine similarity
    q_norm = mx.sqrt(mx.sum(q_embed * q_embed, axis=-1, keepdims=True))
    d_norm = mx.sqrt(mx.sum(doc_embeds * doc_embeds, axis=-1, keepdims=True))
    scores = mx.sum(q_embed * doc_embeds, axis=-1) / (q_norm.squeeze() * d_norm.squeeze() + 1e-8)
    mx.eval(scores)

    scores_list = scores.tolist()
    indexed = sorted(
        ((i, float(s)) for i, s in enumerate(scores_list)),
        key=lambda x: x[1],
        reverse=True,
    )[:top_k]

    return [{"index": i, "score": s} for i, s in indexed]


# ---------------------------------------------------------------------------
# HTTP server (same contract as zerank sidecar)
# ---------------------------------------------------------------------------

BUNDLE: Dict[str, Any] = {}


class SingleThreadedHTTPServer(HTTPServer):
    allow_reuse_address = True


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self._safe_send(200, {"status": "ok", "model": "jina-reranker-v3-mlx"})
            return
        self._safe_send(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/rerank":
            self._safe_send(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("content-length", 0))
            raw = self.rfile.read(length)
            body = json.loads(raw)
            query = body.get("query")
            docs = body.get("documents") or []
            top_k = body.get("top_k", len(docs))
            if not isinstance(query, str) or not query.strip():
                self._safe_send(400, {"error": "query must be a non-empty string"})
                return
            if not isinstance(docs, list):
                self._safe_send(400, {"error": "documents must be a list"})
                return
            if not docs:
                self._safe_send(200, {"results": [], "elapsed_ms": 0})
                return

            t0 = time.perf_counter()
            with _SCORE_LOCK:
                results = score_rerank(BUNDLE, query, [str(d) for d in docs], int(top_k))
            elapsed_ms = int((time.perf_counter() - t0) * 1000)
            self._safe_send(200, {"results": results, "elapsed_ms": elapsed_ms})
        except BrokenPipeError:
            return
        except Exception as e:
            sys.stderr.write(f"rerank error: {e}\n")
            import traceback
            traceback.print_exc(file=sys.stderr)
            self._safe_send(500, {"error": str(e)})

    def _safe_send(self, status: int, body: Dict[str, Any]) -> None:
        try:
            data = json.dumps(body).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError):
            return
        except Exception as e:
            sys.stderr.write(f"_safe_send error: {e}\n")

    def log_message(self, *args, **kwargs):
        return


def main() -> int:
    global BUNDLE
    sys.stdout.write(f"loading {MODEL_ID} via mlx_lm...\n")
    sys.stdout.flush()
    t0 = time.perf_counter()
    BUNDLE = load_model()
    sys.stdout.write(f"loaded in {time.perf_counter()-t0:.1f}s\n")
    sys.stdout.write(f"listening on http://{HOST}:{PORT}\n")
    sys.stdout.flush()

    server = SingleThreadedHTTPServer((HOST, PORT), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        sys.stdout.write("shutting down\n")
        server.shutdown()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
