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
            scores = MODEL.predict(pairs, show_progress_bar=False)
            elapsed_ms = int((time.perf_counter() - t0) * 1000)

            scores_list = scores.tolist() if hasattr(scores, "tolist") else list(scores)
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


def main():
    global MODEL
    host = "127.0.0.1"
    port = 8765

    sys.stdout.write(
        f"loading {MODEL_NAME} (revision={MODEL_REVISION}) — first run downloads ~8GB safetensors...\n"
    )
    sys.stdout.flush()
    t0 = time.perf_counter()
    from sentence_transformers import CrossEncoder

    MODEL = CrossEncoder(
        MODEL_NAME,
        revision=MODEL_REVISION,
        trust_remote_code=True,
    )
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
