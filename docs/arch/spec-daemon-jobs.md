# spec-daemon-jobs.md — implementation spec for the daemon-jobs lane (wave 4)

Binding inputs, in precedence order: `docs/arch/interfaces.md` **v2** (rulings R1–R15 override
everything below them), `docs/arch/redesign-brief.md` (move 4), `docs/arch/coherence-report.md`
(CM-9, CD-1, CD-4, CD-5, CD-6, CC-5, CC-7, IN-23, IN-24, IN-30), and `docs/arch/spec-fact-semantics.md`
where the two lanes touch (decision recording, `ExtractedFact`, `suggested_action` vocabulary).

Everything in this document is written against the code as it stands on `arch/v0.4` at
`81e6a87`. Line numbers are from that tree, and were re-checked against `a239e0e` during the
adversarial review (`docs/arch/review-daemon-jobs.md`): the anchors in `serve.rs`, `facts.rs`,
`compile.rs` and `sqlite.rs` still hold. One thing has moved since the pin — `ErrorCode` has
landed in `engram-core` (`a239e0e`) while `CliError` in `crates/engram-cli/src/error.rs` is still
pre-R11 (`Storage → exit 1`, codes spelled `config_error` / `serde_error`). §3.4's `http_status`
is written against the **post-R11** `code()` strings, so it lands with (or after) contract-codegen's
`error.rs`, not before it.

---

## 1. Scope & non-goals

### 1.1 What this lane ships (wave 4)

| # | Deliverable | Primary artefact |
|---|---|---|
| D1 | **Single-writer `Core`** — one mutation entry point (`Core::write`), one write connection per process, no `await` inside a write transaction | `crates/engram-cli/src/engine.rs` (NEW) |
| D2 | **`Core` hosting**: `engramd` builds it at startup; the CLI embeds the same `Core` when no daemon is running | `context.rs`, `bin/engramd.rs`, `lib.rs` |
| D3 | **Real HTTP/1.1 parsing** — incremental header read, `Content-Length`, bounded bodies, per-connection `tokio::spawn`, keep-alive, timeouts, connection cap — replacing the single 64 KiB `read()` at `serve.rs:42-46` | `commands/serve.rs` |
| D4 | **Jobs API** — submit / status / list / subscribe / cancel / resume, for `compile` and `backfill`, resumable through a persisted cursor | `commands/jobs.rs`, `engram-storage/src/jobs.rs` (NEW), `serve.rs` |
| D5 | **Transactional `facts resolve` claim** — claim-before-mutate, exactly-one-changed-row, `ConflictAlreadyResolved` on a lost claim, supersession-cycle guard | `engine.rs::Core::resolve_conflict` (call site patched into `commands/facts.rs`) |
| D6 | **`/v1/recall` gains `conflicts`** — by consuming the shared builders in `respond.rs`; `serve.rs` constructs no bespoke JSON | `commands/serve.rs` |
| D7 | **Sleep-time compile trigger** — write-volume + idle + budget-capped auto-submission of compile jobs, daemon-only, LLM-off by default | `engine.rs::sleep`, `commands/serve.rs` |
| D8 | **`--store <dir>` flag** (R14, deferred to this wave) | `lib.rs`, `bin/engramd.rs` (+ one clap line from contract-codegen) |
| D9 | **Honest `/health`** — writer queue depth, job counts, sleep-compile state, store path, schema version | `commands/serve.rs` |

### 1.2 Non-goals — explicitly out of scope for wave 4

1. **The CLI does not proxy its mutations to a running daemon.** "CLI embeds the core when no
   daemon" is implemented as: the CLI always embeds a `Core`; daemon *detection* is used for one
   thing only — deciding whether a submitted job runs inline or is left for the daemon's
   scheduler (§4.6). Routing `engram remember` through HTTP is a separate, later change and is
   not required by any defect in the brief.
2. **`Core::write` does not become the only writer in the tree this wave.** It becomes the only
   *sanctioned* writer, with a migration table (§7.3) naming which call sites move when. Lanes
   that still write through `ctx.store` in waves 2–3 keep working; SQLite's WAL + 5 s
   `busy_timeout` (`sqlite.rs:806`) covers the transitional overlap.
3. **No new HTTP framework.** No `hyper`, no `axum`, no new workspace dependency. The parser is
   ~200 lines of `tokio::io` in `serve.rs`, because the surface is nine endpoints on loopback and
   a dependency here would be the largest single addition in the build.
4. **No TLS, no multi-tenant auth.** Bearer token on non-loopback binds, as today.
5. **`reflect` jobs are registered but not implemented.** `JobKind::Reflect` is reserved and its
   runner slot is left empty for reasoning-layer (wave 5); submitting one returns
   `bad_input: unknown job kind 'reflect' (not yet implemented)` rather than a queued row that
   nothing will ever run.
6. **No chunked `Transfer-Encoding` on request bodies** — answered `501` with a named code, never
   silently mis-parsed.
7. **Content-addressed derivation (move 3) is not built here.** Compile-job resume *consumes* it
   if storage-journal has landed it, and refuses to silently re-spend if it has not (§4.5).

### 1.3 The defects this lane closes

| Anchor | Defect | Deliverable |
|---|---|---|
| `facts.rs:451-506` | `facts resolve` is non-transactional and ignores the claim boolean: two concurrent resolves can supersede both facts of a pair and both report success | D5 |
| `serve.rs:42-46` | one 64 KiB `read()` is the entire HTTP framing: a body split across TCP segments is truncated and parsed as malformed JSON | D3 |
| `serve.rs:34-39` | connections are handled serially in the accept loop — one slow request blocks every other client | D3 |
| `serve.rs:148-217` | `POST /v1/compile` runs the whole compile inline on the connection; a 1700 s compile is a 1700 s HTTP request | D4 |
| `serve.rs:577-610` | `/v1/recall` has no `conflicts` array, so an HTTP caller silently gets one side of a live contradiction that the CLI surfaces (`recall.rs:295-354`) | D6 |
| `serve.rs:129` | `SqliteStore::open()` (and therefore `migrate()`) runs on **every** request | D1/D3 |
| brief move 4 | write locks held across network calls | D1 |
| R14 / CD-5 | `--store` exists in no lane's wave until this one | D8 |

### 1.4 Files this lane owns

`crates/engram-cli/src/commands/serve.rs`, `commands/jobs.rs`, `commands/compile.rs`,
`crates/engram-cli/src/lib.rs`, `context.rs`, `main.rs`, `bin/engramd.rs`,
`crates/engram-cli/src/engine.rs` (NEW), `crates/engram-cli/tests/daemon_*.rs` (NEW),
plus — by scoped exception requested in §9.1 — the NEW file
`crates/engram-storage/src/jobs.rs` (an `impl SqliteStore` block in a fresh file; it touches no
existing line of `sqlite.rs`).

Everything else goes through `cross_lane_patches` (§10).

---

## 2. The single-writer `Core`

### 2.1 Why a dedicated writer thread and not a mutex

`rusqlite::Connection` is `Send` but **not** `Sync`, and `SqliteStore` (`sqlite.rs:795-813`) owns
one. A `Core` shared across `tokio::spawn`ed connection tasks therefore cannot hand out `&SqliteStore`.
The two shapes that work are `Arc<Mutex<SqliteStore>>` (which forces every caller to hold a lock
across whatever it does next — exactly the "write locks held across network calls" defect move 4
exists to kill) and a dedicated OS thread owning the connection behind a channel. We take the
second: the channel makes it *structurally impossible* to hold the write lock across an `await`,
because the closure that runs inside the transaction is `FnOnce(&WriteCtx) -> Result<T, CliError>`
and is not a future.

### 2.2 Module and types

New file `crates/engram-cli/src/engine.rs` (named `engine`, not `core`, so that `use core::…`
paths inside the crate keep resolving to the `core` crate without ambiguity). `lib.rs` gains
`pub mod engine;`.

```rust
// crates/engram-cli/src/engine.rs

/// The only sanctioned mutation entry point in the process.
///
/// One process, one write connection, one thread. Reads keep going through
/// `AppContext.store` (CLI) or the reader pool (daemon); writes queue here.
#[derive(Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

struct CoreInner {
    db_path: PathBuf,
    /// Spawned on the FIRST write, so `engram recall` never pays for a writer
    /// thread or a second connection it will not use.
    writer: OnceLock<WriterHandle>,
    /// Daemon-only: N reader connections, opened (and therefore migrated) once
    /// at startup instead of once per request (today: `serve.rs:129`).
    readers: OnceLock<ReaderPool>,
    jobs: JobRegistry,
    actor: RwLock<String>,          // "cli:<diary>" | "daemon" | "job:<uuid>"
    metrics: CoreMetrics,
    sleep: SleepState,
}

struct WriterHandle {
    tx: std::sync::mpsc::SyncSender<WriteTask>,   // bounded, capacity 256
    thread: Option<std::thread::JoinHandle<()>>,
}

type WriteTask = Box<dyn FnOnce(&SqliteStore) + Send + 'static>;

/// What a write closure is handed. Deliberately NOT `&SqliteStore` alone: the
/// actor string is what `EventDraft.actor` needs, and every event a write
/// appends must be attributable without the caller re-deriving it.
pub struct WriteCtx<'a> {
    pub store: &'a SqliteStore,
    pub actor: &'a str,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CoreMetricsSnapshot {
    pub writes_total: u64,
    pub writes_failed: u64,
    pub queue_depth: usize,
    pub last_write_ms: u64,
    pub last_wait_ms: u64,
    pub p99_write_ms: u64,
    pub writer_started: bool,
}
```

### 2.3 The API

```rust
impl Core {
    /// `db_path` is resolved ONCE (`paths::db_path()`), so a `--store` flag or
    /// `ENGRAM_STORE` change mid-process cannot split the process across two
    /// databases.
    pub fn new(db_path: PathBuf, actor: impl Into<String>) -> Self;

    /// The single mutation entry point.
    ///
    /// `f` runs on the writer thread, inside one `BEGIN IMMEDIATE`, with no
    /// possibility of awaiting anything: embeddings, LLM calls and HTTP happen
    /// BEFORE the call and arrive as owned data. On `Err` the transaction is
    /// rolled back whole — including any journal event the closure appended,
    /// which is interfaces.md §2 invariant 1 ("every projection mutation in the
    /// SAME transaction as its event append") made unavoidable rather than
    /// merely documented.
    ///
    /// `op` is a static label ("remember", "facts.resolve", "job.checkpoint")
    /// used for tracing spans and the `/health` metrics; it is never parsed.
    pub async fn write<T, F>(&self, op: &'static str, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&WriteCtx<'_>) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static;

    /// Blocking form for synchronous call sites (`forget`, `memory list`,
    /// anything not already in an async fn). Identical semantics. Panics if
    /// called from inside a tokio worker with `block_in_place` unavailable —
    /// callers in async context must use `write`.
    pub fn write_blocking<T, F>(&self, op: &'static str, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&WriteCtx<'_>) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static;

    /// Daemon read path: runs `f` on a pooled reader connection via
    /// `spawn_blocking`. Readers never take the write lock, so a compile job
    /// cannot stall `/v1/recall`.
    pub async fn read<T, F>(&self, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&SqliteStore) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static;

    /// Opens `readers` eagerly with `n` connections. Called by `serve::run`
    /// only; the CLI never builds a pool.
    ///
    /// `Connection` is `Send` but NOT `Sync`, so the pool cannot hand `&SqliteStore`
    /// to a `spawn_blocking` closure: it is a `Vec<Arc<Mutex<SqliteStore>>>` (or an
    /// `ArrayQueue` of owned handles) from which `read` checks one out for the
    /// duration of the call and returns it. Stated because it is the difference
    /// between compiling and not.
    ///
    /// `SqliteStore::open` runs `migrate()` unconditionally (`sqlite.rs:812`), so
    /// `warm_readers(8)` would run eight migrations at startup — idempotent but
    /// pointless, and it contradicts §11.2's "migrate ran once" assertion. The
    /// pool therefore opens its connections through a migrate-free constructor
    /// (`SqliteStore::open_reader`, §10.1), after `AppContext`'s own `open` has
    /// migrated; if storage-journal declines that constructor, the test asserts
    /// "once per reader at startup, never per request" instead and the defect
    /// narrative (deleting `serve.rs:129`) is unchanged.
    pub fn warm_readers(&self, n: usize) -> Result<(), CliError>;

    pub fn set_actor(&self, actor: impl Into<String>);
    pub fn jobs(&self) -> &JobRegistry;
    pub fn metrics(&self) -> CoreMetricsSnapshot;

    /// Drains the queue, joins the writer thread. Called on daemon shutdown and
    /// from `Drop` with a 5 s grace. A job that is mid-checkpoint finishes its
    /// transaction; a job that has not started does not.
    pub async fn shutdown(&self, grace: Duration);
}
```

### 2.4 Writer-thread loop (normative)

```
loop {
    task = rx.recv()                       // blocks; None => channel closed => exit
    task(&store)                           // the closure owns its own oneshot reply
}
```

`Core::write` builds the task as:

```rust
let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
let actor = self.inner.actor.read().clone();
let queued_at = Instant::now();
let task: WriteTask = Box::new(move |store| {
    let waited = queued_at.elapsed();
    let started = Instant::now();
    let out = store.with_tx_res(|s| f(&WriteCtx { store: s, actor: &actor }));
    metrics.record(op, waited, started.elapsed(), out.is_err());
    let _ = reply_tx.send(out);            // receiver dropped == caller cancelled; the
                                           // transaction still committed, which is correct:
                                           // a committed write is not undone by a client
                                           // hanging up.
});
```

`with_tx_res` is the one storage addition this needs — `SqliteStore::with_tx` (`sqlite.rs:831`)
is hard-wired to `StorageError` and our closures return `CliError`. It is requested in §10.1 as a
three-line generalisation with `with_tx` delegating to it, so no existing caller changes.

**Send failure.** `SyncSender::try_send` full ⇒ `CliError::Transient("engram is busy: 256 writes
already queued")` mapping to `SqliteBusy` → HTTP 503 with `Retry-After: 1`. A bounded queue is
deliberate: an unbounded one converts a write stall into unbounded memory growth and a daemon
that dies instead of pushing back.

**Panic safety.** The writer thread wraps `task(&store)` in `catch_unwind`, and — because
`with_tx`'s current body does **not** unwind-guard the transaction — issues an explicit
`ROLLBACK` (`store.rollback_if_open()`) when it catches, before serving the next task. Without
that, one panic leaves `is_autocommit() == false` on the writer connection forever and every
later `with_tx` silently joins a transaction nothing will commit (`sqlite.rs:838-840`). The
rollback-on-unwind also lands inside `with_tx_res` itself (§10.1), so both layers are safe.
A panicking closure therefore poisons nothing: the transaction is rolled back, the caller gets
`CliError::Transient("write task panicked: …")`, and the thread keeps serving. A writer thread
that dies silently would turn every subsequent write into a hang; the `JoinHandle` is checked on
send failure and the `Core` reports `writer_dead` in `/health`.

### 2.5 Invariants (each has a test in §11)

