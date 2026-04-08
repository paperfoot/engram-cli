#!/usr/bin/env python3
"""zerank_server.py — local zerank-2 reranker HTTP server for engram.

A thin sidecar that loads ZeroEntropy's zerank-2 (4B Qwen3-4B base, custom
modeling code via trust_remote_code) and exposes it as a localhost HTTP
endpoint that the engram Rust binary can call without leaving the machine.

Why a sidecar instead of a pure Rust path:
- zerank-2 uses a custom `modeling_zeranker.py` that's not loadable by ort,
  candle, or fastembed-rs (yet). Standard CrossEncoder loaders work via the
  `revision="refs/pr/2"` branch with `trust_remote_code=True`.
- Python overhead is irrelevant — the GPU forward pass dominates inference.
- When an ONNX export lands upstream we replace this with a pure Rust path.

Endpoints:
  GET  /health  → {"status": "ok", "model": "zerank-2"}
  POST /rerank  → body {"query": "...", "documents": ["...", "..."], "top_k": 5}
                  resp {"results": [{"index": 0, "score": 0.7}, ...]}

Usage:
  uv run --with sentence-transformers --with torch \
         crates/engram-rerank/python/zerank_server.py

  # then engram bench / engram recall with ENGRAM_RERANK_PROVIDER=zerank2
"""

import json
import sys
import time
from http.server import HTTPServer, BaseHTTPRequestHandler
from socketserver import ThreadingMixIn

# Loaded once on startup. The CrossEncoder.predict() call is thread-safe in
# practice for this model — torch handles batching internally.
MODEL = None
MODEL_NAME = "zeroentropy/zerank-2"
MODEL_REVISION = "refs/pr/2"  # PR #2 fixes config.json for batching


class ThreadingHTTPServer(ThreadingMixIn, HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            self._send(200, {"status": "ok", "model": MODEL_NAME})
            return
        self._send(404, {"error": "not found"})

    def do_POST(self):
        if self.path != "/rerank":
            self._send(404, {"error": "not found"})
            return
        try:
            length = int(self.headers.get("content-length", 0))
            raw = self.rfile.read(length)
            body = json.loads(raw)
            query = body.get("query")
            docs = body.get("documents") or []
            top_k = body.get("top_k", len(docs))
            if not isinstance(query, str) or not query.strip():
                self._send(400, {"error": "query must be a non-empty string"})
                return
            if not isinstance(docs, list):
                self._send(400, {"error": "documents must be a list of strings"})
                return
            if not docs:
                self._send(200, {"results": []})
                return

            pairs = [(query, str(d)) for d in docs]
            t0 = time.perf_counter()
            scores_list = MODEL.predict(pairs)
            elapsed_ms = int((time.perf_counter() - t0) * 1000)
            indexed = sorted(
                ((i, float(s)) for i, s in enumerate(scores_list)),
                key=lambda x: x[1],
                reverse=True,
            )[: int(top_k)]
            results = [{"index": i, "score": s} for i, s in indexed]
            self._send(200, {"results": results, "elapsed_ms": elapsed_ms})
        except Exception as e:
            sys.stderr.write(f"rerank error: {e}\n")
            self._send(500, {"error": str(e)})

    def _send(self, status, body):
        data = json.dumps(body).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args, **kwargs):
        # Suppress per-request stdout chatter — engram will log on its side.
        return


# Direct model + tokenizer (the CrossEncoder.predict path is incompatible
# with zerank-2's custom ZeroEntropyTokenizer.__call__ signature in
# sentence-transformers >= 3.0). We tokenize pairs manually, run the
# forward pass, and score by extracting the "yes" token logit at the
# last position — this is the listwise / yes-token scoring pattern from
# the model's modeling_zeranker.py.
TOKENIZER = None
YES_TOKEN_ID = None
DEVICE = None


class _Predictor:
    """Wraps tokenize+forward+score so the Handler doesn't need torch imports."""

    def __init__(self, tokenizer, model, yes_token_id, device):
        import torch  # noqa
        self.torch = torch
        self.tokenizer = tokenizer
        self.model = model
        self.yes_token_id = yes_token_id
        self.device = device

    def predict(self, pairs):
        """Score a list of (query, doc) tuples. Returns a list of floats.

        zerank-2's ZeroEntropyForSequenceClassification already extracts
        the yes-token logit at the last non-pad position internally and
        returns logits of shape [batch, 1] with the score divided by 5
        for calibration. We just squeeze and return.
        """
        torch = self.torch
        # ZeroEntropyTokenizer.__call__ accepts the pairs list directly and
        # applies the chat template internally.
        inputs = self.tokenizer(
            pairs,
            return_tensors="pt",
            padding=True,
            truncation=True,
            max_length=4096,  # well under the 32K context, keeps batches sane
        )
        inputs = {k: v.to(self.device) for k, v in inputs.items()}
        with torch.inference_mode():
            outputs = self.model(**inputs)
        # outputs.logits is already [batch, 1] — the calibrated yes-token logit.
        scores = outputs.logits.squeeze(-1)  # [batch]
        return scores.detach().float().cpu().tolist()


def main():
    global MODEL
    host = "127.0.0.1"
    port = 8765

    sys.stdout.write(
        f"loading {MODEL_NAME} (revision={MODEL_REVISION}) — first run downloads ~8GB safetensors...\n"
    )
    sys.stdout.flush()
    t0 = time.perf_counter()

    import torch
    from transformers import AutoTokenizer, AutoModelForSequenceClassification

    # Apple Silicon: Metal Performance Shaders backend.
    if torch.backends.mps.is_available():
        device = torch.device("mps")
    elif torch.cuda.is_available():
        device = torch.device("cuda")
    else:
        device = torch.device("cpu")
    sys.stdout.write(f"device: {device}\n")
    sys.stdout.flush()

    tokenizer = AutoTokenizer.from_pretrained(
        MODEL_NAME,
        revision=MODEL_REVISION,
        trust_remote_code=True,
    )
    # zerank-2 registers as ZeroEntropyForSequenceClassification but its
    # forward() returns CausalLMOutputWithPast — we extract the yes-token
    # logit from the last position manually below.
    model = AutoModelForSequenceClassification.from_pretrained(
        MODEL_NAME,
        revision=MODEL_REVISION,
        trust_remote_code=True,
        dtype=torch.float16,  # half precision for M-series Metal
    )
    model.eval()
    model.to(device)

    yes_token_id = getattr(model.config, "yes_token_id", 9454)
    sys.stdout.write(f"yes_token_id: {yes_token_id}\n")

    MODEL = _Predictor(tokenizer, model, yes_token_id, device)

    elapsed = time.perf_counter() - t0
    sys.stdout.write(f"loaded in {elapsed:.1f}s\n")
    sys.stdout.write(f"listening on http://{host}:{port}\n")
    sys.stdout.flush()

    server = ThreadingHTTPServer((host, port), Handler)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        sys.stdout.write("shutting down\n")
        server.shutdown()


if __name__ == "__main__":
    main()
