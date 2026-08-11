//! `engram serve` — local HTTP API over the same storage/retrieval/compiler code.
//!
//! The HTTP half used to be one 64 KiB `read()`. That is not a parser, it is a
//! guess: a body split across two TCP segments — any POST over ~1.4 KB on a
//! real link, or any client that writes headers and body separately, `curl`
//! with `Expect: 100-continue` included — was silently truncated and reported
//! to the caller as `serde error: EOF while parsing`. A body over 64 KiB got
//! the same misleading answer, and `from_utf8_lossy` turned corrupt bytes into
//! a plausible request. Connections were also served one at a time in the
//! accept loop, so one slow client blocked everybody, and every request opened
//! (and migrated) its own store.
//!
//! What replaces it is ~400 lines of `tokio::io`: incremental header read with
//! a limit, `Content-Length` validated before a byte of body is read,
//! `read_exact` for exactly the declared length, `str::from_utf8` and never
//! the lossy one, no chunked encoding at all (a named 501 rather than a
//! silent mis-parse), one task per connection, and a reader pool. No new
//! dependency: the surface is a dozen endpoints on loopback, and `hyper` would
//! be the largest single addition in the build.

use crate::commands::usage::{cohere_rerank_cost_usd, estimated_tokens, gemini_embed_cost_usd};
use crate::context::AppContext;
use crate::engine::{sleep, Core};
use crate::error::CliError;
use crate::respond;
use crate::retrieval::{
    hybrid_recall, render_answer_context, resolve_kb_scope, resolve_retrieval_backends, Filters,
    HybridParams, RecallMode, RetrievalProfile,
};
use chrono::{DateTime, Utc};
use engram_core::layers::approx_tokens;
use engram_core::temporal::{parse_bound, DayBound};
use engram_core::types::Layer;
use engram_storage::jobs::JobFilter;
use engram_storage::{paths, SqliteStore};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
    pub limits: Limits,
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_header_bytes: usize,
    pub max_body_bytes: u64,
    pub max_connections: usize,
    pub header_timeout: Duration,
    pub body_timeout: Duration,
    pub idle_timeout: Duration,
    pub max_requests_per_conn: u32,
    pub max_subscribers: usize,
    pub subscriber_max_age: Duration,
    pub sse_poll: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_header_bytes: 32 * 1024,
            max_body_bytes: 8 * 1024 * 1024,
            max_connections: 64,
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
            max_requests_per_conn: 100,
            max_subscribers: 16,
            subscriber_max_age: Duration::from_secs(3600),
            sse_poll: Duration::from_millis(500),
        }
    }
}

impl Limits {
    fn from_env() -> Limits {
        let d = Limits::default();
        Limits {
            max_header_bytes: env_usize("ENGRAM_HTTP_MAX_HEADER_BYTES", d.max_header_bytes),
            max_body_bytes: env_usize("ENGRAM_HTTP_MAX_BODY_BYTES", d.max_body_bytes as usize)
                as u64,
            max_connections: env_usize("ENGRAM_HTTP_MAX_CONNECTIONS", d.max_connections).max(1),
            max_subscribers: env_usize("ENGRAM_JOBS_MAX_SUBSCRIBERS", d.max_subscribers).max(1),
            sse_poll: Duration::from_millis(
                env_usize("ENGRAM_JOBS_POLL_MS", d.sse_poll.as_millis() as usize).max(50) as u64,
            ),
            ..d
        }
    }
}

/// Garbage falls back to the default with a warning rather than to zero: a
/// mistyped limit that silently means "refuse everything" is worse than one
/// that means "the documented default".
fn env_usize(key: &str, default: usize) -> usize {
    match std::env::var(key) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!("{key}={v} is not a number; using {default}");
            default
        }),
        Err(_) => default,
    }
}

struct Server {
    cfg: Arc<ServeConfig>,
    core: Core,
    permits: Arc<tokio::sync::Semaphore>,
    subscribers: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    started_at: DateTime<Utc>,
    sleep_cfg: sleep::SleepConfig,
}