1. **No `await` inside a write.** Enforced by the type: `F` returns `Result<T, CliError>`, not a
   future. Reviewers check nothing; the compiler does.
2. **Never call `Core::write` from inside a `ctx.store.with_tx` closure.** Two connections, both
   wanting `RESERVED`, is a guaranteed 5 s stall then `SQLITE_BUSY`. `Core::write` checks
   `store.in_transaction()` (the storage predicate requested in §10.1) and returns
   `CliError::Transient("nested write: Core::write called inside an open ctx.store transaction")`
   — a **real runtime check in every profile**, not a `debug_assert`. Release builds strip
   assertions, and the transitional dual-writer window (§7.3.1) runs for two waves in exactly the
   builds users install; a 5 s stall that only trips in production is not a guard.
3. **One write connection per process.** `CoreInner::writer` is a `OnceLock`; `Core` is `Clone`
   over an `Arc`, so cloning it into a job runner or a connection task cannot create a second.
4. **A committed write is journalled in the same transaction.** The closure appends its
   `EventDraft` via `WriteCtx::store`; nothing else can commit.
5. **`Core::write` is cancellation-safe at the boundary, not in the middle.** Dropping the future
   does not abort the transaction — it is already queued or already running. Documented on the
   method, because "the client hung up so the write did not happen" is a lie the daemon must
   never tell.

### 2.6 Where `Core` lives

```rust
// crates/engram-cli/src/context.rs
pub struct AppContext {
    pub format: OutputFormat,
    pub quiet: bool,
    pub store: SqliteStore,   // UNCHANGED — every existing read path keeps compiling
    pub core: Core,           // NEW
}

impl AppContext {
    pub fn new(format: OutputFormat, quiet: bool) -> Result<Self, CliError> {
        let db = paths::db_path();
        let store = SqliteStore::open(&db)?;          // runs migrate() exactly once
        let core = Core::new(db, default_actor());    // writer thread NOT started yet
        Ok(Self { format, quiet, store, core })
    }

    /// Test/daemon constructor: an explicit path, no `paths::` lookup. Used by
    /// `serve` (which may hold a `--store` override) and by unit tests that
    /// build a context over a TempDir without touching process env.
    pub fn with_db(format: OutputFormat, quiet: bool, db: PathBuf) -> Result<Self, CliError>;
}

/// `"cli:<diary>"` is not knowable before the subcommand is parsed, so the
/// default is `"cli"` and `dispatch` calls `ctx.core.set_actor()` once it knows.
fn default_actor() -> String;
```

`AppContext::new` keeping `store` means **zero** compile breakage for the other six lanes: they
continue to read (and, in waves 2–3, write) through `ctx.store`, and adopt `ctx.core` per §7.3.

---

## 3. Real HTTP: parsing, framing, concurrency

### 3.1 What is there now, and why each piece has to go

`handle_stream` (`serve.rs:42-121`) does one `stream.read(&mut buf)` into a 64 KiB buffer
(`:43-44`), `String::from_utf8_lossy`es the result (`:45`), splits on the first `\r\n\r\n`
(`:46`, `split_request` at `:694`) and treats whatever follows as the complete body. Four
consequences, all reachable from a well-behaved client:

- a body that arrives in two TCP segments (any `POST` over ~1.4 KB across a real link, or any
  client that writes headers and body separately — `curl` with `Expect: 100-continue`, Python
  `http.client`) is **truncated**, and the caller is told `serde error: EOF while parsing`;
- a body over 64 KiB is truncated with the same misleading error, and `--facts-json` payloads and
  document ingests are exactly that size class;
- `from_utf8_lossy` silently replaces invalid bytes, so a corrupt body becomes a *plausible* one;
- `handle_stream` is `await`ed directly in the accept loop (`:36`), so one slow request blocks
  every other client, and `route` opens (and migrates) a `SqliteStore` per request (`:129`).

### 3.2 New shape

```rust
// commands/serve.rs

pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub limits: Limits,
}

pub struct Limits {
    pub max_header_bytes: usize,   // ENGRAM_HTTP_MAX_HEADER_BYTES, default 32 * 1024
    pub max_body_bytes: u64,       // ENGRAM_HTTP_MAX_BODY_BYTES,   default 8 * 1024 * 1024
    pub max_connections: usize,    // ENGRAM_HTTP_MAX_CONNECTIONS,  default 64
    pub header_timeout: Duration,  // 10 s
    pub body_timeout: Duration,    // 30 s
    pub idle_timeout: Duration,    // 30 s between keep-alive requests
    pub max_requests_per_conn: u32,// 100, then `Connection: close`
    pub max_subscribers: usize,    // ENGRAM_JOBS_MAX_SUBSCRIBERS, default 16 — SSE streams are
                                   // exempt from max_requests_per_conn and hold a permit for
                                   // their whole life (§12.1 risk 6), so they need their own cap
    pub subscriber_max_age: Duration, // 1 h, then the stream is closed with a final `timeout` frame
}

struct Server {
    cfg: Arc<ServeConfig>,
    core: Core,
    permits: Arc<tokio::sync::Semaphore>,
    started_at: DateTime<Utc>,
}

pub async fn run(ctx: &AppContext, host: String, port: u16, token: Option<String>)
    -> Result<(), CliError>;                       // signature UNCHANGED (lib.rs:270, engramd.rs:37)
```

Accept loop:

```rust
let listener = TcpListener::bind((host.as_str(), port)).await?;
server.core.warm_readers(readers_for(&cfg))?;      // default max(2, cores/2), capped 8
server.spawn_scheduler();                          // §4.6
server.spawn_sleep_watch();                        // §6
write_descriptor(&cfg, listener.local_addr()?)?;   // §3.7
loop {
    let (stream, peer) = listener.accept().await?;
    let permit = match permits.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => { spawn_overloaded_response(stream); continue; }   // 503 + Retry-After: 1
    };
    let srv = server.clone();
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(e) = srv.serve_connection(stream, peer).await {
            tracing::debug!(%peer, "connection ended: {e}");
        }
    });
}
```

`accept()` errors are no longer fatal: `EMFILE`/`ECONNABORTED` currently kill the daemon through
the `?` at `serve.rs:35`. New rule — log, `sleep(50ms)` on `EMFILE`/`ENFILE`/`ENOBUFS`, continue;
only a listener error that is not per-connection (e.g. the socket was closed) breaks the loop.

### 3.3 The parser (normative)

`serve_connection` loops until close/idle-timeout/`max_requests_per_conn`:

1. **Headers.** Read into a `Vec<u8>` with `AsyncReadExt::read_buf`, scanning for `\r\n\r\n`
   after each chunk. Bare-LF terminators (`\n\n`) are accepted, because agents hand-roll requests
   with `printf`. Exceeding `max_header_bytes` ⇒ `431` `{code: "header_too_large"}`. EOF before
   any byte ⇒ clean close, no error. `header_timeout` elapsed ⇒ `408`, close.
2. **Request line.** `METHOD SP target SP HTTP/1.x`. Fewer than three tokens, or a version that
   is not `HTTP/1.0`/`HTTP/1.1` ⇒ `400` `{code: "bad_request"}` (byte-identical envelope to
   `serve.rs:51-66`, preserved for `wave05_daemon_*`). `HTTP/1.0` implies `Connection: close`
   unless `keep-alive` is explicitly requested.
3. **Headers → `Vec<(String /*lowercased name*/, String /*trimmed value*/)>`.** Obs-fold
   continuation lines (leading SP/HT) are rejected — `400 obsolete_line_folding` — rather than
   guessed at.
4. **Body length.**
   - `Transfer-Encoding` present and not `identity` ⇒ `501` `{code: "unsupported_transfer_encoding",
     suggestion: "Send a Content-Length body; chunked encoding is not supported."}`.
   - two `Content-Length` headers with different values, or a non-numeric value ⇒ `400`
     `{code: "bad_content_length"}`.
   - `Content-Length > max_body_bytes` ⇒ `413` `{code: "body_too_large"}` **without reading the
     body** (draining a 2 GB body to be polite is a free DoS), then `Connection: close`. Closing
     hard while the client is still writing sends an RST that destroys the already-queued
     response on most stacks, so the sequence is: write the response, flush, `shutdown(Write)`,
     then read-and-discard up to 64 KiB (or 250 ms, whichever first) before dropping the socket.
     Bounded politeness, not unbounded. §11.1's `body_over_limit_is_413` asserts the client
     actually receives the 413.
   - `POST`/`PUT`/`PATCH` with no `Content-Length` and no `Transfer-Encoding` ⇒ body is empty
     (length 0), which every current endpoint rejects as malformed JSON with the right code. We
     do **not** return `411`: `POST /v1/jobs/{id}/cancel` legitimately has no body.
5. **`Expect: 100-continue`** ⇒ write `HTTP/1.1 100 Continue\r\n\r\n` before reading the body.
   Without this, `curl` on bodies >1 KB waits a full second on every request.
6. **Body.** `read_exact` of exactly `Content-Length` bytes, seeded with whatever already sits in
   the header buffer past the terminator. Short read/EOF ⇒ `400 {code: "incomplete_body"}` —
   *this is the truncation defect, now named instead of mis-reported as bad JSON*. Timeout ⇒
   `408`.
7. **UTF-8.** `std::str::from_utf8` (not `from_utf8_lossy`) ⇒ invalid ⇒ `400
   {code: "malformed_json", message: "request body is not valid UTF-8"}`, matching
   `ErrorCode::MalformedJson` (R11).
8. **Auth.** Token check moves to the parsed header list and compares in time constant *in the
   token's contents* (`subtle`-free: a hand-written `u8` accumulate-XOR over equal-length byte
   slices). Length is still compared first and so is still distinguishable by timing — stated
   rather than papered over; the claim is "no byte-by-byte early exit", not "constant time". The
   current check (`serve.rs:721-726`) does `ends_with` on the raw line, which also accepts
   `Authorization: Bearer\ttoken` variants and exits early on the first differing byte.
   Response `401` envelope is byte-identical to `serve.rs:71-86`.
9. **Dispatch.** `route(&self, method, path, query, body).await`, unchanged in shape, taking
   `&Core` instead of opening a store.
10. **Response.** `Content-Length` always set; `Connection: keep-alive` (or `close` on the last
    request / an error that leaves the stream unframed); `x-engram-request-id: <uuid>` on every
    response and in every tracing span.

### 3.4 Status mapping

One function, one table, replacing the ad-hoc match at `serve.rs:93-102` (which special-cases
`serde_error` twice and maps every unknown to 500):

```rust
fn http_status(err: &CliError) -> u16 {
    match err.code() {                    // ErrorCode strings from contract-codegen's error.rs (R11)
        "bad_input" | "malformed_json" | "wrong_id_kind" | "config" => 400,
        "not_found"                                                  => 404,
        "conflict_already_resolved"                                  => 409,
        "rate_limited"                                               => 429,
        "sqlite_busy"                                                => 503,   // + Retry-After: 1
        _                                                            => 500,
    }
}
```

Compatibility notes, deliberate:

- **unknown endpoint stays `400`**, not `404`: `route`'s fallback (`serve.rs:323-325`) raises
  `BadInput`, and `wave05_daemon_*` asserts on that envelope. A genuinely missing *resource*
  (`GET /v1/jobs/<uuid>` for an unknown id) is the `404` case.
- `config` staying `400` preserves today's `exit_code 2 → 400` mapping.
- the `exit_code` field in the error envelope keeps its current values, per brief constraint 3.

### 3.5 Concurrency and the store

- Reads: `core.read(|s| …)` on the pooled connections. `route` no longer calls
  `SqliteStore::open(paths::db_path())` (deleting `serve.rs:129` removes a full `migrate()` from
  the hot path).
- Writes: `core.write("api.…", …)`.
- `POST /v1/kbs` (`serve.rs:141-145`), `POST /v1/budget` (`:308-321`) and
  `DELETE /v1/documents/{id}` (`:253-265`) are the three existing mutating endpoints; all three
  move to `core.write`.
- `POST /v1/ingest` keeps its subprocess shell-out (`serve.rs:621-676`) this wave — it is the one
  place where re-entering the CLI is cheaper than re-plumbing ingest, and the subprocess inherits
  `ENGRAM_STORE`, so `--store` isolation survives it (§8.2). Noted as debt in §12.

### 3.6 `/health`, honestly

```json
{"version":"1","status":"success","data":{
  "ok": true,
  "schema_version": 10,
  "store": "/…/db.sqlite",
  "uptime_s": 812,
  "writer": {"started": true, "alive": true, "queue_depth": 0, "writes_total": 41,
             "writes_failed": 0, "last_write_ms": 3, "p99_write_ms": 21},
  "jobs": {"running": 1, "queued": 0, "failed_24h": 0},
  "sleep_compile": {"enabled": true, "llm": false, "pending_writes": {"default": 12},
                    "next_eligible_at": "2026-08-01T22:41:03Z"},
  "connections": {"active": 2, "max": 64}
}}
```

`ok` is `false` — with HTTP `503` — when the writer thread is dead or the reader pool cannot be
opened. A health endpoint that returns `ok: true` while writes are impossible is precisely the
dishonesty 0.3.1 spent a release removing.

### 3.7 Daemon descriptor file

`serve::run` writes `<data_root>/engramd.json` on bind and removes it on clean shutdown:

```json
{"pid": 4711, "host": "127.0.0.1", "port": 8768, "started_at": "…", "version": "0.4.0",
 "store": "/…/db.sqlite", "token_required": false}
```

Used by the CLI for one decision only (§4.6): run a submitted job inline, or leave it for the
daemon. Liveness is `kill(pid, 0)` plus a `GET /health` with a 250 ms timeout; a stale descriptor
(process gone) is deleted by the first CLI that notices, so a crashed daemon cannot make every
later `jobs submit` hang waiting for a scheduler that does not exist.

### 3.8 Response building — `serve.rs` constructs no bespoke JSON

R12/CM-9 moved the shared builder to `crates/engram-cli/src/respond.rs`, owned by
**contract-codegen**, landing in wave 2. This lane is a **consumer only**.

