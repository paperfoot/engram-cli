#!/usr/bin/env python3
"""zerank_server_mlx.py — MLX port of the zerank-2 reranker sidecar.

Loads `zeroentropy/zerank-2` (Qwen3-4B base + yes-token classifier head with
/5 calibration) via mlx_lm's existing Qwen3 implementation and exposes the
same HTTP contract as the PyTorch sidecar at `zerank_server.py`:

  GET  /health  → {"status": "ok", "model": "zerank-2-mlx"}
  POST /rerank  → body {"query": ..., "documents": [...], "top_k": N}
                  resp {"results": [{"index": i, "score": s}, ...],
                        "elapsed_ms": int}

Architectural notes
-------------------
zerank-2 is Qwen3-4B with:
  1. `tie_word_embeddings = True` (lm_head weight = embed_tokens weight)
  2. A "classifier" that is not a new head — it's just:
         score = logits[batch, last_non_pad_position, yes_token_id] / 5.0
     where `yes_token_id = 9454` (configurable via config.json).
  3. The HF config has `model_type: "zeroentropy"`, which mlx_lm does not
     recognise. We symlink the model dir into a tempdir and patch
     `model_type` → `qwen3` so mlx_lm uses its native Qwen3 code path.

Padding mask: Qwen3 is decoder-only with causal attention. With
`padding_side = "right"` the hidden state at the last non-pad position
only attends to real tokens (causal mask enforces it), so we don't need
to supply a padding mask to mlx_lm's forward pass. Output at the extraction
position is unaffected by trailing padding.

Dev port is 8766 (default) to avoid the legacy 8765 orphan; override via
`ENGRAM_ZERANK_MLX_PORT`.
"""

import json
import os
import shutil
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Any, Dict, List, Tuple

import mlx.core as mx
import numpy as np
from huggingface_hub import snapshot_download

MODEL_NAME = "zeroentropy/zerank-2"
MODEL_REVISION = "refs/pr/2"
DEFAULT_YES_TOKEN_ID = 9454
DEFAULT_PAD_TOKEN_ID = 151643

PORT = int(os.environ.get("ENGRAM_ZERANK_MLX_PORT", "8766"))
HOST = os.environ.get("ENGRAM_ZERANK_MLX_HOST", "127.0.0.1")


# ---------------------------------------------------------------------------
# Model loading
# ---------------------------------------------------------------------------


def _prepare_mlx_dir(src: str) -> str:
    """Symlink the HF snapshot into a tempdir with a patched config.json so
    mlx_lm.load() takes the Qwen3 code path instead of looking up the
    unrecognised `zeroentropy` model_type.

    We also rewrite tokenizer_config.json to use the stock Qwen2TokenizerFast
    class — the custom `ZeroEntropyTokenizer` subclass only overrides
    `__call__` to accept `pairs` and apply the chat template, which we do
    ourselves by inlining the im_start/im_end template below. This avoids
    needing `trust_remote_code=True` at tokenizer load time.
    """
    tmpdir = tempfile.mkdtemp(prefix="zerank2-mlx-")
    for name in os.listdir(src):
        src_path = os.path.join(src, name)
        real = os.path.realpath(src_path)
        os.symlink(real, os.path.join(tmpdir, name))

    # Patch config.json
    cfg_dst = os.path.join(tmpdir, "config.json")
    os.unlink(cfg_dst)
    with open(os.path.join(src, "config.json")) as f:
        cfg = json.load(f)
    cfg["model_type"] = "qwen3"
    cfg.pop("auto_map", None)
    # The original architecture name is ZeroEntropyForSequenceClassification;
    # mlx_lm doesn't read this (it uses model_type), but dropping it avoids
    # confusion if another tool parses the file.
    cfg["architectures"] = ["Qwen3ForCausalLM"]
    with open(cfg_dst, "w") as f:
        json.dump(cfg, f)

    # Patch tokenizer_config.json
    tc_dst = os.path.join(tmpdir, "tokenizer_config.json")
    os.unlink(tc_dst)
    with open(os.path.join(src, "tokenizer_config.json")) as f:
        tcfg = json.load(f)
    tcfg["tokenizer_class"] = "Qwen2TokenizerFast"
    tcfg.pop("auto_map", None)
    with open(tc_dst, "w") as f:
        json.dump(tcfg, f)

    return tmpdir


def _build_compiled_forward(model):
    """Compile only the transformer forward (not the downstream gather +
    yes-token extract) via `mx.compile(shapeless=True)`. Pattern lifted
    from `bgconley/qwen3-reranker-multi`: the gather+extract step is only
    a few hundred microseconds so compiling it is pointless AND it uses
    dynamic advanced indexing that shapeless mode can't shape-infer.

    Returns a callable `compiled(input_ids) -> hidden [B, S, H]` or None
    if compilation is unavailable (we fall back to eager in score_pairs).
    """
    def _fwd(input_ids):
        return model.model(input_ids)

    try:
        return mx.compile(_fwd, shapeless=True)
    except Exception as e:
        sys.stderr.write(f"[mlx] mx.compile failed, using eager fallback: {e}\n")
        return None