impl Clone for Server {
    fn clone(&self) -> Self {
        Server {
            cfg: self.cfg.clone(),
            core: self.core.clone(),
            permits: self.permits.clone(),
            subscribers: self.subscribers.clone(),
            active: self.active.clone(),
            started_at: self.started_at,
            sleep_cfg: self.sleep_cfg.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// The daemon descriptor
// ---------------------------------------------------------------------------

/// Written on bind, removed on clean shutdown. Used for exactly one decision:
/// whether a submitted job runs inline in the CLI or is left for the daemon's
/// scheduler. A stale descriptor (dead process) is deleted by the first caller
/// that notices, so a crashed daemon cannot make every later `jobs submit`
/// wait for a scheduler that does not exist.
fn descriptor_path() -> std::path::PathBuf {
    paths::db_path_peek()
        .parent()
        .map(|p| p.join("engramd.json"))
        .unwrap_or_else(|| std::path::PathBuf::from("engramd.json"))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaemonDescriptor {
    pub pid: u32,
    pub host: String,
    pub port: u16,
    pub started_at: String,
    pub version: String,
    pub store: String,
    pub token_required: bool,
}

fn write_descriptor(d: &DaemonDescriptor) {
    let path = descriptor_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_vec_pretty(d) {
        let _ = std::fs::write(path, body);
    }
}

pub fn remove_descriptor() {
    let _ = std::fs::remove_file(descriptor_path());
}

/// The descriptor of a daemon that actually answers, or `None`.
///
/// Liveness is a TCP connect with a 250 ms timeout rather than a signal probe:
/// it needs no new dependency and it proves the thing that matters — that
/// something is listening on the port the descriptor names.
pub fn live_daemon() -> Option<DaemonDescriptor> {
    let path = descriptor_path();
    let raw = std::fs::read(&path).ok()?;
    let d: DaemonDescriptor = serde_json::from_slice(&raw).ok()?;
    let addr = format!("{}:{}", d.host, d.port);
    let resolved = std::net::ToSocketAddrs::to_socket_addrs(&addr)
        .ok()?
        .next()?;
    match std::net::TcpStream::connect_timeout(&resolved, Duration::from_millis(250)) {
        Ok(_) => Some(d),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

pub async fn run(
    ctx: &AppContext,
    host: String,
    port: u16,
    token: Option<String>,
) -> Result<(), CliError> {
    if !is_local_host(&host) && token.is_none() {
        return Err(CliError::Config(
            "binding engram serve outside localhost requires --token".into(),
        ));
    }
    let limits = Limits::from_env();
    let cfg = Arc::new(ServeConfig {
        host: host.clone(),
        port,
        token,
        limits: limits.clone(),
    });
    let listener = TcpListener::bind((host.as_str(), port)).await?;
    let bound = listener.local_addr()?;

    let core = ctx.core.clone();
    core.set_actor("daemon");
    // Readers are opened ONCE, here, instead of once per request: the old
    // `route` called `SqliteStore::open(paths::db_path())` — and therefore
    // `migrate()` — on every single request.
    core.warm_readers(readers_for())
        .map_err(|e| CliError::Config(format!("could not open the daemon's reader pool: {e}")))?;
    // A `kill -9` leaves `running` rows forever. Reconcile at start so they
    // become `paused` (with a cursor) or `failed` (without) and say why.
    if let Err(e) = ctx.store.reconcile_orphan_jobs(Duration::from_secs(90)) {
        tracing::warn!("could not reconcile orphaned jobs at startup: {e}");
    }

    let server = Server {
        cfg: cfg.clone(),
        core: core.clone(),
        permits: Arc::new(tokio::sync::Semaphore::new(limits.max_connections)),
        subscribers: Arc::new(AtomicUsize::new(0)),
        active: Arc::new(AtomicUsize::new(0)),
        started_at: Utc::now(),
        sleep_cfg: sleep::SleepConfig::from_env(),
    };

    write_descriptor(&DaemonDescriptor {
        pid: std::process::id(),
        host: host.clone(),
        port: bound.port(),
        started_at: server.started_at.to_rfc3339(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        store: core.db_path().display().to_string(),
        token_required: cfg.token.is_some(),
    });
    let background = spawn_background(&server);

    eprintln!("engramd listening on http://{host}:{}", bound.port());
    let outcome = accept_loop(listener, server).await;
    background.stop();
    remove_descriptor();
    outcome
}

async fn accept_loop(listener: TcpListener, server: Server) -> Result<(), CliError> {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                // A per-connection failure is not a reason to kill the daemon.
                // `EMFILE`/`ENFILE`/`ENOBUFS` used to take the whole process
                // down through a `?` right here.
                if matches!(
                    e.raw_os_error(),
                    Some(libc_emfile) if libc_emfile == 24 || libc_emfile == 23 || libc_emfile == 55
                ) {
                    tracing::warn!("accept: {e}; backing off");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                if e.kind() == std::io::ErrorKind::ConnectionAborted {
                    continue;
                }
                return Err(e.into());
            }
        };
        let permit = match server.permits.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // Over the cap: an immediate 503 with Retry-After, never a
                // silent hang the client has to time out on.
                tokio::spawn(async move { overloaded(stream).await });
                continue;
            }
        };
        let srv = server.clone();
        tokio::spawn(async move {
            let _permit = permit;
            srv.active.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = srv.serve_connection(stream).await {
                tracing::debug!(%peer, "connection ended: {e}");
            }
            srv.active.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

fn readers_for() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(2, 8)
}

/// The scheduler and the sleep-time watcher, on one dedicated thread with its
/// own current-thread runtime.
///
/// A runner reuses command code that holds a `SqliteStore` across its awaits,
/// so its future is not `Send` and `tokio::spawn` will not take it. Giving
/// that work its own thread is both the smallest fix and the right shape: a
/// job's connection belongs to the thread that runs the job.
struct Background {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Background {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn spawn_background(server: &Server) -> Background {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let core = server.core.clone();
    let sleep_cfg = server.sleep_cfg.clone();
    let flag = stop.clone();
    let handle = std::thread::Builder::new()
        .name("engram-jobs".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("job scheduler could not start: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                let mut last_sleep_tick =
                    std::time::Instant::now() - Duration::from_secs(sleep_cfg.tick_s);
                while !flag.load(Ordering::Relaxed) {
                    match claim_and_run_one(&core).await {
                        Ok(true) => continue, // a job ran; look for the next one
                        Ok(false) => {}
                        Err(e) => tracing::warn!("scheduler: {e}"),
                    }
                    if sleep_cfg.enabled
                        && last_sleep_tick.elapsed() >= Duration::from_secs(sleep_cfg.tick_s)
                    {
                        last_sleep_tick = std::time::Instant::now();
                        if let Err(e) = sleep::tick(&core, &sleep_cfg).await {
                            tracing::warn!("sleep-time compile check: {e}");
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        })
        .ok();
    Background { stop, handle }
}

async fn claim_and_run_one(core: &Core) -> Result<bool, CliError> {
    let kinds: Vec<String> = core.jobs().kinds().iter().map(|k| k.to_string()).collect();
    let next = core
        .read(move |s| {
            let refs: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
            Ok(s.next_runnable_job(&refs)?)
        })
        .await?;
    let Some(job) = next else { return Ok(false) };
    let owner = crate::engine::jobs::owner_tag();
    match core.run_job(job.id, &owner).await {
        Ok(row) => {
            tracing::info!(job = %row.id, kind = %row.kind, status = %row.status, "job finished");
            Ok(true)
        }
        Err(e) => {
            tracing::warn!(job = %job.id, "job failed: {e}");
            Ok(true)
        }
    }
}

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: String,
    body: String,
    keep_alive: bool,
    /// Lowercased names, trimmed values, in wire order.
    headers: Vec<(String, String)>,
}

/// A framing refusal: the status, the code, and whether the connection can
/// survive it. A body we declined to read leaves the stream unframed, so the
/// only honest thing to do afterwards is close.
struct Refusal {
    status: u16,
    code: &'static str,
    message: String,
    suggestion: &'static str,
    exit_code: i32,
}

impl Refusal {
    fn new(
        status: u16,
        code: &'static str,
        message: impl Into<String>,
        suggestion: &'static str,
    ) -> Refusal {
        Refusal {
            status,
            code,
            message: message.into(),
            suggestion,
            exit_code: 3,
        }
    }

    fn body(&self) -> serde_json::Value {
        json!({
            "version": "1",
            "status": "error",
            "error": {
                "code": self.code,
                "message": self.message,
                "suggestion": self.suggestion,
                "exit_code": self.exit_code,
            }
        })
    }
}

enum ReadOutcome {
    Request(Request),
    Refused(Refusal),
    /// The peer closed cleanly before sending anything.
    Closed,
}

impl Server {
    async fn serve_connection(&self, mut stream: TcpStream) -> Result<(), CliError> {
        let _ = stream.set_nodelay(true);
        let mut carry: Vec<u8> = Vec::new();
        for served in 0..self.cfg.limits.max_requests_per_conn {
            let outcome = self.read_request(&mut stream, &mut carry).await?;
            let req = match outcome {
                ReadOutcome::Closed => return Ok(()),
                ReadOutcome::Refused(r) => {
                    // Write the response, flush, half-close, then read and
                    // discard a bounded amount: closing hard while the client
                    // is still writing sends an RST that destroys the response
                    // we just queued on most stacks. Bounded politeness.
                    let body = serde_json::to_vec(&r.body())?;
                    write_response(
                        &mut stream,
                        r.status,
                        "application/json",
                        &body,
                        false,
                        None,
                    )
                    .await?;
                    let _ = stream.flush().await;
                    drain_briefly(&mut stream).await;
                    return Ok(());
                }
                ReadOutcome::Request(r) => r,
            };

            if let Some(token) = self.cfg.token.as_deref() {
                if !authorized(&req, token) {
                    let body = serde_json::to_vec(&json!({
                        "version": "1",
                        "status": "error",
                        "error": {
                            "code": "unauthorized",
                            "message": "unauthorized",
                            "suggestion": "Pass Authorization: Bearer <token>.",
                            "exit_code": 2
                        }
                    }))?;
                    write_response(&mut stream, 401, "application/json", &body, false, None)
                        .await?;
                    return Ok(());
                }
            }

            // The one streamed response in the daemon.
            if req.method == "GET" && is_events_path(&req.path) {
                self.stream_job_events(&mut stream, &req).await?;
                return Ok(());
            }

            let request_id = Uuid::new_v4().to_string();
            let last = served + 1 >= self.cfg.limits.max_requests_per_conn;
            let keep_alive = req.keep_alive && !last;
            let response = self.route(&req).await;
            let (status, body) = match response {
                Ok(payload) => (200u16, serde_json::to_vec(&payload)?),
                Err(e) => (http_status(&e), serde_json::to_vec(&respond::error(&e))?),
            };
            let retry_after = (status == 503).then_some(1u32);
            write_response(
                &mut stream,
                status,
                "application/json",
                &body,
                keep_alive,
                Some((&request_id, retry_after)),
            )
            .await?;
            if !keep_alive {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn read_request(
        &self,
        stream: &mut TcpStream,
        carry: &mut Vec<u8>,
    ) -> Result<ReadOutcome, CliError> {
        let limits = &self.cfg.limits;
        let mut buf = std::mem::take(carry);
        let first_read = buf.is_empty();
        let head_end = loop {
            if let Some(end) = find_header_end(&buf) {
                break end;
            }
            if buf.len() > limits.max_header_bytes {
                return Ok(ReadOutcome::Refused(Refusal::new(
                    431,
                    "header_too_large",
                    format!("request headers exceed {} bytes", limits.max_header_bytes),
                    "Send fewer or smaller headers.",
                )));
            }
            let timeout = if buf.is_empty() && first_read {
                limits.idle_timeout
            } else {
                limits.header_timeout
            };
            let mut chunk = [0u8; 8192];
            let n = match tokio::time::timeout(timeout, stream.read(&mut chunk)).await {
                Err(_) => {
                    if buf.is_empty() {
                        return Ok(ReadOutcome::Closed);
                    }
                    return Ok(ReadOutcome::Refused(Refusal::new(
                        408,
                        "request_timeout",
                        "timed out waiting for the request headers",
                        "Send the whole request head within the header timeout.",
                    )));
                }
                Ok(Ok(0)) => {
                    if buf.is_empty() {
                        return Ok(ReadOutcome::Closed);
                    }
                    return Ok(ReadOutcome::Refused(Refusal::new(
                        400,
                        "bad_request",
                        "connection closed mid-headers",
                        "Send a complete HTTP request.",
                    )));
                }
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e.into()),
            };
            buf.extend_from_slice(&chunk[..n]);
        };

        let (head_bytes, terminator_len) = head_end;
        let head = match std::str::from_utf8(&buf[..head_bytes]) {
            Ok(h) => h.to_string(),
            Err(_) => {
                return Ok(ReadOutcome::Refused(Refusal::new(
                    400,
                    "bad_request",
                    "request headers are not valid UTF-8",
                    "Send ASCII header names and values.",
                )))
            }
        };
        let mut lines = head.split('\n').map(|l| l.trim_end_matches('\r'));
        let request_line = lines.next().unwrap_or("");
        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            // Byte-identical to the pre-wave-4 envelope: `wave05_daemon_*`
            // asserts on it.
            return Ok(ReadOutcome::Refused(Refusal::new(
                400,
                "bad_request",
                "bad request",
                "Send a valid HTTP request line.",
            )));
        }
        let version = parts.get(2).copied().unwrap_or("HTTP/1.1");
        if !version.starts_with("HTTP/1.") {
            return Ok(ReadOutcome::Refused(Refusal::new(
                400,
                "bad_request",
                "bad request",
                "Send a valid HTTP request line.",
            )));
        }
        let method = parts[0].to_string();
        let (path, query) = split_path_query(parts[1]);

        let mut headers: Vec<(String, String)> = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                // Obsolete line folding: rejected rather than guessed at.
                return Ok(ReadOutcome::Refused(Refusal::new(
                    400,
                    "obsolete_line_folding",
                    "folded header lines are not accepted",
                    "Send each header on one line.",
                )));
            }
            let Some((name, value)) = line.split_once(':') else {
                return Ok(ReadOutcome::Refused(Refusal::new(
                    400,
                    "bad_request",
                    "malformed header line",
                    "Send `Name: value` header lines.",
                )));
            };
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }

        if let Some((_, te)) = headers.iter().find(|(n, _)| n == "transfer-encoding") {
            if !te.eq_ignore_ascii_case("identity") {
                return Ok(ReadOutcome::Refused(Refusal::new(
                    501,
                    "unsupported_transfer_encoding",
                    format!("transfer-encoding `{te}` is not supported"),
                    "Send a Content-Length body; chunked encoding is not supported.",
                )));
            }
        }

        let lengths: Vec<&String> = headers
            .iter()
            .filter(|(n, _)| n == "content-length")
            .map(|(_, v)| v)
            .collect();
        let content_length: u64 = match lengths.as_slice() {
            [] => 0,
            values => {
                let mut parsed: Option<u64> = None;
                for v in values {
                    let Ok(n) = v.trim().parse::<u64>() else {
                        return Ok(ReadOutcome::Refused(Refusal::new(
                            400,
                            "bad_content_length",
                            format!("content-length `{v}` is not a number"),
                            "Send one numeric Content-Length.",
                        )));
                    };
                    if parsed.is_some_and(|p| p != n) {
                        return Ok(ReadOutcome::Refused(Refusal::new(
                            400,
                            "bad_content_length",
                            "two different Content-Length headers",
                            "Send exactly one Content-Length.",
                        )));
                    }
                    parsed = Some(n);
                }
                parsed.unwrap_or(0)
            }
        };
        if content_length > limits.max_body_bytes {
            // Refused WITHOUT reading the body: draining a 2 GB request to be
            // polite is a free denial of service.
            return Ok(ReadOutcome::Refused(Refusal::new(
                413,
                "body_too_large",
                format!(
                    "body of {content_length} bytes exceeds the {} byte limit",
                    limits.max_body_bytes
                ),
                "Send a smaller body, or raise ENGRAM_HTTP_MAX_BODY_BYTES.",
            )));
        }

        if headers
            .iter()
            .any(|(n, v)| n == "expect" && v.eq_ignore_ascii_case("100-continue"))
        {
            stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").await?;
            stream.flush().await?;
        }

        let mut body_bytes = buf.split_off(head_bytes + terminator_len);
        while (body_bytes.len() as u64) < content_length {
            let want = (content_length - body_bytes.len() as u64) as usize;
            let mut chunk = vec![0u8; want.min(64 * 1024)];
            let n = match tokio::time::timeout(limits.body_timeout, stream.read(&mut chunk)).await {
                Err(_) => {
                    return Ok(ReadOutcome::Refused(Refusal::new(
                        408,
                        "request_timeout",
                        "timed out waiting for the request body",
                        "Send the whole body within the body timeout.",
                    )))
                }
                Ok(Ok(0)) => {
                    // THE TRUNCATION DEFECT, now named. This used to be
                    // reported as `serde error: EOF while parsing`, which sent
                    // the caller looking for a bug in their JSON.
                    return Ok(ReadOutcome::Refused(Refusal::new(
                        400,
                        "incomplete_body",
                        format!(
                            "body ended after {} of {content_length} declared bytes",
                            body_bytes.len()
                        ),
                        "Send exactly Content-Length bytes.",
                    )));
                }
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e.into()),
            };
            body_bytes.extend_from_slice(&chunk[..n]);
        }
        // Anything past the declared length belongs to the next request on
        // this connection.
        let rest = body_bytes.split_off(content_length as usize);
        *carry = rest;

        let body = match std::str::from_utf8(&body_bytes) {
            Ok(b) => b.to_string(),
            Err(_) => {
                // NEVER lossy: `from_utf8_lossy` turned a corrupt body into a
                // plausible one.
                return Ok(ReadOutcome::Refused(Refusal::new(
                    400,
                    "malformed_json",
                    "request body is not valid UTF-8",
                    "Send a UTF-8 encoded JSON body.",
                )));
            }
        };

        let connection = headers
            .iter()
            .find(|(n, _)| n == "connection")
            .map(|(_, v)| v.to_ascii_lowercase())
            .unwrap_or_default();
        let keep_alive = if version == "HTTP/1.0" {
            connection.contains("keep-alive")
        } else {
            !connection.contains("close")
        };

        Ok(ReadOutcome::Request(Request {
            method,
            path: path.to_string(),
            query: query.to_string(),
            body,
            keep_alive,
            headers,
        }))
    }
}

/// `\r\n\r\n`, or a bare `\n\n` — agents hand-roll requests with `printf`, and
/// refusing them over a line ending helps nobody. Returns (head length,
/// terminator length).
fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

async fn drain_briefly(stream: &mut TcpStream) {
    let deadline = Duration::from_millis(250);
    let mut sink = [0u8; 8192];
    let mut seen = 0usize;
    while seen < 64 * 1024 {
        match tokio::time::timeout(deadline, stream.read(&mut sink)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => return,
            Ok(Ok(n)) => seen += n,
        }
    }
}

async fn overloaded(mut stream: TcpStream) {
    let body = json!({
        "version": "1",
        "status": "error",
        "error": {
            "code": "sqlite_busy",
            "message": "engram is at its connection limit",
            "suggestion": "Retry in a second, or raise ENGRAM_HTTP_MAX_CONNECTIONS.",
            "exit_code": 1
        }
    });
    if let Ok(bytes) = serde_json::to_vec(&body) {
        let _ = write_response(
            &mut stream,
            503,
            "application/json",
            &bytes,
            false,
            Some(("", Some(1))),
        )
        .await;
    }
    // Closing hard while the client is still writing its request sends an RST
    // that destroys the refusal we just queued, and the caller sees a reset
    // instead of the reason. Read and discard a bounded amount first.
    drain_briefly(&mut stream).await;
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    keep_alive: bool,
    extra: Option<(&str, Option<u32>)>,
) -> Result<(), CliError> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: {}\r\n",
        reason_phrase(status),
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    if let Some((request_id, retry_after)) = extra {
        if !request_id.is_empty() {
            head.push_str(&format!("x-engram-request-id: {request_id}\r\n"));
        }
        if let Some(secs) = retry_after {
            head.push_str(&format!("retry-after: {secs}\r\n"));
        }
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => "ERROR",
    }
}

/// D20's table, with the pinned wire spellings. Everything unlisted is a 500,
/// which is the honest answer for a failure we did not classify.
pub fn http_status(err: &CliError) -> u16 {
    match err.code() {
        "bad_input" | "malformed_json" | "wrong_id_kind" | "config_error" | "serde_error"
        | "ingest_error" | "bench_error" => 400,
        "not_found" => 404,
        "conflict_already_resolved" => 409,
        "rate_limited" => 429,
        "sqlite_busy" => 503,
        _ => 500,
    }
}

/// Compares in time constant IN THE TOKEN'S CONTENTS — no byte-by-byte early
/// exit. Length is compared first and so is still distinguishable by timing;
/// that is stated rather than papered over. The old check did `ends_with` on
/// the raw header line, which also accepted `Bearer\ttoken` shapes.
fn authorized(req: &Request, token: &str) -> bool {
    let Some((_, value)) = req.headers.iter().find(|(n, _)| n == "authorization") else {
        return false;
    };
    let Some(presented) = value.strip_prefix("Bearer ") else {
        return false;
    };
    let (a, b) = (presented.as_bytes(), token.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn is_events_path(path: &str) -> bool {
    path.starts_with("/v1/jobs/") && path.ends_with("/events")
}

fn split_path_query(target: &str) -> (&str, &str) {
    target.split_once('?').unwrap_or((target, ""))
}

/// Percent-decoding, properly. It used to replace `%20` and nothing else, so
/// `?kb=a%2Fb` was silently answered for a KB literally named `a%2Fb`.
fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(&s[i + 1..i + 3], 16) {
                Ok(b) => {
                    out.push(b);
                    i += 3;
                }
                // A stray `%` is data, not a broken escape: dropping it would
                // silently change the caller's value.
                Err(_) => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param_usize(query: &str, key: &str) -> Option<usize> {
    query_param(query, key).and_then(|v| v.parse().ok())
}

fn parse_uuid_path(id: &str) -> Result<Uuid, CliError> {
    Uuid::parse_str(id).map_err(|_| CliError::BadInput(format!("invalid UUID: {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding_is_not_just_space() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("two%20words"), "two words");
        assert_eq!(percent_decode("plus+sign"), "plus sign");
        // A stray escape is data, not a reason to lose a character.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn header_end_accepts_bare_lf() {
        assert_eq!(
            find_header_end(b"GET / HTTP/1.1\r\n\r\nbody"),
            Some((14, 4))
        );
        assert_eq!(
            find_header_end(b"POST / HTTP/1.1\r\nhost: x\r\n\r\n{}"),
            Some((24, 4))
        );
        assert_eq!(find_header_end(b"GET / HTTP/1.1\n\nbody"), Some((14, 2)));
        assert_eq!(find_header_end(b"GET / HTTP/1.1\r\n"), None);
    }

    #[test]
    fn status_table_matches_the_pinned_wire_spellings() {
        // D20: `config_error`, not `config`. The old table matched on a string
        // the enum never emits, so every config failure fell through to 500.
        assert_eq!(http_status(&CliError::Config("x".into())), 400);
        assert_eq!(http_status(&CliError::BadInput("x".into())), 400);
        assert_eq!(
            http_status(&CliError::MalformedJson {
                field: "body",
                detail: "x".into()
            }),
            400
        );
        assert_eq!(
            http_status(&CliError::NotFound {
                what: "job",
                handle: "job:x".into()
            }),
            404
        );
        assert_eq!(
            http_status(&CliError::ConflictAlreadyResolved {
                conflict_id: "x".into(),
                verdict: "y".into()
            }),
            409
        );
        assert_eq!(http_status(&CliError::RateLimited("x".into())), 429);
        assert_eq!(http_status(&CliError::SqliteBusy("x".into())), 503);
        assert_eq!(http_status(&CliError::Transient("x".into())), 500);
    }

    #[test]
    fn a_token_prefix_is_not_a_token() {
        let req = |value: &str| Request {
            method: "GET".into(),
            path: "/health".into(),
            query: String::new(),
            body: String::new(),
            keep_alive: true,
            headers: vec![("authorization".into(), value.into())],
        };
        assert!(authorized(&req("Bearer secret"), "secret"));
        assert!(!authorized(&req("Bearer secre"), "secret"));
        assert!(!authorized(&req("Bearer secrets"), "secret"));
        assert!(!authorized(&req("Bearer\tsecret"), "secret"));
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

impl Server {
    async fn route(&self, req: &Request) -> Result<serde_json::Value, CliError> {
        let core = &self.core;
        let (method, path, query, body) = (
            req.method.as_str(),
            req.path.as_str(),
            req.query.as_str(),
            req.body.as_str(),
        );
        match (method, path) {
            ("GET", "/health") => self.health().await,
            ("GET", "/v1/kbs") => {
                let kbs = core.read(|s| Ok(s.list_kbs()?)).await?;
                Ok(data(json!({ "knowledge_bases": kbs })))
            }
            ("POST", "/v1/kbs") => {
                let req: KbCreateRequest = serde_json::from_str(body)?;
                let name = req.name.clone();
                let description = req.description.clone();
                core.write("api.kbs.create", move |w| {
                    Ok(w.store.ensure_kb(&name, description.as_deref())?)
                })
                .await?;
                Ok(data(json!({ "name": req.name })))
            }
            ("POST", "/v1/recall") => {
                let body = body.to_string();
                with_reader(core, move |s| async move {
                    let out = recall_body(&s, &body).await;
                    (out, s)
                })
                .await
            }
            ("POST", "/v1/ingest") => ingest_api(body).await,
            ("POST", "/v1/compile") => self.compile_api(body).await,
            ("POST", "/v1/reindex") => {
                let body = body.to_string();
                // Reindex re-embeds every chunk and writes as it goes, so it
                // takes a connection of its own rather than holding a pooled
                // reader for minutes.
                with_own_store(core, move |db| async move {
                    let req: ReindexRequest = serde_json::from_str(&body)?;
                    let store = SqliteStore::open(db)?;
                    let stats = crate::commands::reindex::reindex_store(
                        &store,
                        req.kb.as_deref(),
                        req.all.unwrap_or(false),
                        req.offline.unwrap_or(false),
                    )
                    .await?;
                    Ok(data(json!({ "job_id": null, "reindex": stats })))
                })
                .await
            }
            ("GET", "/v1/entities") => {
                let kb = query_param(query, "kb");
                let limit = query_param_usize(query, "limit").unwrap_or(100);
                let entities = core
                    .read(move |s| Ok(s.list_entities(kb.as_deref(), limit, 1)?))
                    .await?;
                Ok(data(json!({ "entities": entities })))
            }
            ("GET", "/v1/documents") => {
                let kb = query_param(query, "kb");
                let limit = query_param_usize(query, "limit").unwrap_or(100);
                let documents = core
                    .read(move |s| Ok(s.list_documents(kb.as_deref(), limit)?))
                    .await?;
                Ok(data(json!({ "documents": documents })))
            }
            ("POST", "/v1/jobs") => self.submit_job_api(body).await,
            ("GET", "/v1/jobs") => {
                let filter = JobFilter {
                    kind: query_param(query, "kind"),
                    kb: query_param(query, "kb"),
                    status: query_param(query, "status"),
                    limit: query_param_usize(query, "limit").unwrap_or(50),
                    offset: query_param_usize(query, "offset").unwrap_or(0),
                };
                let (rows, total) = core.read(move |s| Ok(s.list_jobs(&filter)?)).await?;
                let items: Vec<serde_json::Value> = rows.iter().map(|r| r.to_json()).collect();
                let mut obj = serde_json::Map::new();
                obj.insert("jobs".to_string(), json!(items));
                crate::output::Listing::page(items.len(), total, None).attach(&mut obj);
                Ok(data(serde_json::Value::Object(obj)))
            }
            ("GET", "/v1/usage") => {
                let kb = query_param(query, "kb");
                let since = query_param(query, "since");
                let summary = core
                    .read(move |s| Ok(s.usage_summary(kb.as_deref(), since.as_deref())?))
                    .await?;
                Ok(data(json!({ "summary": summary })))
            }
            ("GET", "/v1/budget") => {
                let kb = query_param(query, "kb");
                let scope = budget_scope(kb.as_deref());
                let s2 = scope.clone();
                let budget = core.read(move |s| Ok(s.get_usage_budget(&s2)?)).await?;
                Ok(data(json!({ "scope": scope, "budget": budget })))
            }
            ("POST", "/v1/budget") => {
                let req: BudgetRequest = serde_json::from_str(body)?;
                let scope = budget_scope(req.kb.as_deref());
                let (s2, kb, daily, monthly) = (
                    scope.clone(),
                    req.kb.clone(),
                    req.daily_usd,
                    req.monthly_usd,
                );
                let budget = core
                    .write("api.budget.set", move |w| {
                        w.store
                            .upsert_usage_budget(&s2, kb.as_deref(), daily, monthly)?;
                        Ok(w.store.get_usage_budget(&s2)?)
                    })
                    .await?;
                Ok(data(json!({ "scope": scope, "budget": budget })))
            }
            ("POST", "/v1/research") => {
                let body = body.to_string();
                with_reader(core, move |s| async move {
                    let out = research_body(&s, &body).await;
                    (out, s)
                })
                .await
            }
            _ if method == "GET" && path.starts_with("/v1/documents/") => {
                let id = parse_uuid_path(path.trim_start_matches("/v1/documents/"))?;
                let document = core.read(move |s| Ok(s.get_document(id)?)).await?;
                Ok(data(json!({ "document": document })))
            }
            _ if method == "DELETE" && path.starts_with("/v1/documents/") => {
                if query_param(query, "confirm").as_deref() != Some("true") {
                    return Err(CliError::BadInput(
                        "DELETE /v1/documents/{id} requires ?confirm=true".into(),
                    ));
                }
                let id = parse_uuid_path(path.trim_start_matches("/v1/documents/"))?;
                let deleted = core
                    .write("api.documents.delete", move |w| {
                        Ok(w.store.delete_document(id)?)
                    })
                    .await?;
                Ok(data(json!({ "document_id": id, "deleted": deleted })))
            }
            _ if method == "GET" && path.starts_with("/v1/entities/") => {
                let name = path.trim_start_matches("/v1/entities/").to_string();
                let kb = query_param(query, "kb");
                let entity = core
                    .read(move |s| Ok(s.find_entity(kb.as_deref(), &name)?))
                    .await?;
                Ok(data(json!({ "entity": entity })))
            }
            _ if method == "POST" && path.starts_with("/v1/jobs/") && path.ends_with("/cancel") => {
                let id = parse_uuid_path(job_id_segment(path, "/cancel"))?;
                self.cancel_job_api(id).await
            }
            _ if method == "POST" && path.starts_with("/v1/jobs/") && path.ends_with("/resume") => {
                let id = parse_uuid_path(job_id_segment(path, "/resume"))?;
                self.resume_job_api(id, body).await
            }
            _ if method == "GET" && path.starts_with("/v1/jobs/") => {
                let id = parse_uuid_path(path.trim_start_matches("/v1/jobs/"))?;
                // An unknown id is `not_found`, not `200 {"job": null}` —
                // interfaces §1, and a deliberate wire change.
                let job = core
                    .read(move |s| {
                        s.get_job(id)?.ok_or(CliError::NotFound {
                            what: "job",
                            handle: format!("job:{id}"),
                        })
                    })
                    .await?;
                Ok(data(json!({ "job": job.to_json() })))
            }
            // Unknown endpoint stays 400, not 404: `route`'s fallback has
            // always raised BadInput and `wave05_daemon_*` asserts on that
            // envelope. A missing RESOURCE is the 404 case.
            _ => Err(CliError::BadInput(format!(
                "unknown endpoint: {method} {path}"
            ))),
        }
    }

    async fn health(&self) -> Result<serde_json::Value, CliError> {
        let core = &self.core;
        let metrics = core.metrics();
        let schema = core.read(|s| Ok(s.schema_version()?)).await;
        let counts = core
            .read(|s| {
                let mut out = HashMap::new();
                for status in ["running", "queued", "failed"] {
                    let (_, total) = s.list_jobs(&JobFilter {
                        status: Some(status.to_string()),
                        limit: 1,
                        ..Default::default()
                    })?;
                    out.insert(status.to_string(), total);
                }
                Ok(out)
            })
            .await
            .unwrap_or_default();
        // `ok: false` when writes are impossible. A health endpoint that says
        // `ok: true` while the writer is dead is precisely the dishonesty an
        // entire release was spent removing.
        let ok = metrics.writer_alive && schema.is_ok();
        Ok(json!({
            "version": "1",
            "status": if ok { "success" } else { "error" },
            "data": {
                "ok": ok,
                "schema_version": schema.unwrap_or(0),
                "store": core.db_path().display().to_string(),
                "uptime_s": (Utc::now() - self.started_at).num_seconds().max(0),
                "writer": {
                    "started": metrics.writer_started,
                    "alive": metrics.writer_alive,
                    "queue_depth": metrics.queue_depth,
                    "writes_total": metrics.writes_total,
                    "writes_failed": metrics.writes_failed,
                    "last_write_ms": metrics.last_write_ms,
                    "max_write_ms": metrics.max_write_ms,
                },
                "jobs": counts,
                "sleep_compile": sleep::health(core, &self.sleep_cfg),
                "connections": {
                    "active": self.active.load(Ordering::Relaxed),
                    "max": self.cfg.limits.max_connections,
                    "subscribers": self.subscribers.load(Ordering::Relaxed),
                }
            }
        }))
    }

    async fn submit_job_api(&self, body: &str) -> Result<serde_json::Value, CliError> {
        let req: JobSubmitRequest = serde_json::from_str(body)?;
        let (row, already_live) = self
            .core
            .submit_job(
                req.kind.clone(),
                req.kb.unwrap_or_else(|| "default".to_string()),
                req.params.unwrap_or(json!({})),
            )
            .await?;
        Ok(data(
            json!({ "job": row.to_json(), "already_live": already_live }),
        ))
    }

    async fn cancel_job_api(&self, id: Uuid) -> Result<serde_json::Value, CliError> {
        let job = self
            .core
            .read(move |s| {
                s.get_job(id)?.ok_or(CliError::NotFound {
                    what: "job",
                    handle: format!("job:{id}"),
                })
            })
            .await?;
        if engram_storage::jobs::is_terminal(&job.status) {
            return Err(CliError::BadInput(format!(
                "job {id} is already {} — nothing to cancel",
                job.status
            )));
        }
        self.core
            .write("api.jobs.cancel", move |w| {
                Ok(w.store.request_cancel(id)?)
            })
            .await?;
        let job = self.core.read(move |s| Ok(s.get_job(id)?)).await?;
        Ok(data(json!({ "job": job.map(|j| j.to_json()) })))
    }

    async fn resume_job_api(&self, id: Uuid, body: &str) -> Result<serde_json::Value, CliError> {
        let force = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("force").and_then(|f| f.as_bool()))
            .unwrap_or(false);
        let job = self
            .core
            .read(move |s| {
                s.get_job(id)?.ok_or(CliError::NotFound {
                    what: "job",
                    handle: format!("job:{id}"),
                })
            })
            .await?;
        let llm = job
            .params
            .get("llm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let cached = self
            .core
            .read(|s| Ok(s.derivation_cache_available()))
            .await?;
        if llm && !force && !cached {
            return Err(CliError::BadInput(format!(
                "resuming job {id} would re-run LLM extraction that was already paid for (no \
                 derivation cache in this store); pass {{\"force\": true}} to accept the cost"
            )));
        }
        let requeued = self
            .core
            .write("api.jobs.resume", move |w| Ok(w.store.requeue_job(id)?))
            .await?;
        if !requeued {
            return Err(CliError::BadInput(format!(
                "job {id} is {} — only paused, failed or cancelled jobs resume",
                job.status
            )));
        }
        let job = self.core.read(move |s| Ok(s.get_job(id)?)).await?;
        Ok(data(json!({ "job": job.map(|j| j.to_json()) })))
    }

    async fn compile_api(&self, body: &str) -> Result<serde_json::Value, CliError> {
        let req: CompileRequest = serde_json::from_str(body)?;
        let params = json!({
            "llm": req.llm.unwrap_or(false),
            "extraction_model": req.extraction_model,
            "synthesis_model": req.synthesis_model,
            "max_llm_chunks": req.max_llm_chunks,
            "trigger": "api",
        });
        if req.r#async.unwrap_or(false) {
            let (row, already_live) = self
                .core
                .submit_job("compile".to_string(), req.kb.clone(), params)
                .await?;
            return Ok(data(
                json!({ "job": row.to_json(), "already_live": already_live }),
            ));
        }
        // Default stays synchronous and returns the exact current shape.
        with_own_store(&self.core, move |db| async move {
            compile_sync(db, req, params).await
        })
        .await
    }
}

async fn compile_sync(
    db: std::path::PathBuf,
    req: CompileRequest,
    params: serde_json::Value,
) -> Result<serde_json::Value, CliError> {
    {
        let ctx = AppContext::with_db(crate::output::OutputFormat::Json, true, db)?;
        let job_id = ctx.with_store_tx(|s| {
            s.abandon_live_jobs("compile", &req.kb, "superseded by a synchronous compile")?;
            Ok(s.create_compile_job(&req.kb, "evidence", params.clone())?)
        })?;
        let options = crate::commands::compile::CompileParams {
            llm: req.llm.unwrap_or(false),
            extraction_model: req.extraction_model.clone(),
            synthesis_model: req.synthesis_model.clone(),
            max_llm_chunks: req.max_llm_chunks,
            budget_usd: None,
            trigger: Some("api".to_string()),
        }
        .into_options();
        match crate::commands::compile::compile_kb_with_options(&ctx, &req.kb, options).await {
            Ok(stats) => {
                ctx.store.finish_compile_job(
                    job_id,
                    "completed",
                    None,
                    json!({ "stats": stats }),
                )?;
                Ok(data(json!({ "job_id": job_id, "compile": stats })))
            }
            Err(err) => {
                // The compile error is the one the caller needs; a job row
                // that is no longer 'running' must not replace it.
                if let Err(bookkeeping) = ctx.store.finish_compile_job(
                    job_id,
                    "failed",
                    Some(&err.to_string()),
                    json!({}),
                ) {
                    tracing::warn!("could not mark compile job {job_id} failed: {bookkeeping}");
                }
                Err(err)
            }
        }
    }
}

impl Server {
    /// `GET /v1/jobs/{id}/events` — the one streamed response in the daemon.
    ///
    /// It polls the row rather than subscribing to an in-memory channel: the
    /// row is the single source of truth, so a subscriber attached to a
    /// different process than the worker sees identical output and there is no
    /// in-memory state to lose on restart.
    async fn stream_job_events(
        &self,
        stream: &mut TcpStream,
        req: &Request,
    ) -> Result<(), CliError> {
        let raw = req
            .path
            .trim_start_matches("/v1/jobs/")
            .trim_end_matches("/events");
        let id = match parse_uuid_path(raw) {
            Ok(id) => id,
            Err(e) => {
                let body = serde_json::to_vec(&respond::error(&e))?;
                return write_response(stream, 400, "application/json", &body, false, None).await;
            }
        };
        let known = self.core.read(move |s| Ok(s.get_job(id)?)).await?;
        if known.is_none() {
            let e = CliError::NotFound {
                what: "job",
                handle: format!("job:{id}"),
            };
            let body = serde_json::to_vec(&respond::error(&e))?;
            return write_response(stream, 404, "application/json", &body, false, None).await;
        }
        // Streams hold a connection permit for their whole life and are exempt
        // from max_requests_per_conn, so they carry their own cap.
        let current = self.subscribers.fetch_add(1, Ordering::Relaxed);
        if current >= self.cfg.limits.max_subscribers {
            self.subscribers.fetch_sub(1, Ordering::Relaxed);
            let body = serde_json::to_vec(&json!({
                "version": "1",
                "status": "error",
                "error": {
                    "code": "sqlite_busy",
                    "message": "too many job subscribers",
                    "suggestion": "Poll GET /v1/jobs/{id} instead, or raise ENGRAM_JOBS_MAX_SUBSCRIBERS.",
                    "exit_code": 1
                }
            }))?;
            return write_response(
                stream,
                503,
                "application/json",
                &body,
                false,
                Some(("", Some(1))),
            )
            .await;
        }
        let out = self.pump_events(stream, id).await;
        self.subscribers.fetch_sub(1, Ordering::Relaxed);
        out
    }

    async fn pump_events(&self, stream: &mut TcpStream, id: Uuid) -> Result<(), CliError> {
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
            )
            .await?;
        stream.flush().await?;
        let started = std::time::Instant::now();
        let mut last = String::new();
        let mut since_keepalive = std::time::Instant::now();
        loop {
            let job = self.core.read(move |s| Ok(s.get_job(id)?)).await?;
            let Some(job) = job else { return Ok(()) };
            let fingerprint = format!("{}|{}|{}", job.status, job.progress_done, job.updated_at);
            if fingerprint != last {
                last = fingerprint;
                let terminal = engram_storage::jobs::is_terminal(&job.status)
                    || job.status == engram_storage::jobs::STATUS_PAUSED;
                let name = if terminal {
                    job.status.as_str()
                } else {
                    "progress"
                };
                let frame = format!(
                    "event: {name}\ndata: {}\n\n",
                    serde_json::to_string(&job.to_json())?
                );
                stream.write_all(frame.as_bytes()).await?;
                stream.flush().await?;
                if terminal {
                    return Ok(());
                }
            }
            if since_keepalive.elapsed() >= Duration::from_secs(15) {
                since_keepalive = std::time::Instant::now();
                stream.write_all(b": keep-alive\n\n").await?;
                stream.flush().await?;
            }
            if started.elapsed() >= self.cfg.limits.subscriber_max_age {
                let frame = format!(
                    "event: timeout\ndata: {}\n\n",
                    json!({
                        "job_id": id.to_string(),
                        "note": "stream closed after its maximum age",
                        "poll": format!("GET /v1/jobs/{id}")
                    })
                );
                stream.write_all(frame.as_bytes()).await?;
                stream.flush().await?;
                return Ok(());
            }
            tokio::time::sleep(self.cfg.limits.sse_poll).await;
        }
    }
}

fn data(payload: serde_json::Value) -> serde_json::Value {
    json!({ "version": "1", "status": "success", "data": payload })
}

fn budget_scope(kb: Option<&str>) -> String {
    kb.map(|k| format!("kb:{k}"))
        .unwrap_or_else(|| "global".to_string())
}

fn job_id_segment<'a>(path: &'a str, suffix: &str) -> &'a str {
    path.trim_start_matches("/v1/jobs/")
        .trim_end_matches(suffix)
}

/// Run work that must hold a store connection across `await` points.
///
/// Retrieval embeds the query with a provider mid-call, so its future owns the
/// store while it is suspended — and `SqliteStore` is `Send` but not `Sync`,
/// which makes such a future un-spawnable. It therefore runs on a blocking
/// thread with its own current-thread runtime, and the connection is handed
/// back to the pool afterwards rather than being reopened per request.
async fn with_reader<T, F, Fut>(core: &Core, f: F) -> Result<T, CliError>
where
    F: FnOnce(SqliteStore) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = (Result<T, CliError>, SqliteStore)>,
    T: Send + 'static,
{
    let (store, pooled) = core.checkout_parts()?;
    let joined = tokio::task::spawn_blocking(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                return (
                    Err(CliError::Transient(format!(
                        "could not start a request runtime: {e}"
                    ))),
                    store,
                )
            }
        };
        rt.block_on(f(store))
    })
    .await;
    match joined {
        Ok((out, store)) => {
            core.checkin_parts(store, pooled);
            out
        }
        Err(e) => Err(CliError::Transient(format!("request task failed: {e}"))),
    }
}

/// The same, on a connection of its own: for work that WRITES (reindex,
/// compile) and must not hold a pooled reader while it does.
async fn with_own_store<T, F, Fut>(core: &Core, f: F) -> Result<T, CliError>
where
    F: FnOnce(std::path::PathBuf) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<T, CliError>>,
    T: Send + 'static,
{
    let db = core.db_path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| CliError::Transient(format!("could not start a request runtime: {e}")))?;
        rt.block_on(f(db))
    })
    .await
    .map_err(|e| CliError::Transient(format!("request task failed: {e}")))?
}

/// Daemon diary semantics, CLI parity: an omitted `diary` means the diary
/// literally named "default" — exactly what `engram recall` does when no
/// --diary flag is given — and `"*"` is the explicit way to search every
/// diary. Mapping omission to ALL diaries let a minimal `/v1/recall` request
/// return specialist-diary content the equivalent CLI invocation excludes.
fn daemon_diary(diary: Option<String>) -> Option<String> {
    match diary.as_deref() {
        None => Some("default".to_string()),
        Some("*") => None,
        Some(_) => diary,
    }
}

async fn research_body(store: &SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: ResearchRequest = serde_json::from_str(body)?;
    // An empty query is refused BEFORE any provider or budget work, exactly as
    // `engram research ""` is (exit 3 -> HTTP 400). It used to fall through to
    // hybrid_recall, which embedded the empty string: a paid Gemini call, a
    // recorded usage event and a budget decremented for a query that cannot
    // match anything. With no key it was reported as a config error instead,
    // which sends the caller to fix their keys over a typo.
    if req.query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let kb_filter = req.kb.clone().filter(|k| k != "*");
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    // The daemon is a retrieval surface like any other: a cloud profile with no
    // key is a config error here too, and the caller's budget is checked before
    // the paid call. It used to do neither and answer from the stub instead.
    let backends = resolve_retrieval_backends(store, profile, kb_filter.as_deref())?;
    let params = HybridParams {
        query: &req.query,
        top_k: req.top_k.unwrap_or(12),
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: daemon_diary(req.diary.clone()),
            kb: kb_filter.clone(),
            since: None,
            until: None,
            // Research assembles evidence for synthesis; expired memories are
            // not evidence of current state. Same default as CLI research.
            include_expired: false,
        },
        mode: RecallMode::Explore,
        graph_hops: 2,
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let outcome = hybrid_recall(
        store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;
    let (results, notes) = (outcome.results, outcome.notes);
    // A budget is a guardrail over RECORDED usage, so a surface that spends
    // must record: this endpoint paid Gemini and told the ledger nothing, so
    // daemon research traffic could never move a cap it now enforces.
    if backends.cloud_embedder {
        let tokens = estimated_tokens(&req.query);
        store.record_usage_event(
            "gemini",
            "api_research_query_embed",
            Some(backends.embed_model.as_str()),
            kb_filter.as_deref(),
            daemon_diary(req.diary.clone()).as_deref(),
            1,
            1,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({ "estimated": true, "api": true }),
        )?;
    }
    if backends.use_cohere() && !results.is_empty() {
        let search_units = 1.0;
        store.record_usage_event(
            "cohere",
            "api_research_rerank",
            Some(backends.cohere_model.as_str()),
            kb_filter.as_deref(),
            daemon_diary(req.diary.clone()).as_deref(),
            1,
            rerank_candidates as i64,
            0,
            0,
            search_units,
            cohere_rerank_cost_usd(search_units),
            json!({ "api": true }),
        )?;
    }
    let status = if results.is_empty() {
        "no_results"
    } else {
        "success"
    };
    let mut resp = json!({
        "version": "1",
        "status": status,
        "data": {
            "status": status,
            "query": req.query,
            "answer_context": render_answer_context(&results),
            "results": results.iter().map(|r| json!({
                "id": r.id.to_string(),
                "kind": r.kind,
                "score": r.score,
                "rerank_score": r.rerank_score,
                "kb": r.kb,
                "content": r.content,
                "citations": r.citations,
                "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "metadata": {
                "profile": profile.as_str(),
                // A caller cannot tell 1536-dim Gemini retrieval from the
                // 64-dim offline stub out of `profile` alone: `offline` and a
                // forced stub render the same word. The CLI's `research` has
                // reported these since Wave 0; the daemon copy did not.
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "prompt_format": backends.prompt_format,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
                "diary_isolation": notes.diary_isolation_json(),
                "temporal_scope": notes.temporal_scope_json()
            }
        }
    });
    // Presence IS the signal: skill.md tells agents the field appears only in
    // affected responses, so a healthy daemon must OMIT it — `json!` would
    // render a None as `null` and make every presence check fire.
    if let Some(note) = backends.embedder.degradation_note() {
        resp["data"]["metadata"]["embedder_degraded"] = serde_json::json!(note);
    }
    Ok(resp)
}

async fn recall_body(store: &SqliteStore, body: &str) -> Result<serde_json::Value, CliError> {
    let req: RecallRequest = serde_json::from_str(body)?;
    // An empty query is refused BEFORE any provider or budget work, exactly as
    // `engram recall ""` is (exit 3 -> HTTP 400). It used to fall through to
    // hybrid_recall, which embedded the empty string: a paid Gemini call, a
    // recorded usage event and a budget decremented for a query that cannot
    // match anything. With no key it was reported as a config error instead,
    // which sends the caller to fix their keys over a typo.
    if req.query.trim().is_empty() {
        return Err(CliError::BadInput("query cannot be empty".into()));
    }
    // Same scope precedence as `engram recall`: explicit kb > retrieval.default_kb
    // > every KB. An omitted `kb` searches everything rather than the single
    // namespace literally named "default".
    let scope = resolve_kb_scope(req.kb.as_deref(), req.all_kbs.unwrap_or(false));
    let req_diary = req.diary.clone();
    let profile = RetrievalProfile::parse(req.profile.as_deref().unwrap_or("cloud_quality"))?;
    let mode = RecallMode::parse(req.mode.as_deref().unwrap_or("evidence"))?;
    // The clap defaults of `engram recall`, so the same request spelled over
    // HTTP answers the same way (cli.rs: --layer topic, --top-k 10).
    let layer = parse_layer(req.layer.as_deref().unwrap_or("topic"))?;
    let since = parse_bound_param(req.since.as_deref(), DayBound::Start)?;
    let until = parse_bound_param(req.until.as_deref(), DayBound::End)?;
    let top_k = req.top_k.unwrap_or(10);
    let rerank_candidates = req
        .rerank_top_n
        .unwrap_or_else(|| profile.default_rerank_top_n());
    // Same provider + budget rule as `engram recall` (see
    // `resolve_retrieval_backends`): no key under a cloud profile is a config
    // error, not stub results dressed up as gemini ones. The embedder is also
    // asked what it is rather than being hardcoded here.
    let backends = resolve_retrieval_backends(store, profile, scope.as_deref())?;
    let params = HybridParams {
        query: &req.query,
        top_k,
        rrf_k: 60.0,
        rerank_top_n: rerank_candidates,
        filters: Filters {
            diary: daemon_diary(req_diary.clone()),
            kb: scope.kb.clone(),
            since,
            until,
            include_expired: req.include_expired.unwrap_or(false),
        },
        mode,
        graph_hops: req.graph_hops.unwrap_or(1),
        allow_mixed_embeddings: req.allow_mixed_embeddings.unwrap_or(false),
    };
    let outcome = hybrid_recall(
        store,
        backends.embedder.as_ref(),
        backends.reranker.as_deref(),
        params,
    )
    .await?;
    let (results, notes) = (outcome.results, outcome.notes);
    // Same layer contract as `engram recall`: the token budget only trims what
    // exceeds the caller's own top_k, so a layer can never return FEWER results
    // than were asked for. Reported so a caller can see which budget applied.
    let budget = layer.default_token_budget();
    let mut used_tokens = 0usize;
    let mut layered = Vec::new();
    for r in &results {
        let cost = approx_tokens(&r.content);
        if layered.len() >= top_k && used_tokens + cost > budget {
            break;
        }
        layered.push(r);
        used_tokens += cost;
    }
    if backends.cloud_embedder {
        let tokens = estimated_tokens(&req.query);
        store.record_usage_event(
            "gemini",
            "api_recall_query_embed",
            Some(backends.embed_model.as_str()),
            scope.as_deref(),
            daemon_diary(req_diary.clone()).as_deref(),
            1,
            1,
            tokens,
            0,
            0.0,
            gemini_embed_cost_usd(tokens),
            json!({ "estimated": true, "api": true }),
        )?;
    }
    if backends.use_cohere() && !results.is_empty() {
        let search_units = 1.0;
        store.record_usage_event(
            "cohere",
            "api_recall_rerank",
            Some(backends.cohere_model.as_str()),
            scope.as_deref(),
            daemon_diary(req_diary.clone()).as_deref(),
            1,
            rerank_candidates as i64,
            0,
            0,
            search_units,
            cohere_rerank_cost_usd(search_units),
            json!({ "api": true }),
        )?;
    }
    let status = if results.is_empty() {
        "no_results"
    } else {
        "success"
    };
    let mut resp = json!({
        "version": "1",
        "status": status,
        "data": {
            "status": status,
            // `layered` is a Vec<&HybridResult>; `.iter()` would hand the
            // renderer `&&HybridResult`, so copy the references out.
            "answer_context": render_answer_context(layered.iter().copied()),
            "results": layered.iter().map(|r| json!({
                "id": r.id.to_string(),
                "kind": r.kind,
                "score": r.score,
                "rerank_score": r.rerank_score,
                "kb": r.kb,
                "content": r.content,
                "citations": r.citations,
                "sources": r.sources.iter().map(|s| format!("{:?}", s).to_lowercase()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "metadata": {
                "profile": profile.as_str(),
                "embed_model": backends.embed_model,
                "embed_dims": backends.embed_dims,
                "prompt_format": backends.prompt_format,
                "reranker": backends.reranker_label,
                "candidates_considered": rerank_candidates,
                "layer": format!("{:?}", layer).to_lowercase(),
                "token_budget": budget,
                "tokens_used": used_tokens,
                "kb": scope.label(),
                "kb_scope": scope.origin.as_str(),
                "diary_isolation": notes.diary_isolation_json(),
                "temporal_scope": notes.temporal_scope_json()
            }
        }
    });
    // The contradictions the CLI has always surfaced and the daemon never did:
    // an agent driving engram over HTTP used to be handed one side of a live
    // disagreement with no signal at all. Same builder, same R7 vocabulary,
    // same single runnable resolve command — one producer, two surfaces.
    //
    // Additive and ALWAYS present: an empty array rather than an omitted key,
    // because absence-as-signal is already spoken for by `embedder_degraded`
    // and reusing it for two meanings is how presence checks rot.
    let conflicts = crate::commands::recall::surface_conflicts(
        store,
        scope.as_deref(),
        daemon_diary(req_diary.clone()).as_deref(),
        &req.query,
        10,
    )?;
    resp["data"]["conflicts"] = json!(conflicts.items);
    resp["data"]["metadata"]["conflicts_total"] = json!(conflicts.total);
    // Presence IS the signal: skill.md tells agents the field appears only in
    // affected responses, so a healthy daemon must OMIT it — `json!` would
    // render a None as `null` and make every presence check fire.
    if let Some(note) = backends.embedder.degradation_note() {
        resp["data"]["metadata"]["embedder_degraded"] = serde_json::json!(note);
    }
    Ok(resp)
}

async fn ingest_api(body: &str) -> Result<serde_json::Value, CliError> {
    let req: IngestRequest = serde_json::from_str(body)?;
    let mut exe = std::env::current_exe()?;
    if exe
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n == "engramd")
        .unwrap_or(false)
    {
        exe.set_file_name("engram");
    }
    let mut command = std::process::Command::new(exe);
    command
        .arg("--json")
        .arg("ingest")
        .arg(req.path)
        .arg("--kb")
        .arg(req.kb.unwrap_or_else(|| "default".to_string()))
        .arg("--mode")
        .arg(req.mode.unwrap_or_else(|| "auto".to_string()))
        .arg("--diary")
        .arg(req.diary.unwrap_or_else(|| "default".to_string()))
        .arg("--compile")
        .arg(req.compile.unwrap_or_else(|| "none".to_string()));
    if req.dry_run.unwrap_or(false) {
        command.arg("--dry-run");
    }
    if req.all.unwrap_or(false) {
        command.arg("--all");
    }
    if let Some(max_files) = req.max_files {
        command.arg("--max-files").arg(max_files.to_string());
    }
    for pattern in req.include.unwrap_or_default() {
        command.arg("--include").arg(pattern);
    }
    for pattern in req.exclude.unwrap_or_default() {
        command.arg("--exclude").arg(pattern);
    }
    let output = command.output()?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(CliError::Transient(format!(
            "ingest subprocess failed: {err}"
        )));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(json!({
        "version": "1",
        "status": "success",
        "data": {
            "job_id": null,
            "ingest": parsed.get("data").cloned().unwrap_or(parsed)
        }
    }))
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `recall --layer`, over HTTP.
///
/// Duplicated rather than shared out of `commands::recall`: that module belongs
/// to another lane this wave, and a six-arm match is cheaper than a cross-file
/// conflict. The error string is byte-identical to `recall.rs` on purpose —
/// one rule, two entry points — so folding the two together is a pure delete.
fn parse_layer(s: &str) -> Result<Layer, CliError> {
    match s.to_ascii_lowercase().as_str() {
        "identity" | "l0" => Ok(Layer::Identity),
        "critical" | "l1" => Ok(Layer::Critical),
        "topic" | "l2" => Ok(Layer::Topic),
        "deep" | "l3" => Ok(Layer::Deep),
        other => Err(CliError::BadInput(format!(
            "unknown layer: {other} (expected identity|critical|topic|deep)"
        ))),
    }
}

/// `recall --since/--until`, over HTTP: RFC3339, or a bare `YYYY-MM-DD` that
/// means the START of that UTC day for `since` and its END for `until`, so
/// `since == until == D` selects exactly day D.
///
/// Unparseable input is a hard error, never a silent "now" and never a silently
/// ignored field — the daemon used to accept `since` and drop it on the floor,
/// so a windowed request was answered from the whole store and the caller was
/// never told.
fn parse_bound_param(
    raw: Option<&str>,
    bound: DayBound,
) -> Result<Option<DateTime<Utc>>, CliError> {
    match raw {
        None => Ok(None),
        Some(v) => parse_bound(v, bound).map(Some).ok_or_else(|| {
            CliError::BadInput(format!(
                "invalid timestamp {v}: expected RFC3339 (2026-06-01T09:30:00Z) or YYYY-MM-DD"
            ))
        }),
    }
}

#[derive(Debug, Deserialize)]
struct KbCreateRequest {
    name: String,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompileRequest {
    kb: String,
    llm: Option<bool>,
    extraction_model: Option<String>,
    synthesis_model: Option<String>,
    max_llm_chunks: Option<usize>,
    /// Opt-in. The default stays synchronous and returns the exact shape it
    /// always has, because a caller that has not asked for a handle is not
    /// expecting one.
    #[serde(default)]
    r#async: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct JobSubmitRequest {
    kind: String,
    kb: Option<String>,
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ReindexRequest {
    kb: Option<String>,
    all: Option<bool>,
    /// Explicit stub opt-in, mirroring the CLI --offline flag; keyless
    /// reindex without it is a config error, never a silent stub rebuild.
    offline: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct IngestRequest {
    path: String,
    kb: Option<String>,
    mode: Option<String>,
    diary: Option<String>,
    compile: Option<String>,
    dry_run: Option<bool>,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    max_files: Option<usize>,
    all: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RecallRequest {
    query: String,
    kb: Option<String>,
    all_kbs: Option<bool>,
    diary: Option<String>,
    mode: Option<String>,
    profile: Option<String>,
    layer: Option<String>,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    graph_hops: Option<u8>,
    allow_mixed_embeddings: Option<bool>,
    since: Option<String>,
    until: Option<String>,
    /// Same contract as `recall --include-expired` (CLI/daemon parity).
    include_expired: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ResearchRequest {
    query: String,
    kb: Option<String>,
    diary: Option<String>,
    profile: Option<String>,
    top_k: Option<usize>,
    rerank_top_n: Option<usize>,
    allow_mixed_embeddings: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BudgetRequest {
    kb: Option<String>,
    daily_usd: Option<f64>,
    monthly_usd: Option<f64>,
}