**Signature mismatch flagged in review (finding 4), unresolved and escalated (§12.2.8):**
`spec-contract-codegen.md` §2.4 pins `respond.rs` as `resolve_conflict` / `attach` /
`resolve_command_string` — the *resolve payload* builder R12 actually names. The envelope builders
below (`success`, `error`, `listing`, `conflicts`) are this lane's ask, not something codegen has
agreed to. They are listed as a §10.2 patch request, and if the lead routes them elsewhere the
daemon calls whatever codegen exposes — what this lane will **not** do is ship a fourth private
envelope layer, which is exactly what CM-9 forbade. `ResolveAction` likewise exists in both specs
and must end up defined once (codegen's copy wins; `engine.rs` imports it).

Every response body in `serve.rs` is built from:

```rust
// crates/engram-cli/src/respond.rs — contract-codegen owns these; we call them
pub fn success(data: serde_json::Value, meta: Metadata) -> serde_json::Value;
pub fn error(err: &CliError) -> serde_json::Value;
pub fn listing(items: Vec<serde_json::Value>, returned: usize, total: i64,
               cursor: Option<String>) -> serde_json::Value;      // returned/total/cursor + legacy count
pub fn resolve_block(args: Vec<String>, allowed_actions: &[&str]) -> serde_json::Value;
pub fn conflicts(rows: &[ConflictInput<'_>]) -> Vec<serde_json::Value>;
```

The literal `json!({"version":"1","status":"success","data":{…}})` at
`serve.rs:131,136,144,214,227,231,238,247,260,266,275,283,289,302,316,427,468,668` and the error
envelopes at `:54,74,106` all become `respond::` calls. If `respond.rs` has not landed by the
time this lane starts, wave 4 **blocks on it** rather than shipping a fourth private copy — that
is the whole point of CM-9's pin.

**`/v1/recall` gains `conflicts`.** The CLI's `recall` already surfaces contested and superseded
facts (`recall.rs:295-354`, injected into the response by `recall.rs:~345`); the daemon's
`recall_api` (`serve.rs:469-619`) does not, so an agent driving engram over HTTP is handed one
side of a live contradiction with no signal. The fix is one field:

```rust
// inside recall_api, after `layered` is built and before the response is assembled
let conflicts = core.read({
    let kb = scope.kb.clone();
    let q = req.query.clone();
    move |s| crate::commands::recall::surface_conflicts_for(s, kb.as_deref(), &q)
}).await?;
…
"data": { …, "conflicts": conflicts, … }
```

`surface_conflicts_for` is `recall.rs`'s existing `surface_conflicts` (`recall.rs:296-353`) with
its `&AppContext` parameter replaced by `&SqliteStore` and made `pub` — a read-surfaces patch
requested in §10.3. The rendering of each element goes through `respond::conflicts`, so the CLI
and the daemon emit the identical `suggested_action` vocabulary (R7:
`keep-both | supersede-old | requires_context`) and the identical `resolve` /
`resolve_command` pair (R12) — the daemon must not be the surface that still prints
`supersede-old` on a safety pair after fact-semantics narrowed it.

The field is **additive**: a response with no contested facts carries `"conflicts": []`. It is an
empty array rather than an omitted key because absence-as-signal is already used for
`embedder_degraded` (`serve.rs:463-465`) and reusing it for two different meanings is how
presence checks rot.

---

## 4. Jobs

### 4.1 Data model — extend `compile_jobs`, do not add a table

`compile_jobs` (`sqlite.rs:674-685`, index at `:1064`) already stores id/kb/status/mode/
created_at/updated_at/message/metadata_json, is read by `jobs list|show` (`commands/jobs.rs`),
by `GET /v1/jobs` (`serve.rs:266-288`) and — load-bearing — by `MEMORY_CONSTRAINS_DERIVED`
(`sqlite.rs:779-790`), which proves a deleted memory has been compiled out by looking for a
`status = 'completed'` row whose `created_at` is later than the deletion.

Adding a parallel `jobs` table would fork that predicate. We extend the existing one with
additive, defaulted columns:

```sql
-- applied idempotently by ensure_jobs_schema(); every column has a DEFAULT so an
-- older engram binary (which lists columns explicitly on INSERT) still reads and
-- writes this table unchanged.
ALTER TABLE compile_jobs ADD COLUMN kind             TEXT    NOT NULL DEFAULT 'compile';
ALTER TABLE compile_jobs ADD COLUMN params_json      TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE compile_jobs ADD COLUMN cursor_json      TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE compile_jobs ADD COLUMN progress_done    INTEGER NOT NULL DEFAULT 0;
ALTER TABLE compile_jobs ADD COLUMN progress_total   INTEGER;
ALTER TABLE compile_jobs ADD COLUMN actor            TEXT    NOT NULL DEFAULT 'cli';
ALTER TABLE compile_jobs ADD COLUMN lease_owner      TEXT;
ALTER TABLE compile_jobs ADD COLUMN lease_expires_at TEXT;
ALTER TABLE compile_jobs ADD COLUMN attempts         INTEGER NOT NULL DEFAULT 0;
ALTER TABLE compile_jobs ADD COLUMN cancel_requested INTEGER NOT NULL DEFAULT 0;
ALTER TABLE compile_jobs ADD COLUMN started_at       TEXT;
ALTER TABLE compile_jobs ADD COLUMN finished_at      TEXT;

CREATE INDEX IF NOT EXISTS idx_jobs_status_created ON compile_jobs(status, created_at);
CREATE INDEX IF NOT EXISTS idx_jobs_kind_kb        ON compile_jobs(kind, kb, created_at);

-- Collapse pre-existing duplicates BEFORE the unique index, exactly as
-- interfaces §2.1 step 5 requires for idx_fcq_open_key. `create_compile_job`
-- (`sqlite.rs:2621-2646`) abandons-then-inserts in two unguarded statements, so
-- a store written by 0.3.x can legitimately hold two `running` rows for one KB;
-- a bare CREATE UNIQUE INDEX against that store ERRORS, inside schema setup,
-- and bricks every command.
UPDATE compile_jobs SET status = 'abandoned',
       message = COALESCE(message, 'superseded: duplicate live job collapsed at wave-4 schema setup'),
       updated_at = <now>
 WHERE status IN ('queued', 'running')
   AND id NOT IN (SELECT id FROM (SELECT id FROM compile_jobs c2
                                   WHERE c2.status IN ('queued','running')
                                   GROUP BY c2.kind, c2.kb
                                   HAVING MIN(c2.created_at || c2.id)));  -- survivor: oldest, ties by id

-- At most one live job per (kind, kb): makes submit idempotent without a
-- read-then-write race.
CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_live
    ON compile_jobs(kind, kb) WHERE status IN ('queued', 'running');
```

`ensure_jobs_schema(&self) -> Result<(), StorageError>` reads `PRAGMA table_info(compile_jobs)`,
issues only the `ALTER`s whose column is absent, then the collapse, then the three
`CREATE … IF NOT EXISTS`, all inside one transaction. It runs **at store-open time**
(`AppContext::new`/`with_db`, and `serve::run`), *not* on `Core`'s first write: `Core::new` only
records a path and the writer thread is lazy (§2.3, pinned by `writer_thread_is_lazy`), so a
first-write trigger would leave `engram jobs list` — a read-only command reading the new columns
through `ctx.store` — selecting `kind`/`params_json` from a table that was never ALTERed. It is
**not** a schema
version bump: the changes are additive-with-defaults and downgrade-safe, so `CURRENT_VERSION`
(`sqlite.rs:793`) stays whatever storage-journal sets for v10. If storage-journal prefers to fold
the same statements into `migrate()`'s v10 step, the function is idempotent and both may run —
that choice is theirs (§9.1).

**Status vocabulary.** Existing values `running`, `completed`, `failed`, `abandoned` keep their
exact meanings. Added: `queued` (submitted, unclaimed), `paused` (stopped at a checkpoint with a
resumable cursor — budget exhausted, cancelled, or daemon shutdown), `cancelled` (terminal, not
resumable). **`completed` continues to mean "the derived layer was fully rebuilt"**, so a
budget-stopped or cancelled compile must never be written as `completed` — doing so would tell
`MEMORY_CONSTRAINS_DERIVED` that a deleted memory had been compiled out when it had not. This is
the single most dangerous mistake available in this lane; §11 pins it with a test.

`create_compile_job`'s existing "abandon the previous running job for this KB" behaviour
(`sqlite.rs:2621-2632`) is **preserved for the legacy path only** and is not used by
`submit_job`; the partial unique index replaces it with a refusal, because silently abandoning
someone else's running job is how two compiles of one KB both report success.

**The legacy path must not trip the new index (review finding 2).** `create_compile_job` abandons
only `status='running'`, so a `queued` row left by `jobs submit --detach` makes the very next
synchronous `engram compile` of that KB fail on `idx_jobs_live` with a raw
`UNIQUE constraint failed` — inside the same wave-4 binary, not only against an old one. The lane
therefore also lands, as a §10.1 storage patch:

- `create_compile_job`'s `UPDATE` widens to `WHERE kb = ?2 AND kind = 'compile' AND status IN
  ('running', 'queued')`, with the queued case messaged as "superseded by a synchronous compile";
- both statements move inside one `with_tx`, so the abandon/insert pair is atomic and two
  concurrent legacy compiles can no longer leave two live rows;
- the `INSERT` maps a `SQLITE_CONSTRAINT_UNIQUE` on `idx_jobs_live` to `StorageError::Busy`
  ("another compile of this KB is already live"), never a raw SQL error.

An older 0.3.x binary run against a wave-4 store keeps its narrower abandon and can still hit the
index; that surfaces as a failed `engram compile` with a constraint message, not corruption, and
is recorded in §12.1 as accepted downgrade risk.

### 4.2 Storage API — new file, no edits to `sqlite.rs`

`crates/engram-storage/src/jobs.rs` (NEW). **Correction (review finding 1):** a *sibling* module
cannot reach `SqliteStore`'s private `conn` (`sqlite.rs:25-26`) — Rust field privacy extends to the
defining module and its descendants only. Two shapes work, in this order of preference:

1. `conn` becomes `pub(crate)` — a one-line visibility patch requested in §10.1. `spec-storage-journal.md`
   already plans exactly this change for its own modules, so the patch is very likely free;
2. failing that, the file lands as `crates/engram-storage/src/sqlite/jobs.rs`, a *child* module of
   `sqlite`, which does see the private field but does mean `sqlite.rs` gains one `mod jobs;` line.

Either way, `lib.rs` gains `pub mod jobs;` (or a re-export) and a `pub use`, §10.1. No other line of
`sqlite.rs` changes.

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JobRow {
    pub id: Uuid,
    pub kind: String,
    pub kb: String,
    pub status: String,
    pub mode: String,                       // legacy column, kept: 'evidence' for compile
    pub params: serde_json::Value,
    pub cursor: serde_json::Value,
    pub progress_done: i64,
    pub progress_total: Option<i64>,
    pub stats: serde_json::Value,           // parsed from the legacy metadata_json
    pub message: Option<String>,
    pub actor: String,
    pub attempts: i64,
    pub cancel_requested: bool,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub struct NewJob<'a> {
    pub kind: &'a str, pub kb: &'a str, pub params: serde_json::Value, pub actor: &'a str,
}

pub enum SubmitOutcome { Submitted(Uuid), AlreadyLive(JobRow) }

impl SqliteStore {
    pub fn ensure_jobs_schema(&self) -> Result<(), StorageError>;

    /// INSERT … ; a unique-index violation on `idx_jobs_live` is translated to
    /// `AlreadyLive`, never surfaced as a SQL error. Idempotent submit with no
    /// read-then-write window.
    pub fn submit_job(&self, new: &NewJob<'_>) -> Result<SubmitOutcome, StorageError>;

    /// `UPDATE … SET status='running', lease_owner=?, lease_expires_at=?,
    ///  attempts=attempts+1, started_at=COALESCE(started_at,?)
    ///  WHERE id=? AND (status='queued' OR (status='running' AND lease_expires_at < ?))`
    /// Returns false when someone else holds a live lease. Exactly-one-row claim.
    pub fn claim_job(&self, id: Uuid, owner: &str, lease: Duration) -> Result<bool, StorageError>;

    /// Oldest queued job, or a running job whose lease expired (crash recovery),
    /// restricted to `kinds`. Ordered by created_at.
    pub fn next_runnable_job(&self, kinds: &[&str]) -> Result<Option<JobRow>, StorageError>;

    /// Extends the lease and reports whether cancellation was requested, in one
    /// statement: `UPDATE … WHERE id=? AND lease_owner=?` then read
    /// `cancel_requested`. Returns Err(NotFound) when the lease was stolen — a
    /// worker that lost its lease must stop rather than double-write.
    pub fn heartbeat_job(&self, id: Uuid, owner: &str, lease: Duration)
        -> Result<JobHeartbeat, StorageError>;                 // { cancel_requested: bool }

    /// Persist a resume point. Guarded on `lease_owner`, so a zombie worker
    /// cannot rewind a job another process now owns.
    pub fn checkpoint_job(&self, id: Uuid, owner: &str, cursor: &serde_json::Value,
                          done: i64, total: Option<i64>) -> Result<bool, StorageError>;

    /// Terminal (or paused) transition. Guarded on `status='running' AND lease_owner=?`.
    pub fn finish_job(&self, id: Uuid, owner: &str, status: &str, message: Option<&str>,
                      stats: &serde_json::Value) -> Result<bool, StorageError>;

    pub fn request_cancel(&self, id: Uuid) -> Result<bool, StorageError>;

    /// `paused`/`failed`/`cancelled` → `queued`, cursor preserved, lease cleared.
    /// Refuses (`Ok(false)`) for `running` and `completed`.
    pub fn requeue_job(&self, id: Uuid) -> Result<bool, StorageError>;

    pub fn get_job(&self, id: Uuid) -> Result<Option<JobRow>, StorageError>;

    /// Returns (rows, total_matching) so the envelope can carry `returned`/`total`
    /// (IN-23) instead of the count-meaning-page defect the brief names.
    pub fn list_jobs(&self, f: &JobFilter) -> Result<(Vec<JobRow>, i64), StorageError>;

    /// Sleep-time pressure: memories in `kb` created after the newest
    /// `status='completed' AND kind='compile'` job for that kb, plus the oldest
    /// such memory's timestamp. Both NULL-safe on a KB that never compiled.
    /// The `kind` filter is load-bearing: without it a completed `backfill`
    /// (or, in wave 5, `reflect`) job silently resets compile pressure to zero.
    pub fn compile_pressure(&self, kb: &str) -> Result<CompilePressure, StorageError>;
    // pub struct CompilePressure { pub pending: i64, pub oldest_pending_at: Option<DateTime<Utc>>,
    //                              pub last_compile_at: Option<DateTime<Utc>> }

    /// Crash recovery at daemon start: `running` rows with an expired lease and
    /// no cursor → `failed` with a stated reason; with a cursor → `paused`.
    pub fn reconcile_orphan_jobs(&self, grace: Duration) -> Result<usize, StorageError>;
}
```

`CompileJobRecord`, `create_compile_job`, `finish_compile_job`, `list_compile_jobs` and
`get_compile_job` are **left exactly as they are**: the legacy CLI compile path and the legacy
`/v1/jobs` shape keep working while `jobs submit` uses the new API.

### 4.3 Runners

```rust
// engine.rs
pub type JobFuture<'a> = Pin<Box<dyn Future<Output = Result<JobOutcome, CliError>> + Send + 'a>>;

pub trait JobRunner: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Rejects a malformed submission at submit time, not three minutes later
    /// inside a worker. Returns the normalised params that get persisted.
    fn validate(&self, params: serde_json::Value) -> Result<serde_json::Value, CliError>;
    fn run<'a>(&'a self, jc: &'a JobCtx, params: &'a serde_json::Value,
               cursor: &'a serde_json::Value) -> JobFuture<'a>;
}