def load_model() -> Dict[str, Any]:
    """Load the zerank-2 MLX model and return a bundle for `score_pairs`.

    Two paths:

    1. `ENGRAM_ZERANK_MLX_PATH` set → load directly from that local directory.
       Used for pre-converted / pre-quantized variants (e.g. zerank-2-q8 from
       `mlx_lm.convert`). The directory must already have `model_type=qwen3`
       in config.json; no runtime patching is applied.

    2. Otherwise → snapshot_download the HF model, symlink into a tempdir,
       patch `config.json` (model_type zeroentropy→qwen3) and
       `tokenizer_config.json` (drop custom class), then mlx_lm.load.

    The loaded model is wrapped in a compiled score function via
    `mx.compile(shapeless=True)` so repeated calls reuse a fused Metal
    graph. Set `ENGRAM_ZERANK_MLX_NO_COMPILE=1` to disable compilation
    (useful when debugging numerical drift).
    """
    local_path = os.environ.get("ENGRAM_ZERANK_MLX_PATH")
    if local_path:
        if not os.path.isdir(local_path):
            raise FileNotFoundError(
                f"ENGRAM_ZERANK_MLX_PATH={local_path} does not exist"
            )
        cfg_path = os.path.join(local_path, "config.json")
        with open(cfg_path) as f:
            cfg = json.load(f)
        # Pre-converted variants may have dropped the yes_token_id field during
        # mlx_lm.convert; fall back to the zerank-2 default.
        yes_token_id = int(cfg.get("yes_token_id", DEFAULT_YES_TOKEN_ID))
        pad_token_id = int(cfg.get("pad_token_id", DEFAULT_PAD_TOKEN_ID))
        mlx_dir = local_path
    else:
        src = snapshot_download(MODEL_NAME, revision=MODEL_REVISION)
        with open(os.path.join(src, "config.json")) as f:
            orig_cfg = json.load(f)
        yes_token_id = int(orig_cfg.get("yes_token_id", DEFAULT_YES_TOKEN_ID))
        pad_token_id = int(orig_cfg.get("pad_token_id", DEFAULT_PAD_TOKEN_ID))
        mlx_dir = _prepare_mlx_dir(src)

    import mlx_lm

    model, tokenizer = mlx_lm.load(mlx_dir)
    model.eval()

    if os.environ.get("ENGRAM_ZERANK_MLX_NO_COMPILE"):
        compiled_forward = None
    else:
        compiled_forward = _build_compiled_forward(model)

    return {
        "model": model,
        "tokenizer": tokenizer,
        "yes_token_id": yes_token_id,
        "pad_token_id": pad_token_id,
        "mlx_dir": mlx_dir,
        "compiled_forward": compiled_forward,
    }


# ---------------------------------------------------------------------------
# Scoring
# ---------------------------------------------------------------------------


def _build_prompts(pairs: List[Tuple[str, str]]) -> List[str]:
    """Inline the Qwen3 chat template used by ZeroEntropyTokenizer.__call__:
        system = query, user = doc, then add_generation_prompt.
    """
    out: List[str] = []
    for query, doc in pairs:
        out.append(
            "<|im_start|>system\n"
            + str(query).strip()
            + "<|im_end|>\n<|im_start|>user\n"
            + str(doc).strip()
            + "<|im_end|>\n<|im_start|>assistant\n"
        )
    return out


def _encode_batch(
    tokenizer, prompts: List[str], max_length: int = 4096
) -> List[List[int]]:
    """Use the wrapped fast tokenizer to tokenize each prompt. mlx_lm's
    TokenizerWrapper exposes `.encode` directly."""
    ids: List[List[int]] = []
    for p in prompts:
        t = tokenizer.encode(p)
        if len(t) > max_length:
            t = t[:max_length]
        ids.append(list(t))
    return ids


def _pad_right(
    encoded: List[List[int]], pad_id: int
) -> Tuple[mx.array, mx.array]:
    """Right-pad to uniform length. Returns (ids [B,S] int32, mask [B,S] int32)."""
    max_len = max(len(e) for e in encoded)
    B = len(encoded)
    ids = np.full((B, max_len), pad_id, dtype=np.int32)
    mask = np.zeros((B, max_len), dtype=np.int32)
    for i, e in enumerate(encoded):
        L = len(e)
        ids[i, :L] = e
        mask[i, :L] = 1
    return mx.array(ids), mx.array(mask)