pub enum JobOutcome {
    /// Ran to the end. `completed` — and for compile, it means the derived layer
    /// was fully rebuilt (§4.1).
    Completed { stats: serde_json::Value },
    /// Stopped at a checkpoint and can continue. NEVER reported as completed.
    Paused { cursor: serde_json::Value, stats: serde_json::Value, reason: PauseReason },
}
pub enum PauseReason { Cancelled, BudgetExhausted, Shutdown, Deadline }

pub struct JobCtx {
    pub core: Core,
    pub id: Uuid,
    pub kb: String,
    pub owner: String,           // lease owner: "<pid>@<hostname>:<uuid>"
    pub budget: JobBudget,
    ctx: AppContext,             // format=Json, quiet=true — runners reuse existing command code
}
```

**`JobCtx.ctx`'s connection identity (normative).** `SqliteStore` is not `Clone` (it owns a
`Connection`), so "the same store handle" in §7.2 is not implementable as written. The rule:
`AppContext::for_job` builds a context over a **fresh reader connection** to the same db path
(`AppContext::with_db`), used only for the reads that existing command code performs. Every
mutation a runner makes goes through `jc.core.write(...)` — the runner's `ctx.store` is
read-only by convention, and a runner that writes through it is a review reject. This keeps the
one-write-connection-per-process invariant (§2.5.3) intact while letting runners reuse command
code unchanged.

```rust

impl JobCtx {
    /// Persists the cursor AND asks whether to stop, in one round trip. Runners
    /// call this at every natural boundary; a runner that never calls it is
    /// simply not resumable.
    pub async fn checkpoint(&self, cursor: serde_json::Value, done: i64, total: Option<i64>)
        -> Result<Continue, CliError>;                 // Continue::Yes | Continue::Stop(PauseReason)
    pub fn app(&self) -> &AppContext;
}

pub struct JobRegistry { runners: HashMap<&'static str, Arc<dyn JobRunner>> }
impl JobRegistry {
    pub fn register(&mut self, r: Arc<dyn JobRunner>);
    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobRunner>>;
    pub fn kinds(&self) -> Vec<&'static str>;
}
```

Registered in wave 4: `CompileRunner` (`commands/compile.rs`), `BackfillRunner`
(`engine::runners::backfill`). `reflect` is deliberately unregistered (§1.2.5); reasoning-layer
registers it in wave 5 with a one-line `registry.register(Arc::new(ReflectRunner))`.

`JobBudget`:

```rust
pub struct JobBudget {
    pub max_usd: Option<f64>,        // params.budget_usd, else the store's budget for kb:<kb>
    pub deadline: Option<Instant>,   // params.deadline_s, default: none for CLI, 6 h for daemon
    pub max_llm_calls: Option<u64>,  // params.max_llm_chunks, mirrors CompileOptions
    spent_usd: AtomicU64,            // f64 bits; fed from record_usage_event costs
}
```
Checked at each checkpoint and before each LLM batch. Exhaustion ⇒ `JobOutcome::Paused
{ reason: BudgetExhausted }` with a `stats.stopped_reason` string that names the cap that fired
and the exact `engram jobs resume <id>` command to continue.

### 4.4 Compile as a job

`commands/compile.rs` keeps `run()` (CLI, synchronous — unchanged default behaviour) and
`compile_kb_with_options()` (`compile.rs:~200`, called from `serve.rs:185`). The runner wraps the
latter:

```rust
// commands/compile.rs
pub struct CompileRunner;

#[derive(Deserialize, Serialize, Default)]
pub struct CompileParams {          // superset of CompileOptions, serde-stable
    #[serde(default)] pub llm: bool,
    #[serde(default)] pub extraction_model: Option<String>,
    #[serde(default)] pub synthesis_model: Option<String>,
    #[serde(default)] pub max_llm_chunks: Option<usize>,
    #[serde(default)] pub budget_usd: Option<f64>,
    #[serde(default)] pub trigger: Option<String>,       // "manual" | "sleep_time" | "api"
}

#[derive(Deserialize, Serialize, Default)]
pub struct CompileCursor {
    #[serde(default)] pub phase: CompilePhase,   // Deterministic | LlmExtract | Synthesis | Swap
    #[serde(default)] pub next_chunk: usize,
    #[serde(default)] pub chunks_total: usize,
}
```

`compile_kb_with_options` gains an optional `&JobCtx` (`Option<&JobCtx>`; `None` for the CLI
path) and calls `jc.checkpoint(...)` after each LLM chunk and at each phase boundary. The
build-then-swap transaction is untouched — the swap is the last phase and either happens whole or
not at all, so a paused compile leaves the previous derived layer intact and `completed` keeps
its meaning.

### 4.5 Resume, and the honest limit on it

Resume replays from `cursor.phase` + `cursor.next_chunk`. For `llm: false` compiles this is free
and exact. For `llm: true` it re-runs the extractor on chunks already extracted **unless**
storage-journal's content-addressed derivation ledger (move 3, keyed `(content_hash,
extractor_id@version)`) is present, in which case those calls are memoized and cost nothing.

Rule, because silently re-spending a user's money is not acceptable:

> `jobs resume` on a job with `params.llm == true` **refuses** with
> `bad_input: resuming this compile would re-run LLM extraction for N chunks that were already
> extracted (no derivation cache available); pass --force to accept the cost` unless the
> derivation ledger reports a hit rate ≥ 0.9 for the job's KB, or `--force` / `"force": true`
> is passed.

Detection is one call, `store.derivation_cache_available()`, requested from storage-journal in
§10.1 with a `false`-returning default so this lane compiles before move 3 lands.

### 4.6 Where jobs actually run

| Situation | Behaviour |
|---|---|
| Daemon running (descriptor live, §3.7) | `submit_job` inserts `queued`; the daemon's scheduler claims and runs it. The CLI returns the handle immediately. |
| No daemon, CLI foreground (default) | `submit_job` inserts `queued`, then the same process claims it and runs it inline with a live progress line on stderr. Ctrl-C ⇒ the runner's cancel token fires, the current checkpoint is persisted, the row goes `paused`, and the CLI prints the resume command. Exit code 1. |
| No daemon, `--detach` | The row stays `queued` and the CLI prints, on stderr and in `data.warning`, that **nothing will run it until `engram serve` starts**. Never silently. |

Scheduler (daemon): a single tokio task, tick 1 s, `next_runnable_job(&["compile","backfill"])`,
`claim_job(id, owner, 60s)`, spawn the runner with a 20 s heartbeat, concurrency
`ENGRAM_JOBS_CONCURRENCY` (default 1 — compile is CPU- and API-heavy and two concurrent compiles
of different KBs contend for the same writer anyway). On `serve` shutdown (SIGINT/SIGTERM): stop
claiming, signal `PauseReason::Shutdown`, wait up to 20 s for checkpoints, then
`Core::shutdown(5s)`.

`reconcile_orphan_jobs(90s)` runs once at daemon start and once per hour, so a `kill -9` leaves
`paused`/`failed` rows with a stated reason instead of the permanent `running` rows the current
code papers over by abandoning them at the next compile (`sqlite.rs:2621-2632`).

### 4.6a Journal obligations (normative)

Interfaces §2 defines `job.submitted` / `job.completed` / `job.failed`, and
`spec-storage-journal.md` §2.3 assigns `compile.ran` to **this lane in wave 4**. Neither was
specified above; both are obligations on the write path, not garnish (IN-17), and each is appended
in the SAME transaction as the projection mutation it describes:

| Event | Appended by | In the same transaction as |
|---|---|---|
| `job.submitted` (job_id, kind, params) | `Core`'s submit path | the `submit_job` INSERT. Not appended for `SubmitOutcome::AlreadyLive` — nothing was written. |
| `job.completed` (job_id, kind, stats) | the scheduler/inline runner | `finish_job(status='completed')` |
| `job.failed` (job_id, kind, error) | ditto | `finish_job(status='failed')`. `paused` and `cancelled` are **not** failures and append neither; they are visible in the row and in `job.completed`'s absence. |
| `compile.ran` (kb, job_id?, stats) | `apply_compile_plan` (§7.5) | the build-then-swap write, for BOTH the job path and the synchronous CLI path — a compile that ran is a compile that ran. |

Because `submit_job`, `finish_job` and `apply_compile_plan` all execute inside a `Core::write`
closure, this is free: the append uses the same `WriteCtx.store` and rolls back with everything
else. A runner that ends `paused` mid-flight appends nothing, which is correct — the derived layer
was not swapped.

### 4.7 HTTP surface

| Method + path | Body / query | Response |
|---|---|---|
| `POST /v1/jobs` | `{kind, kb, params?, detach?}` | `202` `{data:{job}}`; if a live job for `(kind, kb)` exists → `200` `{data:{job, already_live:true}}` (idempotent, not an error) |
| `GET /v1/jobs` | `?kind=&kb=&status=&limit=&cursor=` | `{data:{jobs:[…], returned, total, cursor?}}` — **and legacy `count`** = `returned` (IN-23) |
| `GET /v1/jobs/{id}` | `?wait=terminal&timeout_ms=` | `{data:{job}}`; unknown id → `404 not_found` (today: `null` inside a `200`, `serve.rs:281-288`) |
| `GET /v1/jobs/{id}/events` | — | `text/event-stream`, §4.8 |
| `POST /v1/jobs/{id}/cancel` | — | `{data:{job}}`; already terminal → `400 bad_input` naming the current status. **Not** `conflict_already_resolved`: R11 defines that code for the facts-resolve claim race, and an agent branching on `error.code` must not have adjudication and job lifecycle collapsed into one value. |
| `POST /v1/jobs/{id}/resume` | `{force?}` | `{data:{job}}`; not resumable → `400 bad_input` naming the status |
| `POST /v1/compile` | unchanged, **plus** `"async": true` | default stays synchronous and returns the exact current shape `{data:{job_id, compile: stats}}` (`serve.rs:214-216`); `async:true` → `202 {data:{job}}` |

Backward compatibility of `GET /v1/jobs`: the array element gains fields, loses none, and the
legacy `CompileJobRecord` fields (`id, kb, status, mode, created_at, updated_at, message,
metadata`) keep their names and types. `metadata` continues to carry what `stats` carries.

### 4.8 Subscribe (SSE)

`GET /v1/jobs/{id}/events` → `200`, `content-type: text/event-stream`, `cache-control: no-cache`,
`connection: close`, no `Content-Length` (the one response in the daemon that is streamed). The
handler polls `get_job` every 500 ms (`ENGRAM_JOBS_POLL_MS`) and writes a frame whenever
`updated_at`, `status` or `progress_done` changed:

```
event: progress
data: {"job_id":"…","status":"running","phase":"llm_extract","done":12,"total":40,"updated_at":"…"}

: keep-alive                       <- every 15 s, so proxies and idle timeouts do not kill it

event: completed
data: {"job_id":"…","status":"completed","stats":{…}}
```

Streams count against `Limits::max_subscribers` (16) and are closed after
`Limits::subscriber_max_age` (1 h) with a final `event: timeout` frame naming the poll command to
use instead; over the cap the request is `503` with `Retry-After: 1`, never a silent hang. These
are normative here, not merely a mitigation noted in §12.1.

Terminal statuses (`completed`, `failed`, `cancelled`, `paused`, `abandoned`) emit a final frame
named after the status and close. Polling rather than a broadcast channel is deliberate: the row
is the single source of truth, so a subscriber attached to a *different process* than the worker
(CLI subscribing to a daemon-run job) sees identical output, and there is no in-memory state to
lose on restart. Unknown id → `404` before the stream starts.

### 4.9 CLI surface

`commands/jobs.rs` grows (existing `list`/`show` keep their signatures and output):

```rust
pub fn list(ctx: &AppContext, kb: String, all_kbs: bool, limit: usize) -> Result<(), CliError>;   // unchanged
pub fn show(ctx: &AppContext, id: String) -> Result<(), CliError>;                                 // unchanged
pub async fn submit(ctx: &AppContext, kind: String, kb: String, params: Option<String>,
                    detach: bool, wait: bool) -> Result<(), CliError>;
pub async fn cancel(ctx: &AppContext, id: String) -> Result<(), CliError>;
pub async fn resume(ctx: &AppContext, id: String, force: bool) -> Result<(), CliError>;
pub async fn watch(ctx: &AppContext, id: String) -> Result<(), CliError>;   // NDJSON on stdout, one line per change
```

New clap surface (definitions in `cli.rs`, which contract-codegen owns — §10.2):
`engram jobs submit --kind <compile|backfill> [--kb K] [--params JSON] [--detach] [--wait]`,
`engram jobs cancel <id>`, `engram jobs resume <id> [--force]`, `engram jobs watch <id>`,
`engram jobs list [--kind K] [--status S]`, and `engram compile --async`.

---

## 5. Transactional `facts resolve` claim

### 5.1 The defect, precisely

`commands/facts::resolve` (`facts.rs:451-517`) runs five statements with no transaction:

1. `get_fact_conflict(qid)` (`:461`) — read
2. `row.status != "open"` check (`:464`) — a **read-then-write race window** that stays open for
   the whole rest of the function
3. `get_fact(new)` / `get_fact(old)` (`:471-472`) — read
4. `supersede_fact_with(...)` (`:481` / `:502`) — write, **return value discarded**
5. `resolve_fact_conflict(qid, "caller", note)` (`:517`) — write, **return value discarded**
   (the function returns `bool` and documents "returns false when the row was not open",
   `sqlite.rs:4441-4443`)

Two `engram facts resolve <same-id>` processes therefore both pass the `status == "open"` check,
both call `supersede_fact_with`, and both print `"resolved": true`. Because the two actions can
differ (`supersede-old` on one side, `reject-new` on the other), the pair can end up with **both
facts superseded by each other** — a two-node supersession cycle with no open interval, which is
the brief's "supersede both facts in a cycle with two success reports". `supersede_fact_with`'s
own `AND superseded_by IS NULL` guard (`sqlite.rs:4118`) prevents the *second* write on the same
fact, but not the crossed pair, and its `bool` is discarded at both call sites, so a refused
supersession still reports success.

### 5.2 The fix — `Core::resolve_conflict`

Owned by this lane (it is a `Core` method in `engine.rs`); the call site in `facts.rs` shrinks to
one `await` (patch §10.3).

```rust
// engine.rs
pub struct ResolveRequest {
    pub conflict_id: Uuid,
    pub action: ResolveAction,       // SupersedeOld | KeepBoth | RejectNew
    pub note: Option<String>,
    pub actor: String,               // "cli:<diary>" | "daemon" | "job:<uuid>"
}
pub struct ResolveApplied {
    pub conflict_id: Uuid,
    pub action: &'static str,
    pub kept: Vec<Fact>,
    pub closed: Option<Fact>,
    pub decision_id: Uuid,
    pub event_id: Uuid,
}

impl Core {
    pub async fn resolve_conflict(&self, req: ResolveRequest)
        -> Result<ResolveApplied, CliError>;
}
```

Body — the entire sequence inside **one** `Core::write` closure, in this order:

```
 1. row = store.get_fact_conflict(id)?            ; None            -> NotFound (exit 3)
 2. CLAIM FIRST, before any mutation:
        claimed = store.resolve_fact_conflict(id, "caller", note)?
        if !claimed {
            reread = store.get_fact_conflict(id)?          // for the message only
            return Err(CliError::conflict_already_resolved(id, reread.status, reread.resolved_by))
        }
 3. new_f = store.get_fact(row.new_fact_id)?      ; old_f = store.get_fact(row.old_fact_id)?
        either None -> Err(NotFound) -> the whole transaction rolls back, so the claim is undone
 4. cycle + liveness guard (§5.3)
 4b. AUTHORITY GATE, before any mutation (§5.4):
        auth = CloseAuthority::Caller { actor: req.actor.clone() }
        spec = relations::relation_spec(&canonical_relation(&closing.relation))
        match interval::may_close(&spec, &auth) {
            CloseVerdict::Refuse { reason } => return Err(CliError::bad_input(reason)),  // rolls back the claim
            CloseVerdict::Allow => {}
        }
        (KeepBoth closes nothing and skips this gate.)
 5. apply:
        SupersedeOld -> ok = store.supersede_fact_with(old.id, new.id, "caller", new.valid_from)?
        RejectNew    -> ok = store.supersede_fact_with(new.id, old.id, "caller", None)?
        KeepBoth     -> ok = true            (no fact mutation; the DECISION is the artefact)
        if !ok { return Err(CliError::conflict_already_resolved(...)) }   // rolls back the claim
 6. WHEN A FACT WAS CLOSED (SupersedeOld | RejectNew), append `fact.interval_closed` FIRST — R2
    makes `closed_by`/`close_reason`/`close_authority` and the `superseded_by` compat write
    rebuildable from this one event, and interfaces §2 invariant 3 (`admin rebuild` reproduces
    byte-identical projections) fails without it:
        close_event = store.append_event(&EventDraft { kind: FactIntervalClosed,
            subject_id: Some(closed.id), kb, diary, actor,
            payload: { fact_id, valid_until, closed_by: Some(survivor.id), reason: "superseded",
                       authority: "caller", sets_superseded_by: true } })?
 7. decision event + row, in that order — `DecisionRow.event_id` is frozen (interfaces §3.4), so
    the event id must exist before the row is written:
        verdict = match action { SupersedeOld => "close_old", RejectNew => "reject_new",
                                 KeepBoth => "keep_both" }          // R4's vocabulary, not the CLI's
        event_id = store.append_event(&EventDraft { kind: DecisionRecorded, subject_id: Some(...),
                                                    kb, diary, actor, payload })?.1
        decision_id = store.record_decision(/* DecisionRow fields minus id */ kb, diary,
                                            decision_key, subject_norm, relation, pair_hash,
                                            verdict, authority="caller", actor, note,
                                            Some(conflict_id), event_id, key_version)?
 8. Ok(ResolveApplied { … })
```

The CLI's action vocabulary (`supersede-old | keep-both | reject-new`) and the journal's verdict
vocabulary (`close_old | reject_new | keep_both | void`, R4) are **different alphabets**; the map
above is the only place they meet, and writing the CLI spelling into `decision.recorded` would
break `lookup_decision`/replay silently. If `record_decision`'s final signature (interfaces §3.4
leaves it underspecified) does not take `event_id`, the lane takes whatever storage-journal lands
and appends the event inside it instead — but the ordering constraint survives either way.

Properties this buys, each pinned by a test in §11.4:

- **Exactly one winner.** Step 2 is a conditional `UPDATE … WHERE id=? AND status='open'`
  (`sqlite.rs:4449-4453`) executed inside `BEGIN IMMEDIATE`, and its boolean is now *load-bearing*.
  The loser never reaches step 5.
- **`ConflictAlreadyResolved`** (`ErrorCode::ConflictAlreadyResolved`, exit 3, HTTP 409) with a
  message naming the winning actor and time — not `BadInput`, which is what `facts.rs:465-468`
  returns today for the already-resolved case and which an agent cannot distinguish from a typo.
- **All-or-nothing.** Any failure after the claim rolls the claim back, so a conflict is never
  left "resolved" with no supersession and no decision — the state that made `keep-both` a
  write-only decision the store forgets.
- **`keep-both` finally writes something** (steps 6–7), which is CD-3's precondition for decision
  replay; without it the caller's decision is re-asked on every re-derivation.

### 5.3 Cycle guard

Before applying, walk `superseded_by` from the intended *survivor* for at most `MAX_CHAIN = 64`
hops:

```rust
fn would_cycle(store: &SqliteStore, closing: Uuid, survivor: Uuid) -> Result<bool, StorageError>;
// true when `closing == survivor`, or when following survivor.superseded_by
// reaches `closing`, or when the chain exceeds MAX_CHAIN (a pre-existing cycle
// in the data is itself a refusal, not an infinite loop).
```

`true` ⇒ `CliError::BadInput("resolving conflict <id> this way would create a supersession cycle:
fact <survivor> is already superseded by <…> which leads back to <closing>")` and the transaction
rolls back. A pre-existing cycle (possible in stores written by 0.3.x) is reported with the
`engram doctor --integrity` hint rather than silently walked forever.

This also covers the `new_fact_id == old_fact_id` degenerate row, which today would supersede a
fact by itself.

### 5.4 What this lane does not decide

The *semantics* of the verdict — `SemanticKey` construction, `pair_hash` (R9), `key_version`,
whether a safety slot may be closed at all (`may_close`) — belong to fact-semantics and
storage-journal. `Core::resolve_conflict` calls their functions and refuses to reinterpret them.
Where `may_close` returns `CloseVerdict::Refuse{reason}`, the resolve returns
`CliError::BadInput` carrying that exact reason string, unmodified.

---

## 6. Sleep-time compile trigger

### 6.1 Rule

A single daemon task (`spawn_sleep_watch`, tick 60 s) submits a compile job for a KB when **all**
of the following hold:

| Gate | Default | Env |
|---|---|---|
| pending writes since the last `completed` compile ≥ `min_writes` … | 25 | `ENGRAM_SLEEP_COMPILE_MIN_WRITES` |
| … **or** the oldest pending write is older than `max_age` | 6 h | `ENGRAM_SLEEP_COMPILE_MAX_AGE_S` |
| no write through `Core` for `idle` | 300 s | `ENGRAM_SLEEP_COMPILE_IDLE_S` |
| auto-compiles for this KB in the last 24 h < `max_per_day` | 4 | `ENGRAM_SLEEP_COMPILE_MAX_PER_DAY` |
| no live (`queued`/`running`) job for `(compile, kb)` | — | (enforced by `idx_jobs_live`) |
| the feature is on | **on in the daemon, off in the CLI** | `ENGRAM_SLEEP_COMPILE=0` to disable |
| LLM extraction | **off** | `ENGRAM_SLEEP_COMPILE_LLM=1` to enable |
| budget headroom, when LLM is on: remaining daily budget for `kb:<kb>` > `min_headroom_usd` | 0.10 | `ENGRAM_SLEEP_COMPILE_MIN_HEADROOM_USD` |

`llm: false` by default is the whole safety story: an unattended background trigger that can
spend money is a bug report waiting to happen, so switching it on is an explicit, per-install act.
With `llm: true` the submitted params also carry `budget_usd = min(remaining_daily, cap)` so the
runner pauses instead of draining the budget.

Pressure comes from `store.compile_pressure(kb)` (§4.2), which is derived from existing rows —
no counter table, no state to drift.

The idle signal must be **store-derived, not in-process**. `CoreMetrics.last_write_at` only sees
writes made by *this* process's `Core`, and §1.2.1 keeps every CLI mutation in a different process
— so a daemon watching its own metrics considers the store idle while an agent is hammering
`engram remember`, and compiles into the middle of an ingest. The gate is therefore

```
last_write_at = max(CoreMetrics.last_write_at,
                    store.newest_write_ts(kb))   // MAX(events.ts) for the kb, or MAX(memories.created_at)
                                                 // on a store where the journal is not yet populated
```

read once per tick on a reader connection. The in-process metric stays as a cheap upper bound; it
is never the only input.

### 6.2 Observability

Auto-submitted jobs carry `params.trigger = "sleep_time"` and `actor = "daemon:sleep"`, so
`engram jobs list --kind compile` shows plainly which compiles the user asked for and which the
daemon decided on. `/health.sleep_compile` reports `enabled`, `llm`, per-KB `pending_writes` and
`next_eligible_at`. Every submission logs at `info` with the gate values that fired.

### 6.3 Non-goal

No adaptive/learned scheduling, no cross-KB prioritisation, no "quiet hours" calendar. The rule
above is deliberately a fixed function of observable state so that "why did it compile then?" has
an answer that fits in a log line.

---

## 7. File-by-file change plan

### 7.1 `crates/engram-cli/src/engine.rs` — NEW (~700 lines)

`Core`, `CoreInner`, `WriterHandle`, `WriteCtx`, `ReaderPool`, `CoreMetrics`, `JobRegistry`,
`JobRunner`, `JobCtx`, `JobBudget`, `Scheduler`, `sleep_watch`, `Core::resolve_conflict`,
`would_cycle`, `runners::backfill`. Sub-modules kept in one file until it passes ~900 lines,
then split `engine/{mod,jobs,runners,sleep}.rs`.

### 7.2 `crates/engram-cli/src/context.rs` (22 lines today)

- `AppContext` gains `pub core: Core` (§2.6).
- `AppContext::new` resolves `paths::db_path()` **once** and passes the same path to
  `SqliteStore::open` and `Core::new`, so the process cannot straddle two databases.
- New `AppContext::with_db(format, quiet, db)` for the daemon and for tests.
- New `AppContext::for_job(&self, job_id) -> AppContext` — same store handle, actor
  `job:<uuid>`, `format = Json`, `quiet = true`.

### 7.3 `crates/engram-cli/src/lib.rs` (399 lines today)

- `pub mod engine;` added to the module list (`lib.rs:3-11`).
- `dispatch` (`:19`): the `--store` override is applied as the **first statement**, before
  `output::mark_command_start()` and before the `Command::AgentInfo` early return at `:25-27`,
  so `engram --store /tmp/x agent-info` reports `/tmp/x`'s paths (§8.2).
- After `AppContext::new` (`:28`), `ctx.core.set_actor(actor_for(&cli.command))` — the diary is
  known only after the match arms, so the actor is `cli:<diary>` where the arm carries one and
  `cli` otherwise.
- New dispatch arms for `JobsCommand::{Submit, Cancel, Resume, Watch}` alongside the existing
  `List`/`Show` (`:173-178`), and `Command::Compile` gains the `r#async` field (`:133-151`).
- `Command::Serve` (`:270`) unchanged in shape.
- Reserved: the `Command::State` arm (CD-4) and the `Command::Admin` arm (interfaces §2
  invariant 3) land here as one-line delegations when reasoning-layer and storage-journal request
  them; this lane pre-agrees to accept both patches.

#### 7.3.1 `Core` adoption schedule (the "other lanes consume" half of move 4)

`Core::write` is available to every lane the moment wave 4 lands. It is **not** retrofitted into
their files by this lane. Sequence, to be enforced at the wave-5 integration gate:

| Call site | Lane | Wave it moves |
|---|---|---|
| `serve.rs` mutating endpoints (kbs, budget, document delete, compile submit) | daemon-jobs | 4 (this spec) |
| `commands/compile.rs` build-then-swap (§7.5) | daemon-jobs | 4 |
| `commands/facts.rs::resolve` → `Core::resolve_conflict` | read-surfaces (call site), daemon-jobs (body) | 4, via patch §10.3 |
| `commands/facts.rs::triage`, `backfill` | read-surfaces | 5 |
| `remember`, `edit`, `forget`, `memory`, `import` | write-surfaces | 5 |
| `fact_write.rs` applier | fact-semantics | 5 |
| everything else (`ingest`, `reindex`, `kb`, `chunks`, `repair`) | unowned/frozen | 6 or later |

Until a site moves, it writes through `ctx.store` on the CLI's own connection. That is safe (WAL
+ `busy_timeout` 5 s, `sqlite.rs:806`) and observable: `/health.writer.writes_total` counts only
what went through `Core`, so the gap is measurable rather than assumed.

### 7.4 `crates/engram-cli/src/commands/serve.rs` (843 lines today)

| Today | Change |
|---|---|
| `run` `:21-40` | build `Server`, warm readers, spawn scheduler + sleep watch, write the descriptor, per-connection `tokio::spawn`, non-fatal `accept` errors |
| `handle_stream` `:42-121` | **deleted**, replaced by `serve_connection` + `read_request` + `Request` struct (§3.3) |
| `route` `:123-327` | takes `&Server` (hence `&Core`); `SqliteStore::open` at `:129` **deleted**; every arm becomes `core.read(...)` or `core.write(...)`; every `json!` envelope becomes a `respond::` call (§3.8) |
| `/v1/compile` `:148-217` | submits a job; synchronous by default (identical response shape), `async: true` returns `202` |
| `/v1/jobs*` `:266-288` | rewritten on `JobRow` + `respond::listing`; `GET /v1/jobs/{id}` 404s on unknown |
| new arms | `POST /v1/jobs`, `GET /v1/jobs/{id}/events`, `POST /v1/jobs/{id}/cancel`, `POST /v1/jobs/{id}/resume` |
| `recall_api` `:469-619` | `conflicts` array (§3.8); store comes from the pool |
| `research_api` `:342-467` | store from the pool; envelopes via `respond::` |
| `write_json` `:678-692` | replaced by `write_response(stream, status, headers, body)` with keep-alive support |
| `split_request` `:694-700`, `authorized` `:721-726` | replaced by the real parser + constant-time token compare |
| `query_param` `:706-711` | replaced by proper percent-decoding (`%20`-only today, so `?kb=a%2Fb` is silently wrong) |
| `daemon_diary` `:334-340`, `parse_layer` `:738-748`, `parse_bound_param` `:758-770` | **unchanged** — the diary and bound semantics they pin are correct and tested by `wave05_daemon_parity.rs` |

### 7.5 `crates/engram-cli/src/commands/compile.rs` (1408 lines today)

The defect to fix here is structural and is exactly move 4's "write locks held across network
calls": `compile_kb_with_options:186` calls `ctx.store.begin_immediate()` and then `await`s
`compile_within_tx` (`:205`), which `await`s `run_llm_compiler` (`:547`) and
`insert_llm_evidence` (`:679`) — so an `IMMEDIATE` write lock is held for the entire duration of
every OpenRouter call, up to the 1700 s the brief cites. Every other writer in the process **and
on the machine** is blocked for that window.

Split into plan and apply:

```rust
/// Everything the compile derived, owned and in memory. No connection, no lock.
pub struct CompilePlan {
    pub kb: String,
    pub spans: Vec<SourceSpanRow>, pub claims: Vec<ClaimRow>, pub entities: Vec<EntityRow>,
    pub relations: Vec<RelationRow>, pub takeaways: Vec<TakeawayRow>, pub wiki: Vec<WikiRow>,
    pub facts: Vec<PlannedFactWrite>,          // from fact-semantics' planner (§10.4)
    pub usage: Vec<PendingUsage>,
    pub stats: CompileStats,
}

/// Phase 1 — async, NO transaction. Reads chunks, runs the deterministic
/// derivation and all LLM extraction/synthesis, checkpoints through `jc`.
async fn plan_compile(ctx: &AppContext, kb: &str, options: &CompileOptions,
                      jc: Option<&JobCtx>) -> Result<CompilePlan, CliError>;

/// Phase 2 — synchronous, inside ONE Core::write. clear_derived_for_kb + insert
/// everything. Build-then-swap is preserved and is now genuinely atomic without
/// the lock ever spanning a network call.
fn apply_compile_plan(w: &WriteCtx<'_>, plan: CompilePlan) -> Result<CompileStats, CliError>;

pub async fn compile_kb_with_options(ctx, kb, options) -> Result<CompileStats, CliError> {
    let plan = plan_compile(ctx, kb, &options, None).await?;
    let usage = plan.usage.clone();
    let out = ctx.core.write("compile.apply", move |w| apply_compile_plan(w, plan)).await;
    flush_usage_events(ctx, kb, &usage);        // spent money is recorded even on rollback (`:194-197`)
    out
}
```

`compile_within_tx` (`:205-471`) is refactored in place: its body becomes `plan_compile` plus a
row-emitting `apply_*` half; `flush_usage_events` (`:472`), `resolve_compiler_llms` (`:509`),
`run_llm_compiler` (`:547`), `parse_llm_evidence` (`:989`) and every pure helper below `:953` are
untouched.

`insert_llm_evidence` (`:679-880`) calls `write_fact` (`:878`) with `embedder: None` and
`llm: None` (`:838-846`). Under the split it emits `PlannedFactWrite` values instead, applied
inside the write closure by fact-semantics' synchronous applier (§10.4).

**The `block_on(write_fact(...))` fallback is withdrawn (review finding 6).** `write_fact`
(`fact_write.rs:858+`) mutates through `ctx.store` — the CLI's connection — while the write
closure runs on the writer thread's *own* connection. The facts would commit outside
`apply_compile_plan`'s transaction, so a rolled-back compile would leave its facts behind, and two
connections both wanting `RESERVED` inside one logical write is the 5 s-stall-then-`SQLITE_BUSY`
shape §2.5.2 forbids. It is unsafe, not merely ugly, and a `debug_assert` on `embedder`/`llm`
does not address either problem.

If fact-semantics' applier is not exposed in time, the sanctioned fallback is instead: keep facts
out of the compile write entirely — `apply_compile_plan` writes the derived rows, commits, and the
planned fact writes are then applied by a second, separate `Core::write` (or by the existing
`write_fact` path outside any transaction). That is weaker atomicity, stated plainly, rather than a
silent cross-connection write. Prefer the §10.4 patch.

Also here: `CompileRunner` (§4.4), `CompileParams`, `CompileCursor`, and the `..Default::default()`
one-liner CD-1 needs at `:866-875` when fact-semantics lands `ExtractedFact`'s new fields — this
lane pre-accepts that patch (§10.5) and will simply write it itself if wave 3 lands first.

### 7.6 `crates/engram-cli/src/commands/jobs.rs` (41 lines today)

`list` (`:9-26`) and `show` (`:28-40`) keep their signatures and their exact output; internally
they move to `list_jobs`/`get_job` with the legacy fields preserved and `returned`/`total` added.
New: `submit`, `cancel`, `resume`, `watch` (§4.9). `show` on an unknown id currently returns
`BadInput` (`:31`); it becomes `NotFound` (exit 3 either way, but the code is now honest and
matches interfaces §1).

### 7.7 `crates/engram-cli/src/bin/engramd.rs` (44 lines today)

- `--store <DIR>` argument, applied through the same `apply_store_override` before
  `AppContext::new` (`:30`), so `AppContext::new` is still what runs. It must **not** be wired as
  `AppContext::with_db(…)`: `with_db` overrides only the db path while vectors, config and cache
  keep resolving through `paths::`, which would split the daemon across two stores — the exact
  failure brief constraint 1 exists to prevent. `with_db` stays a test-only constructor for
  in-process contexts over a TempDir.
- SIGINT/SIGTERM handling: `tokio::signal`, graceful scheduler stop, `Core::shutdown(5s)`,
  descriptor file removed.
- `--readers <N>` (default `max(2, cores/2)`, capped 8).

### 7.8 `crates/engram-cli/src/main.rs` (23 lines today)

Unchanged except that `dispatch` now owns the `--store` handling; `main` keeps its exit-code
mapping verbatim (brief constraint 3).

### 7.9 `crates/engram-storage/src/jobs.rs` — NEW (~350 lines, scoped exception §9.1)

`JobRow`, `NewJob`, `SubmitOutcome`, `JobFilter`, `JobHeartbeat`, `CompilePressure`, and the
`impl SqliteStore` block from §4.2. No line of `sqlite.rs` changes.

---

## 8. Migration, compatibility, and store resolution

### 8.1 Schema

- **No schema-version bump from this lane.** `ensure_jobs_schema()` (§4.1) issues only
  `ALTER TABLE … ADD COLUMN … DEFAULT` and `CREATE … IF NOT EXISTS`, is idempotent, and is safe
  to run against a v9 or a v10 store. `CURRENT_VERSION` (`sqlite.rs:793`) is storage-journal's to
  set.
- **Downgrade-safe.** Every added column has a default and every existing `INSERT` into
  `compile_jobs` lists its columns explicitly (`sqlite.rs:2635-2637`), so a 0.3.x binary run
  against a wave-4 store still creates and finishes compile jobs. The partial unique index
  `idx_jobs_live` is the one behaviour change an old binary would meet: `create_compile_job`'s
  abandon-then-insert (`:2621-2646`) clears only `running` rows, so an old binary meeting a
  `queued` row for the same `(compile, kb)` gets a constraint failure on its `INSERT` — a failed
  compile with a SQL message, not corruption. The wave-4 binary avoids it by widening the abandon
  and translating the constraint (§4.1); accepted downgrade risk, §12.1.
- **Backfill of existing rows.** None needed. `kind` defaults to `'compile'`, which is what every
  existing row is; `params_json`/`cursor_json` default to `{}`; `actor` defaults to `'cli'`.
  Rows written before wave 4 are simply not resumable, which is honest — they have no cursor.
- **`MEMORY_CONSTRAINS_DERIVED` (`sqlite.rs:779-790`) is untouched** and keeps its exact meaning
  because `completed` keeps its exact meaning (§4.1). A test pins that a paused/cancelled compile
  does not un-gate a deleted memory.

### 8.2 Wire and CLI compatibility

| Surface | Guarantee |
|---|---|
| `GET /health` | fields added only; `ok` and `schema_version` unchanged |
| `POST /v1/compile` | default behaviour, response shape and `job_id` field unchanged; `async` is opt-in |
| `GET /v1/jobs`, `GET /v1/jobs/{id}` | all legacy `CompileJobRecord` field names/types kept; new fields additive; **one behaviour change**: an unknown job id is `404` instead of `200 {"job": null}` (interfaces §1: unknown → `not_found`) |
| `POST /v1/recall`, `/v1/research` | `conflicts` added to recall only; everything else byte-identical (`wave05_daemon_parity.rs` and `wave05_daemon_envelope.rs` must pass untouched) |
| error envelope | `version`/`status`/`error{code,message,suggestion,exit_code}` unchanged; HTTP status refined per §3.4 |
| `engram jobs list|show` | output unchanged apart from additive fields |
| `engram compile` | unchanged unless `--async` is passed |
| exit codes | unchanged; `jobs show <unknown>` stays exit 3 |

### 8.3 `--store <dir>` (R14, deferred here from wave 2)

Precedence, per interfaces §5: `--store <dir>` > `ENGRAM_STORE` > `ProjectDirs`.

Implementation, deliberately the cheapest correct one: `paths.rs`'s `store_override()` reads
`ENGRAM_STORE` from the environment on every call (`paths.rs:23-27`), so the flag is implemented
by **exporting the flag's value into `ENGRAM_STORE`** as the first statement of `dispatch`:

```rust
pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    apply_store_override(cli.store.as_deref())?;   // FIRST — before mark_command_start,
    output::mark_command_start();                  // before the AgentInfo early return (:25),
    …                                              // before any path is resolved
}

/// Rejects an empty value, rejects a path that exists and is not a directory,
/// makes it absolute WITHOUT touching the filesystem (`std::path::absolute`, not
/// `canonicalize`, which requires the path to exist), and exports it so that
/// child processes — `serve`'s ingest shell-out (`serve.rs:621-676`) and every
/// `assert_cmd` test binary — inherit the same isolation.
///
/// It creates NOTHING. Directory creation stays where it already is, in
/// `paths::ensure()`, which runs on the first real path resolution — so
/// `engram --store X agent-info` still leaves X non-existent (CC-7), which is
/// exactly what §11.6's `agent_info_with_store_opens_nothing` asserts.
fn apply_store_override(dir: Option<&Path>) -> Result<(), CliError>;
```

Consequences, stated because they are load-bearing:

- **Subprocess inheritance is a feature, not a leak.** `POST /v1/ingest` re-enters the CLI as a
  child process; without the export, a `--store`-isolated daemon would ingest into the user's
  real store. This is the single most dangerous path in the lane (brief constraint 1) and has its
  own test (§11.6).
- `std::env::set_var` is called once, at the top of `dispatch`, before this lane spawns any
  thread or task. The tokio runtime's worker threads already exist, so this is the one place in
  the codebase where the setenv/getenv data race is possible in principle; it is confined to a
  single call at process start, before any store path is read. If storage-journal would rather
  own a `paths::set_store_override(PathBuf)` `OnceLock` setter, this lane switches to it in one
  line and keeps the export purely for child processes (§10.1, optional).
- `engramd --store` does the same before `AppContext::new` (`engramd.rs:30`).
- `engram --store X agent-info` reports X's paths and **still opens and creates nothing** (CC-7's
  landed behaviour is preserved: the override is a pure string operation, and `paths::*_peek()`
  creates no directories). This is why `apply_store_override` must not `canonicalize` or
  `create_dir_all` — a discovery command that materialises a store directory is the same class of
  bug as a discovery command that migrates one.