def score_pairs(bundle: Dict[str, Any], pairs: List[Tuple[str, str]]) -> List[float]:
    """Score a list of (query, doc) pairs. Returns list[float] of calibrated
    scores matching zerank-2's yes-token-logit / 5 convention."""
    if not pairs:
        return []

    model = bundle["model"]
    tokenizer = bundle["tokenizer"]
    yes_token_id: int = bundle["yes_token_id"]
    pad_token_id: int = bundle["pad_token_id"]

    prompts = _build_prompts(pairs)
    encoded = _encode_batch(tokenizer, prompts)
    input_ids, attention_mask = _pad_right(encoded, pad_token_id)

    # Transformer forward (compiled via mx.compile when available, eager
    # otherwise). Compiling just the forward (and not the gather+extract
    # below) is the pattern from bgconley/qwen3-reranker-multi — the
    # gather+extract is cheap and uses dynamic advanced indexing that
    # breaks mx.compile(shapeless=True).
    compiled_forward = bundle.get("compiled_forward")
    if compiled_forward is not None:
        hidden = compiled_forward(input_ids)  # [B, S, H]
    else:
        hidden = model.model(input_ids)  # [B, S, H]

    # Yes-token shortcut (quant-compatible): gather hidden at last non-pad
    # position per row, project ONLY that [B, H] slice through the tied
    # embedding to get [B, V], then take the yes_token_id column and /5.
    # This skips the full [B, S, V=151936] projection that model(...)
    # would compute, saving ~S× the work (S ≈ 150 typical).
    last_positions = mx.sum(attention_mask, axis=1) - 1
    B = input_ids.shape[0]
    rows = mx.arange(B)
    hidden_last = hidden[rows, last_positions, :]  # [B, H]
    logits_last = model.model.embed_tokens.as_linear(hidden_last)  # [B, V]
    yes_logits = logits_last[:, yes_token_id]  # [B]
    scores = yes_logits / 5.0

    mx.eval(scores)
    return [float(s) for s in scores.tolist()]


# ---------------------------------------------------------------------------
# HTTP server (same contract as zerank_server.py)
# ---------------------------------------------------------------------------


BUNDLE: Dict[str, Any] = None  # type: ignore

# Global mutex around the MLX forward pass. MLX is NOT thread-safe for
# concurrent calls into the same model (ml-explore/mlx#3078), and under
# load from a bench the stock ThreadingHTTPServer triggered a Metal
# GPU Address Fault (kIOGPUCommandBufferCallbackErrorPageFault, SIGABRT)
# after ~38 requests. We now use the single-threaded HTTPServer and
# belt-and-braces this lock so future concurrent callers still serialize
# on the model. The bench's Rust client issues rerank calls sequentially
# per question, so this costs nothing in practice.
_SCORE_LOCK = threading.Lock()


class SingleThreadedHTTPServer(HTTPServer):
    allow_reuse_address = True


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        if self.path == "/health":
            self._safe_send(200, {"status": "ok", "model": "zerank-2-mlx"})
            return
        self._safe_send(404, {"error": "not found"})

    def do_POST(self):  # noqa: N802
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
                self._safe_send(400, {"error": "documents must be a list of strings"})
                return
            if not docs:
                self._safe_send(200, {"results": []})
                return

            pairs = [(query, str(d)) for d in docs]
            t0 = time.perf_counter()
            with _SCORE_LOCK:
                scores = score_pairs(BUNDLE, pairs)
            elapsed_ms = int((time.perf_counter() - t0) * 1000)
            indexed = sorted(
                ((i, float(s)) for i, s in enumerate(scores)),
                key=lambda x: x[1],
                reverse=True,
            )[: int(top_k)]
            results = [{"index": i, "score": s} for i, s in indexed]
            self._safe_send(200, {"results": results, "elapsed_ms": elapsed_ms})
        except BrokenPipeError:
            # Client disconnected mid-response — don't try to write anything
            # else or we'll corrupt the connection (this was the zerank_server
            # crash loop bug).
            return
        except Exception as e:
            sys.stderr.write(f"rerank error: {e}\n")
            self._safe_send(500, {"error": str(e)})

    def _safe_send(self, status: int, body: Dict[str, Any]) -> None:
        """Single-shot response with guarded header writes. Never raises;
        on BrokenPipe we just give up on this connection so we don't trigger
        the double-header bug that plagued the PyTorch sidecar."""
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

    def log_message(self, *args, **kwargs):  # noqa: N802
        # Suppress per-request stdout chatter.
        return


def main() -> int:
    global BUNDLE
    sys.stdout.write(
        f"loading {MODEL_NAME} (revision={MODEL_REVISION}) via mlx_lm...\n"
    )
    sys.stdout.flush()
    t0 = time.perf_counter()
    BUNDLE = load_model()
    sys.stdout.write(f"loaded in {time.perf_counter()-t0:.1f}s\n")
    sys.stdout.write(f"yes_token_id: {BUNDLE['yes_token_id']}\n")
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