---

## 9. Interface additions and ownership exceptions requested

These are requests to the integration lead; none is a licence to edit another lane's file.

### 9.1 Scoped exception: `crates/engram-storage/src/jobs.rs` (NEW file)

The jobs API is ~350 lines of SQL that belong physically in `engram-storage` (the `conn` field is
private) and semantically to daemon-jobs. Requested: daemon-jobs owns this **new** file. It touches
exactly one existing line of `sqlite.rs` — `conn` becoming `pub(crate)` (§4.2 correction; a child
module under `sqlite/` is the fallback) — so the merge-conflict surface with storage-journal is one
token. The alternative — routing every
signature in §4.2 through storage-journal in wave 2, before the runner that consumes them exists
— costs a wave and produces an API designed by a lane that has no caller for it.

If the lead declines, the fallback is that storage-journal implements §4.2 verbatim in wave 2 and
daemon-jobs consumes it; the signatures above are written to be implementable as-is.

### 9.2 Additions requested for `interfaces.md` v3

§6 of interfaces.md ("Single-writer core and jobs") is one paragraph. It should carry, verbatim
from this spec, so that other lanes can code against it:

1. `Core::write` / `Core::read` / `WriteCtx` signatures (§2.3) and the five invariants (§2.5).
2. The job status vocabulary and the rule that **`completed` means a full derived-layer rebuild**
   (§4.1) — this is a cross-lane semantic (`MEMORY_CONSTRAINS_DERIVED` depends on it).
3. `Core::resolve_conflict`'s claim-then-mutate order and `ConflictAlreadyResolved` (§5.2), which
   read-surfaces and fact-semantics both call into.
4. `respond::conflicts` / `respond::listing` in §4's R12 list, since `/v1/recall` now consumes
   them (§3.8).
5. HTTP status ↔ `ErrorCode` mapping (§3.4), as the daemon half of R11.

---

## 10. cross_lane_patches

One-liners and small, pre-negotiated edits this lane needs in files it does not own. Each is
written so the lead can apply it mechanically.

### 10.1 storage-journal

| File | Patch |
|---|---|
| `crates/engram-storage/src/lib.rs` | add `pub mod jobs;` and `pub use jobs::{JobRow, NewJob, JobFilter, SubmitOutcome, JobHeartbeat, CompilePressure};` |
| `crates/engram-storage/src/sqlite.rs:26` | `conn: Connection` → `pub(crate) conn: Connection`, so the sibling `jobs` module can hold the `impl SqliteStore` block (§4.2). Storage-journal plans the same change; if it declines, the file becomes a child module of `sqlite` instead. |
| `crates/engram-storage/src/sqlite.rs` (near `with_tx`, `:831-850`) | generalise the error type: `pub fn with_tx_res<T, E: From<StorageError>>(&self, f: impl FnOnce(&Self) -> Result<T, E>) -> Result<T, E>`, and `pub fn with_tx<T>(…)` delegating to it. **Not** the existing body verbatim: it must also roll back on unwind (`catch_unwind` around `f`, or a drop-guard that issues `ROLLBACK` unless the closure committed). Today's `with_tx` leaves the transaction open on a panic, and its join-an-open-transaction branch (`:838-840`) then makes every later write on that connection join a transaction that is never committed. No existing caller changes. |
| `crates/engram-storage/src/sqlite.rs` | `pub fn in_transaction(&self) -> bool { !self.conn.is_autocommit() }` — one line; used by `Core::write`'s `debug_assert` against nested-transaction deadlock (§2.5.2) |
| `crates/engram-storage/src/sqlite.rs` | `pub fn derivation_cache_available(&self) -> bool` — returns whether move 3's derivation ledger exists and is populated; a `false`-returning stub is acceptable until move 3 lands (§4.5) |
| `crates/engram-storage/src/sqlite.rs` | `pub fn open_reader(path) -> Result<Self, StorageError>` — `open` without the `migrate()` call, for the daemon's reader pool (§2.3). Refuses (returns `StorageError::Migration`) when the stored `user_version` is not `CURRENT_VERSION`, so it can never be used to skip a migration by accident. |
| `crates/engram-storage/src/sqlite.rs` | `pub fn rollback_if_open(&self)` — issues `ROLLBACK` when `!is_autocommit()`, used by the writer thread after `catch_unwind` (§2.4) |
| *optional* `crates/engram-storage/src/paths.rs` | `pub fn set_store_override(dir: PathBuf)` backed by a `OnceLock` consulted by `store_override()` before the env var, if the lead prefers it over `env::set_var` (§8.3) |

### 10.2 contract-codegen

| File | Patch |
|---|---|
| `crates/engram-cli/src/cli.rs`, `struct Cli` (`:13-24`) | `/// Root every path (db, vectors, config, cache) at DIR. Overrides ENGRAM_STORE.`<br>`#[arg(long, global = true, value_name = "DIR")] pub store: Option<PathBuf>,` |
| `crates/engram-cli/src/cli.rs`, `Command::Compile` (`:234-`) | `/// Submit the compile as a background job and return its handle immediately.`<br>`#[arg(long = "async")] pub r#async: bool,` |
| `crates/engram-cli/src/cli.rs`, `enum JobsCommand` (`:695-`) | add `Submit { kind: String, kb: String, params: Option<String>, detach: bool, wait: bool }`, `Cancel { id: String }`, `Resume { id: String, force: bool }`, `Watch { id: String }`; add `--kind`/`--status` to `List` (exact clap block supplied with the wave-4 branch) |
| `crates/engram-cli/src/respond.rs` | `pub fn conflicts(rows: &[ConflictInput<'_>]) -> Vec<serde_json::Value>` and `pub fn listing(...)` per §3.8, if not already in the wave-2 shape |
| `crates/engram-cli/src/error.rs` | `CliError::conflict_already_resolved(id, status, by)` constructor mapping to `ErrorCode::ConflictAlreadyResolved` (exit 3), plus `CliError::not_found(...)`; both are R11 items this lane consumes |
| `crates/engram-cli/assets/skill.md`, `agent_info.rs` | document `jobs submit/cancel/resume/watch`, `--store`, `--async`, `conflicts` on `/v1/recall`, and the SSE endpoint — generated from the schema, so this is a codegen input, not prose |

### 10.3 read-surfaces

| File | Patch |
|---|---|
| `crates/engram-cli/src/commands/recall.rs:296` | `fn surface_conflicts(ctx: &AppContext, kb: Option<&str>, query: &str)` → `pub fn surface_conflicts_for(store: &SqliteStore, kb: Option<&str>, query: &str)`; the three `ctx.store` uses inside (`:309`, `:318`, `:336`) become `store`; the existing CLI call site passes `&ctx.store` (3 lines total). **Reparameterizing is not enough (review finding 21):** `surface_conflicts` still builds its own `resolve_command` string with a pipe (`recall.rs:345-348`), so lifting it as-is would carry the IN-24 pipe bug straight into `/v1/recall`. The same patch routes each row's resolve payload through `respond::` (R12), which is read-surfaces' wave-3 work anyway; if it has not landed, D6 waits for it rather than emitting a piped command from a second surface. |
| `crates/engram-cli/src/commands/facts.rs:451-517` | `pub fn resolve(...)` becomes `pub async fn resolve(...)` and its body becomes: parse the id, map `action` to `ResolveAction`, `ctx.core.resolve_conflict(req).await?`, then the existing `print_success` block rendering `ResolveApplied`. All five statements of the current non-transactional sequence are deleted. Dispatch arm `lib.rs:331-333` gains `.await` (this lane's file). |
| `crates/engram-cli/src/commands/facts.rs:818` | expose a batch entry point for the backfill runner:<br>`pub struct BackfillBatch { pub processed: usize, pub facts_added: usize, pub queued: usize, pub next_after: Option<Uuid>, pub exhausted: bool }`<br>`pub async fn backfill_batch(ctx: &AppContext, kb: Option<&str>, diary: Option<&str>, after: Option<Uuid>, limit: usize) -> Result<BackfillBatch, CliError>` — the existing `backfill` becomes a loop over it. **Fallback if declined:** the backfill runner shells out to `engram facts backfill --limit N` per batch, exactly as `/v1/ingest` already does (`serve.rs:621-676`); it works, it is uglier, and it costs a process per batch. |

### 10.4 fact-semantics

| File | Patch |
|---|---|
| `crates/engram-cli/src/fact_write.rs` | expose the planner/applier split the lane already plans (spec-fact-semantics §2.4 step 9) as two public items: `pub async fn plan_fact_write(...) -> Result<PlannedFactWrite, CliError>` (may embed/await) and `pub fn apply_fact_plan(w: &WriteCtx<'_>, plan: &PlannedFactWrite, out: &mut FactWriteOutcome) -> Result<(), CliError>` (synchronous, no I/O). `compile.rs` consumes both (§7.5). **Fallback:** compile keeps calling `write_fact` under `Handle::block_on` inside the write closure with `embedder: None` and `llm: None`, guarded by a debug assertion. |
| `crates/engram-cli/src/fact_write.rs` | confirm that `suggested_action` values reaching `respond::conflicts` are exactly R7's `keep-both | supersede-old | requires_context`; the daemon renders whatever it is given and must not be the surface that reintroduces a fourth spelling |

### 10.5 Patches this lane pre-accepts (inbound)

- `compile.rs:866-875` gaining `..Default::default()` when `ExtractedFact` grows `valid_until`
  and `state` (CD-1). If wave 3 lands first, daemon-jobs writes it; if wave 4 lands first,
  fact-semantics may apply it directly.
- `lib.rs` dispatch arms for `Command::State` (reasoning-layer, CD-4) and `Command::Admin`
  (storage-journal, interfaces §2 invariant 3), as one-line delegations.
- `commands/mod.rs` `pub mod` lines are the lead's (R15); this lane adds none (`engine.rs` is a
  crate-root module declared in `lib.rs`, which this lane owns).

---

## 11. Test plan

### 11.0 Isolation — non-negotiable

**Every** test in this lane isolates with `ENGRAM_STORE` pointed at a `tempfile::TempDir`. No
`point_at_home`, no `HOME`/XDG tricks, no exceptions (brief constraint 1; interfaces §5; CD-6).
The shared harness enforces it:

```rust
// crates/engram-cli/tests/daemon_support/mod.rs   (NEW, this lane)
pub struct Store { pub dir: TempDir }
impl Store {
    /// Every spawned binary gets ENGRAM_STORE and nothing else. Also clears
    /// GEMINI_API_KEY / COHERE_API_KEY / OPENROUTER_API_KEY so a developer's
    /// real keys cannot make a test spend money or reach the network.
    pub fn cmd(&self, bin: &str) -> assert_cmd::Command;
    pub fn db(&self) -> PathBuf;                    // <dir>/db.sqlite
    pub fn open(&self) -> SqliteStore;              // direct handle for assertions
    pub fn seed(&self, args: &[&str]);              // ENGRAM_BENCH_FORCE_STUB=1
}
pub struct Daemon { child: Child, pub port: u16, _store: () }
impl Daemon {
    pub fn start(store: &Store) -> Daemon;          // free-port retry loop, waits for /health
    pub fn start_with(store: &Store, env: &[(&str,&str)]) -> Daemon;
    pub fn raw(&self) -> TcpStream;                 // for hand-written HTTP
    pub fn post(&self, path: &str, body: &str) -> (u16, Value);
    pub fn get(&self, path: &str) -> (u16, Value);
}
impl Drop for Daemon { /* kill + wait */ }
```

`Daemon::start` reuses the proven free-port retry loop from
`wave05_daemon_envelope.rs:53-80` (five attempts; a lost port race is not a product failure).

A guard test, `daemon_support_isolation`, asserts that `Store::cmd` sets `ENGRAM_STORE`, that the
real data directory is **not** created during the whole test binary's run (checked via
`paths::db_path_peek()`'s parent existing-before vs existing-after with `ENGRAM_STORE` unset in
the *test process*), and that no test file in this lane contains the string `point_at_home`.

### 11.1 `tests/daemon_http_framing.rs` — the parser

Every case writes raw bytes to a `TcpStream` and reads the raw response.

| Test | Asserts |
|---|---|
| `body_split_across_writes_is_reassembled` | headers written, 50 ms pause, body written in three chunks → `200`, correct result. **This is the defect at `serve.rs:42-46`.** |
| `body_larger_than_64k_is_not_truncated` | a 200 KiB `POST /v1/recall` (long query) → parsed whole |
| `body_over_limit_is_413` | `Content-Length: 9000000` with `ENGRAM_HTTP_MAX_BODY_BYTES` default → `413 body_too_large`, connection closed, **body never read** (the client's write blocks/errors rather than being drained) |
| `short_body_is_incomplete_not_malformed_json` | `Content-Length: 100`, 40 bytes sent, then FIN → `400 incomplete_body`, not `serde error` |
| `chunked_is_501` | `Transfer-Encoding: chunked` → `501 unsupported_transfer_encoding` |
| `duplicate_conflicting_content_length_is_400` | two different `Content-Length` headers → `400 bad_content_length` |
| `header_flood_is_431` | 40 KiB of headers → `431 header_too_large` |
| `invalid_utf8_body_is_400` | `0xff 0xfe` body → `400 malformed_json`, never lossily accepted |
| `expect_100_continue` | `Expect: 100-continue` → `100 Continue` then `200`, in one connection |
| `keep_alive_two_requests_one_connection` | two `GET /health` on one socket, both answered, `connection: keep-alive` present |
| `connection_close_is_honoured` | `Connection: close` → socket closed after one response |
| `bad_request_line_envelope_unchanged` | `"garbage\r\n\r\n"` → the byte-identical `400` envelope of `serve.rs:51-66` |
| `unauthorized_envelope_unchanged` | token set, no header → the byte-identical `401` envelope of `serve.rs:71-86` |
| `token_prefix_is_rejected` | `Bearer <token-without-last-char>` and `Bearer <token>x` both `401` |
| `slow_client_does_not_block_others` | one connection sends headers and stalls; a second connection gets `/health` in <1 s. **This is the serial-accept defect at `serve.rs:36`.** |
| `twenty_concurrent_recalls` | 20 parallel `/v1/recall`, all `200`, no `database is locked` |
| `over_connection_cap_is_503` | `ENGRAM_HTTP_MAX_CONNECTIONS=2`, 5 simultaneous → the extras get `503` with `Retry-After`, not a hang |
| `header_timeout_is_408` | connect, send nothing → `408` within `header_timeout + 2s` |
| `unknown_endpoint_is_still_400` | compatibility pin for `wave05_daemon_*` |

### 11.2 `tests/daemon_core.rs` — the single writer

| Test | Asserts |
|---|---|
| `writes_are_serialized` | 200 concurrent `Core::write` closures each incrementing a row; final value 200, zero `SQLITE_BUSY` |
| `failed_closure_rolls_back_event_and_projection` | a closure that appends an event then errors leaves neither the event nor the projection row |
| `panicking_closure_does_not_kill_the_writer` | a closure that panics → caller gets `Transient`, the writer connection is back in autocommit (no transaction left open, §2.4), and the next write both succeeds **and commits** |
| `nested_write_is_refused_in_release` | `Core::write` called from inside a `ctx.store.with_tx` closure returns `Transient("nested write…")` immediately, in every profile — no 5 s stall, no `debug_assert` dependence (§2.5.2) |
| `queue_full_is_busy_not_unbounded` | 300 queued writes against a blocked writer → `sqlite_busy`-mapped error, memory bounded |
| `dropped_future_still_commits` | drop the `write` future after send; the row is present. Pins the documented semantics of §2.5.5. |
| `writer_thread_is_lazy` | `AppContext::new` + a read-only command spawns no writer thread (`metrics().writer_started == false`) |
| `one_write_connection_per_process` | cloning `Core` into 8 tasks yields one `writer_started` transition |
| `no_reader_migration_per_request` | 50 daemon requests → `migrate()` ran once (asserted via a `PRAGMA user_version` write counter / `sqlite3_total_changes` proxy on the store file mtime) |

### 11.3 `tests/daemon_jobs.rs`

| Test | Asserts |
|---|---|
| `submit_status_subscribe_completes` | submit compile → `202` with a job handle → SSE stream emits ≥1 `progress` and a terminal `completed` → `GET /v1/jobs/{id}` agrees |
| `submit_is_idempotent_per_kb` | two submits for the same `(compile, kb)` → the second returns the same job with `already_live: true`, and exactly one row exists |
| `unknown_job_is_404` | `GET /v1/jobs/<random-uuid>` → `404`, not `200 {"job": null}` |
| `cancel_pauses_at_a_checkpoint` | cancel a running backfill → status `paused` (or `cancelled` if it had no checkpoint), a cursor is persisted, and the response names the resume command |
| `resume_continues_from_the_cursor` | pause after N of M memories, resume → the remaining M−N are processed and none is processed twice |
| `resume_refuses_to_respend` | `llm: true` job paused with no derivation cache → `resume` is `400` naming the cost; `--force` proceeds |
| `paused_compile_is_not_completed` | a paused compile leaves `status='paused'`, and a memory deleted before it **still** constrains derived rows (`MEMORY_CONSTRAINS_DERIVED` unaffected). **The most dangerous mistake in the lane.** |
| `crashed_worker_is_reclaimed` | insert a `running` row with an expired lease → `reconcile_orphan_jobs` → `paused` (cursor) / `failed` (no cursor), with a stated reason |
| `lease_stealing_is_refused` | a second claimer on a live lease gets `false`; `checkpoint_job` from the stale owner returns `false` and writes nothing |
| `job_lifecycle_is_journalled` | submit → run → finish appends `job.submitted`, `job.completed` and (for compile) `compile.ran`, each in the transaction of the row change it describes; a `paused` job appends neither `job.completed` nor `compile.ran` (§4.6a) |
| `jobs_list_works_on_an_unmigrated_store` | open a v9 store, run `engram jobs list` → succeeds, because `ensure_jobs_schema` ran at store-open and not at first write (§4.1) |
| `legacy_compile_does_not_trip_the_live_index` | `jobs submit --detach` leaves a `queued` compile for a KB, then synchronous `engram compile <kb>` → succeeds (the queued row is abandoned), never a raw `UNIQUE constraint failed` |
| `duplicate_live_rows_are_collapsed_not_fatal` | seed two `running` compile rows for one KB (as 0.3.x could), then open the store → schema setup succeeds, one row survives (oldest), the loser is `abandoned` with a stated reason |
| `legacy_jobs_list_shape_unchanged` | every field `CompileJobRecord` serialized before wave 4 is still present with the same type |
| `no_daemon_detach_warns` | `engram jobs submit --detach` with no daemon → exit 0, `data.warning` naming that nothing will run it, row stays `queued` |
| `no_daemon_foreground_runs_inline` | `engram jobs submit` with no daemon → job reaches `completed` in the same process |
| `compile_async_returns_immediately` | `POST /v1/compile {"async":true}` returns in <500 ms on a corpus whose synchronous compile takes longer |
| `compile_holds_no_write_lock_across_llm` | with a stub LLM that sleeps 2 s, a concurrent `remember` completes in <500 ms. **This is `compile.rs:186`'s lock-across-network defect.** |

### 11.4 `tests/daemon_resolve_race.rs` — the `facts resolve` claim

| Test | Asserts |
|---|---|
| `concurrent_resolve_has_exactly_one_winner` | two threads resolve the same conflict id with different actions → exactly one exit 0; the loser is `conflict_already_resolved`, exit **3**, and the message names the winner |
| `loser_mutates_nothing` | after the race, exactly one fact is closed, `superseded_by` forms no cycle, and exactly one `decision.recorded` event exists |
| `no_two_node_cycle` | scripted crossed resolve (`supersede-old` and `reject-new` interleaved at the worst point via a test seam) cannot produce `a.superseded_by == b && b.superseded_by == a` |
| `cycle_guard_refuses` | pre-build a chain a→b→c and resolve a conflict that would close `c` in favour of `a` → `bad_input` naming the cycle, nothing written |
| `self_conflict_refused` | a queue row with `new_fact_id == old_fact_id` → refused, not self-superseded |
| `keep_both_records_a_decision` | `--action keep-both` writes a `DecisionRow` and a `decision.recorded` event (CD-3's precondition), where today it writes nothing |
| `resolve_journals_the_close` | a `supersede-old` resolve appends `fact.interval_closed` (authority `caller`, `sets_superseded_by`) **and** `decision.recorded`, in that order, in one transaction; `admin rebuild` reproduces `superseded_by`/`close_reason`/`close_authority` byte-identically (R2 + interfaces §2 invariant 3) |
| `decision_verdict_vocabulary` | the recorded verdict is `close_old`/`reject_new`/`keep_both` (R4), never the CLI's `supersede-old` spelling; `lookup_decision` finds it |
| `safety_close_is_refused_and_rolls_back` | `may_close` returning `Refuse` on a safety slot leaves the conflict `open`, writes no supersession, and returns the refusal reason unmodified (§5.4) |
| `already_resolved_is_not_bad_input` | resolving a resolved conflict → code `conflict_already_resolved`, not `bad_input` |
| `failure_after_claim_rolls_the_claim_back` | inject a failure between claim and supersede (env seam, in the style of `WRITE_FAIL_ENV`) → the conflict is still `open` |

### 11.5 `tests/daemon_recall_conflicts.rs`

| Test | Asserts |
|---|---|
| `recall_api_returns_conflicts` | seed a contested pair; `POST /v1/recall` returns a non-empty `conflicts` array |
| `cli_and_daemon_conflicts_are_identical` | the same query through `engram recall --json` and `/v1/recall` produce equal `conflicts` arrays (modulo ordering) — one builder, two surfaces |
| `no_conflicts_is_empty_array` | clean store → `"conflicts": []`, present |
| `suggested_action_vocabulary` | every emitted `suggested_action` ∈ R7's three values; safety pairs are `requires_context` |
| `resolve_command_has_no_pipe` | R12/IN-24: exactly one runnable command, no `|` |

### 11.6 `tests/daemon_store_isolation.rs`

| Test | Asserts |
|---|---|
| `store_flag_roots_everything` | `engram --store <tmp> remember …` writes `<tmp>/db.sqlite` and nothing under the real data dir |
| `flag_beats_env` | `ENGRAM_STORE=A` + `--store B` → B wins (interfaces §5 precedence) |
| `agent_info_with_store_opens_nothing` | `engram --store <tmp> agent-info` → exit 0, `<tmp>` **not created** |
| `ingest_subprocess_inherits_the_store` | daemon started with `--store <tmp>`, `POST /v1/ingest` → the ingested document is in `<tmp>/db.sqlite` and the real store's mtime is unchanged. **Brief constraint 1's highest-risk path.** |
| `serve_descriptor_is_written_and_removed` | `<tmp>/engramd.json` exists while serving, is gone after a clean shutdown, and a stale one (dead pid) is ignored and deleted |
| `empty_store_flag_is_rejected` | `--store ""` → exit 2 config error, never "unset" |

### 11.7 `tests/daemon_sleep_compile.rs`

| Test | Asserts |
|---|---|
| `pressure_below_threshold_does_not_compile` | 3 writes with `MIN_WRITES=25` → no job after 3 ticks |
| `pressure_triggers_a_compile` | `MIN_WRITES=2`, `IDLE_S=1` → a `compile` job with `params.trigger == "sleep_time"` and `actor == "daemon:sleep"` |
| `busy_store_is_not_compiled` | continuous writes → no job while writes keep arriving |
| `llm_is_off_by_default` | triggered job has `params.llm == false` with no env set |
| `daily_cap_holds` | `MAX_PER_DAY=1` → a second trigger in the same window submits nothing |
| `budget_exhaustion_pauses_not_fails` | with `llm: true` and a 0-headroom budget, no job is submitted at all; with headroom that runs out mid-job, the job ends `paused` with `stopped_reason: "budget"` |
| `disabled_in_cli` | a CLI process never spawns the sleep watcher |

### 11.8 Unit tests in-crate

- `engine.rs`: `would_cycle` table-driven (self, 2-cycle, 3-cycle, MAX_CHAIN overflow, acyclic);
  `Limits` env parsing including garbage values falling back to defaults with a warning;
  `http_status` covering every `ErrorCode` (the R11 contract test's daemon half).
- `engram-storage/src/jobs.rs`: `ensure_jobs_schema` idempotence (run twice on a fresh **and** on
  a legacy store), `submit_job` unique-index translation, `claim_job` exactly-one-row under two
  connections, `compile_pressure` on a KB that never compiled.
- All in-crate tests use `SqliteStore::open(tmp.path().join("db.sqlite"))`, never
  `paths::db_path()`.

### 11.9 Gates

`cargo build --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`,
`cargo test --workspace` green in the worktree before every commit. `engram bench personal
--json` compared before/after: the compile plan/apply split changes *when* rows are written, not
*which*, so the number must be identical, not merely close. `wave05_daemon_parity.rs`,
`wave05_daemon_envelope.rs`, `wave05_compiled_facts.rs` and `wave0_*` pass **unmodified** — if
one needs editing, the change is a compatibility break and goes to the lead first.

Ignored tests: this lane commits none `#[ignore]`d (CC-4). A `cfg`-gated test is an ignored test
with better manners, so the gating list is narrowed to things this lane genuinely cannot reach:

- **Not gated. Must be green in wave 4:** `keep_both_records_a_decision`. It proves §5.2 — the
  behaviour this lane *owns* and CD-3's precondition — and wave 4 landing green without it would
  be the lane passing on its own headline claim. `record_decision`/`append_event` are wave-2
  storage-journal deliverables that precede this wave; if they have genuinely not landed when the
  lane starts, wave 4 blocks on them exactly as it blocks on `respond.rs` (§12.1.7), and that is
  reported to the lead rather than absorbed by a `cfg`.
- **Gated, and only these two:** `cli_and_daemon_conflicts_are_identical` (needs `respond.rs`,
  contract-codegen wave 2) and `resume_refuses_to_respend` (needs move 3's derivation ledger,
  which §1.2.7 explicitly puts out of scope). Both are turned on at the wave-5 integration gate
  and both are listed in the lane report's open items.

---

## 12. Risks and open questions

### 12.1 Risks, ranked

1. **Two write connections in one process during the transition.** Until §7.3.1 completes, the
   CLI may write through `ctx.store` while `Core` writes through its own connection. WAL plus the
   5 s `busy_timeout` (`sqlite.rs:806`) makes this safe but not free. The failure mode that is
   *not* safe is calling `Core::write` from inside a `ctx.store.with_tx` closure — a guaranteed
   5 s stall then `SQLITE_BUSY`. Mitigation: the `debug_assert` in §2.5.2, the invariant stated
   on `Core::write`'s doc comment, and a test (`writes_are_serialized`). Residual risk: a release
   build has no `debug_assert`. Consider promoting it to a real check returning
   `Transient("nested write")` if any lane trips it in wave 5.
2. **The compile plan/apply split is the largest refactor in the lane** (`compile.rs:172-471`).
   It changes the ordering of derived-row writes relative to LLM calls. Mitigation: `bench
   personal` must be byte-identical, `wave05_compiled_facts.rs` unmodified, and the split lands
   as its own commit ahead of the jobs work so a bisect can isolate it.
3. **`completed` semantics.** If a paused or cancelled compile is ever written as `completed`,
   `MEMORY_CONSTRAINS_DERIVED` (`sqlite.rs:779-790`) will report that a deleted memory has been
   compiled out when it has not — a privacy-shaped bug, not a bookkeeping one. Pinned by
   `paused_compile_is_not_completed`; called out in §9.2 as an interfaces-level semantic.
4. **`env::set_var` for `--store`.** One call at the top of `dispatch`, before any path is read,
   but tokio's worker threads already exist. The optional `paths::set_store_override` in §10.1
   removes the theoretical race; the env export is still needed for the `/v1/ingest` subprocess.
5. **Hand-written HTTP.** A parser is a security surface. Mitigations: no allocation before a
   limit check, `Content-Length` validated before any body read, `read_exact` for exactly the
   declared length, no chunked support at all, `str::from_utf8` (never lossy), loopback-only by
   default, and 20 adversarial framing tests (§11.1). The honest alternative is `hyper`, rejected
   in §1.2.3; if the lead prefers the dependency, §3.3's semantics survive unchanged and §11.1
   still applies.
6. **SSE polling at 500 ms** costs one indexed `SELECT` per subscriber per tick. With the
   connection cap at 64 this is trivial, but a subscriber that never disconnects holds a
   connection permit; the `max_requests_per_conn` cap does not apply to a stream. Mitigation:
   subscriptions are capped separately at `ENGRAM_JOBS_MAX_SUBSCRIBERS` (default 16) and time out
   after 1 h.
7. **`respond.rs` is a hard dependency** (§3.8). If contract-codegen's wave 2 slips, wave 4's D6
   slips with it. This is by design (CM-9's pin) — the alternative is a fourth private copy of
   the resolve payload.
8. **Backfill batching depends on read-surfaces** (§10.3). The subprocess fallback works but
   costs a process per batch and cannot share the `Core` writer, so it reintroduces a second
   writing process. Prefer the patch.
9. **The daemon's own mutating surface is not single-writer while `/v1/ingest` shells out**
   (§3.5, review finding 23). Ingest re-enters the CLI as a child process with its own connection,
   so the daemon has a second, unsynchronised writer for its whole wave-4 life. It is not a *new*
   defect — that is what the code does today — but "single-writer core" is true of the daemon
   only modulo ingest, and saying otherwise would be the kind of half-claim 0.3.1 was spent
   removing. Wave 5 either re-plumbs ingest in-process or the claim gets qualified in
   `agent-info`.
10. **An older binary can still trip `idx_jobs_live`.** The wave-4 binary widens the legacy
    abandon and translates the constraint (§4.1), but a 0.3.x binary run against a wave-4 store
    meets a `queued` row it does not know how to abandon and fails its `INSERT`. Accepted:
    a failed `engram compile` with a constraint message, no corruption, and the store stays
    readable. Alternative, if the lead prefers zero downgrade friction: drop the unique index and
    do the idempotence check as a guarded `INSERT … WHERE NOT EXISTS` inside `submit_job`'s
    transaction, which is racier only across connections and is strictly worse — hence not chosen.

### 12.2 Open questions for the lead

1. **§9.1 — does daemon-jobs own `crates/engram-storage/src/jobs.rs`?** Everything in §4 depends
   on the answer; the fallback (storage-journal implements §4.2 in wave 2) costs a wave.
2. **Extend `compile_jobs` vs. a new `jobs` table.** This spec extends, to keep
   `MEMORY_CONSTRAINS_DERIVED` and the legacy `/v1/jobs` shape whole. If storage-journal's v10
   work already rewrites that table, say so now — the `ALTER`s and the migration must not collide.
3. **Should `ensure_jobs_schema` fold into migration v10** instead of running lazily? Idempotent
   either way; storage-journal's call.
4. **`Command::State` and `Command::Admin` dispatch arms** (CD-4) — this lane pre-accepts both
   patches, but someone must own writing them.
5. **`GET /v1/jobs/{id}` returning `404` for an unknown id** is a behaviour change from
   `200 {"job": null}`. interfaces §1 requires it; confirming it is a sanctioned break.
6. **Does the CLI ever proxy mutations to a live daemon?** §1.2.1 says no in wave 4. If the
   answer is "yes, eventually", the `Core` API is the right seam (a `RemoteCore` implementing the
   same surface), and that should be recorded now so `Core` is kept behind a trait rather than a
   struct.
7. **`ENGRAM_SLEEP_COMPILE` default.** This spec turns it **on** in the daemon with `llm: false`.
   If the lead would rather ship it off-by-default for one release, it is a one-line default
   change and §11.7's tests set the env explicitly either way.

Added after the adversarial review (`docs/arch/review-daemon-jobs.md`), unresolved:

8. **Who owns the HTTP envelope builders?** §3.8 needs `respond::{success, error, listing,
   conflicts}`; `spec-contract-codegen.md` §2.4 pins `respond.rs` as the *resolve payload*
   builder only (`resolve_conflict` / `attach` / `resolve_command_string`). Either codegen's
   `respond.rs` grows the envelope half (this lane's §10.2 request), or the lead names another
   home. The one outcome CM-9 forbids — a daemon-private envelope layer — is off the table, so
   D6 is blocked until this is answered. `ResolveAction` is defined in both specs and must
   collapse to one definition (codegen's) at the same time.
9. **`record_decision`'s final signature.** Interfaces §3.4 freezes `DecisionRow` (including
   `event_id`) but leaves the constructor as "fields as DecisionRow minus id/event_id", which
   cannot be right if the row stores an `event_id`. §5.2 assumes the caller appends
   `decision.recorded` first and passes the id in. Storage-journal should pin the exact signature
   in interfaces v3 before wave 4 codes against it.
10. **A shared DDL list for the new `compile_jobs` columns** (extends Q2/Q3). If storage-journal's
    v10 step and this lane's `ensure_jobs_schema` both issue the `ALTER`s and the duplicate
    collapse, they must issue *the same* statements in *the same order* — otherwise two idempotent
    functions produce two different tables depending on which ran first, and the collapse (which
    mutates rows) runs twice with different survivors. Whoever owns it, the statements live in one
    constant that the other calls.
11. **Does the brief's "CLI embeds the core when no daemon" survive §1.2.1's narrowing?** The CLI
    here *always* embeds and never proxies (Q6). That is half of move 4's client model; if the
    lead intends the other half eventually, `Core` should be a trait now so `RemoteCore` is a
    later addition rather than a later refactor of every call site.

---

## 13. Implementation order

Each step is a commit; the workspace is green at every one.

1. `engine.rs` skeleton: `Core`, writer thread, `WriteCtx`, metrics, `Core::write/read`,
   `AppContext.core`. Tests §11.2. (Needs §10.1's `with_tx_res`.)
2. `--store` flag + `apply_store_override` + `engramd --store`. Tests §11.6. (Needs §10.2's clap
   line; until it lands, `ENGRAM_STORE` alone is exercised.)
3. HTTP parser rewrite in `serve.rs`, still on per-request stores. Tests §11.1. This is the
   commit that makes the daemon correct under a real client.
4. Reader pool + `route` on `Core`; delete `serve.rs:129`. Tests §11.2's last two rows.
5. `engram-storage/src/jobs.rs` + `ensure_jobs_schema`. Unit tests §11.8.
6. `compile.rs` plan/apply split, no jobs yet. `bench personal` byte-identical;
   `wave05_compiled_facts.rs` unmodified. This is the lock-across-network fix and deserves to be
   bisectable on its own.
7. Job registry, runners, scheduler, HTTP job endpoints, SSE, CLI subcommands. Tests §11.3.
8. `Core::resolve_conflict` + `would_cycle`; `facts.rs` call-site patch (§10.3). Tests §11.4.
9. `/v1/recall` conflicts via `respond.rs`. Tests §11.5.
10. Sleep-time trigger + `/health` detail. Tests §11.7.
11. Full gate suite, `bench personal --json` diff, lane report with the final
    `cross_lane_patches` list.
