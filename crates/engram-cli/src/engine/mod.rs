//! The single-writer core.
//!
//! Named `engine`, not `core`, so `use core::…` inside this crate keeps
//! resolving to the standard library.
//!
//! One process, one write connection, one thread. `rusqlite::Connection` is
//! `Send` but not `Sync`, so the two shapes that work are a mutex every caller
//! holds across whatever it does next — which is the "write locks held across
//! network calls" defect this exists to kill — or a dedicated thread behind a
//! channel. This is the second: the closure that runs inside the transaction
//! is `FnOnce(&WriteCtx) -> Result<T, CliError>` and is not a future, so
//! holding the write lock across an `await` is not merely discouraged, it does
//! not typecheck.

pub mod jobs;
pub mod resolve;
pub mod sleep;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use engram_storage::SqliteStore;

use crate::error::CliError;

pub use jobs::{
    Continue, JobBudget, JobCtx, JobOutcome, JobRegistry, JobRunner, PauseReason, JOB_KINDS,
};
pub use resolve::{ResolveApplied, ResolveRequest};

/// How many writes may queue before a caller is pushed back. Bounded on
/// purpose: an unbounded queue converts a write stall into unbounded memory
/// growth and a daemon that dies instead of saying it is busy.
const WRITE_QUEUE_CAPACITY: usize = 256;

/// What a write closure is handed.
///
/// Deliberately not `&SqliteStore` alone: `EventDraft.actor` needs the actor
/// string, and every event a write appends must be attributable without the
/// caller re-deriving who it is.
pub struct WriteCtx<'a> {
    pub store: &'a SqliteStore,
    pub actor: &'a str,
}

type WriteTask = Box<dyn FnOnce(&SqliteStore) + Send + 'static>;

/// The only sanctioned mutation entry point in the process.
#[derive(Clone)]
pub struct Core {
    inner: Arc<CoreInner>,
}

struct CoreInner {
    db_path: PathBuf,
    /// Set on a context whose store has no file behind it (the in-memory
    /// benchmark fixture). Opening a second `:memory:` connection would be a
    /// second, empty database, so a write here refuses by name instead of
    /// succeeding against a store nobody can read.
    detached: Option<&'static str>,
    /// Spawned on the FIRST write, so `engram recall` never pays for a writer
    /// thread and a second connection it will not use.
    writer: OnceLock<WriterHandle>,
    writer_init: Mutex<()>,
    /// Daemon-only: N reader connections opened once at startup, instead of
    /// one `SqliteStore::open` (and therefore one `migrate()`) per request.
    readers: OnceLock<ReaderPool>,
    actor: RwLock<String>,
    metrics: CoreMetrics,
    jobs: JobRegistry,
    sleep: sleep::SleepState,
}

struct WriterHandle {
    tx: SyncSender<WriteTask>,
    alive: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct ReaderPool {
    /// `Connection` is `Send` but not `Sync`, so a pooled reader is checked
    /// out for the duration of one call and returned, never shared.
    idle: Mutex<Vec<SqliteStore>>,
    total: usize,
}

#[derive(Default)]
struct CoreMetrics {
    writes_total: AtomicU64,
    writes_failed: AtomicU64,
    queue_depth: AtomicUsize,
    last_write_ms: AtomicU64,
    last_wait_ms: AtomicU64,
    max_write_ms: AtomicU64,
    last_write_at_ms: AtomicU64,
    writer_started: AtomicBool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct CoreMetricsSnapshot {
    pub writes_total: u64,
    pub writes_failed: u64,
    pub queue_depth: usize,
    pub last_write_ms: u64,
    pub last_wait_ms: u64,
    pub max_write_ms: u64,
    pub writer_started: bool,
    pub writer_alive: bool,
    pub readers: usize,
}

thread_local! {
    /// How deep the CALLING thread is inside an `AppContext::with_store_tx`
    /// transaction. `Core::write` refuses rather than queueing behind a lock
    /// its own caller holds — see [`Core::write`].
    static COMPANION_TX_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// RAII guard: increments the companion-transaction depth for the current
/// thread. `AppContext::with_store_tx` holds one for the life of the closure.
pub struct CompanionTxGuard(());

impl CompanionTxGuard {
    pub fn enter() -> CompanionTxGuard {
        COMPANION_TX_DEPTH.with(|d| d.set(d.get() + 1));
        CompanionTxGuard(())
    }
}

impl Drop for CompanionTxGuard {
    fn drop(&mut self) {
        COMPANION_TX_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

fn in_companion_tx() -> bool {
    COMPANION_TX_DEPTH.with(|d| d.get() > 0)
}

impl Core {
    /// `db_path` is resolved ONCE by the caller, so a `--store` flag or an
    /// `ENGRAM_STORE` change mid-process cannot split one process across two
    /// databases.
    pub fn new(db_path: PathBuf, actor: impl Into<String>) -> Self {
        Core::build(db_path, actor, None)
    }

    /// A core that refuses to write, with a reason. For contexts over a store
    /// that has no file — a write through a second `:memory:` connection would
    /// land in a database nothing else can see.
    pub fn detached(reason: &'static str) -> Self {
        Core::build(PathBuf::from(":memory:"), "fixture", Some(reason))
    }

    fn build(db_path: PathBuf, actor: impl Into<String>, detached: Option<&'static str>) -> Self {
        Core {
            inner: Arc::new(CoreInner {
                db_path,
                detached,
                writer: OnceLock::new(),
                writer_init: Mutex::new(()),
                readers: OnceLock::new(),
                actor: RwLock::new(actor.into()),
                metrics: CoreMetrics::default(),
                jobs: JobRegistry::with_builtin_runners(),
                sleep: sleep::SleepState::default(),
            }),
        }
    }

    pub fn db_path(&self) -> &std::path::Path {
        &self.inner.db_path
    }

    pub fn set_actor(&self, actor: impl Into<String>) {
        if let Ok(mut a) = self.inner.actor.write() {
            *a = actor.into();
        }
    }

    pub fn actor(&self) -> String {
        self.inner
            .actor
            .read()
            .map(|a| a.clone())
            .unwrap_or_else(|_| "cli".to_string())
    }

    pub fn jobs(&self) -> &JobRegistry {
        &self.inner.jobs
    }

    pub(crate) fn sleep_state(&self) -> &sleep::SleepState {
        &self.inner.sleep
    }

    /// The single mutation entry point.
    ///
    /// `f` runs on the writer thread inside one `BEGIN IMMEDIATE`, with no
    /// possibility of awaiting anything: embeddings, LLM calls and HTTP happen
    /// BEFORE the call and arrive as owned data. On `Err` the transaction is
    /// rolled back whole, including any journal event the closure appended —
    /// which makes interfaces §2 invariant 1 ("every projection mutation in the
    /// SAME transaction as its event append") unavoidable rather than merely
    /// documented.
    ///
    /// **Cancellation-safe at the boundary, not in the middle.** Dropping the
    /// returned future does not abort the transaction: it is already queued or
    /// already running, and a committed write is not undone by a client
    /// hanging up. "The caller went away so the write did not happen" is a lie
    /// the daemon must never tell.
    ///
    /// **Never call this from inside a transaction on another connection.**
    /// Two connections both wanting `RESERVED` is a lock wait then
    /// `SQLITE_BUSY`. Callers that need a transaction on `ctx.store` take it
    /// through `AppContext::with_store_tx`, which arms a real runtime guard in
    /// every profile — a `debug_assert` would be stripped in exactly the
    /// builds users install.
    pub async fn write<T, F>(&self, op: &'static str, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&WriteCtx<'_>) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self.enqueue(op, f)?;
        rx.await.map_err(|_| {
            CliError::Transient("the writer thread dropped a queued write".to_string())
        })?
    }

    /// Blocking form, for synchronous call sites. Identical semantics.
    pub fn write_blocking<T, F>(&self, op: &'static str, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&WriteCtx<'_>) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self.enqueue(op, f)?;
        rx.blocking_recv().map_err(|_| {
            CliError::Transient("the writer thread dropped a queued write".to_string())
        })?
    }

    fn enqueue<T, F>(
        &self,
        op: &'static str,
        f: F,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<T, CliError>>, CliError>
    where
        F: FnOnce(&WriteCtx<'_>) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static,
    {
        if in_companion_tx() {
            return Err(CliError::Transient(format!(
                "nested write: Core::write(\"{op}\") was called inside an open transaction on \
                 another connection in this process; finish that transaction first"
            )));
        }
        let handle = self.writer()?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let actor = self.actor();
        let metrics = self.inner.clone();
        let queued_at = Instant::now();
        metrics.metrics.queue_depth.fetch_add(1, Ordering::Relaxed);
        let task: WriteTask = Box::new(move |store: &SqliteStore| {
            let waited = queued_at.elapsed();
            let started = Instant::now();
            let out = store.with_tx_res(|s| {
                f(&WriteCtx {
                    store: s,
                    actor: &actor,
                })
            });
            metrics.metrics.record(waited, started.elapsed(), &out);
            // A dropped receiver means the caller cancelled. The transaction
            // still committed, which is correct.
            let _ = reply_tx.send(out);
        });
        match handle.tx.try_send(task) {
            Ok(()) => Ok(reply_rx),
            Err(TrySendError::Full(_)) => {
                self.inner
                    .metrics
                    .queue_depth
                    .fetch_sub(1, Ordering::Relaxed);
                Err(CliError::SqliteBusy(format!(
                    "engram is busy: {WRITE_QUEUE_CAPACITY} writes are already queued"
                )))
            }
            Err(TrySendError::Disconnected(_)) => {
                self.inner
                    .metrics
                    .queue_depth
                    .fetch_sub(1, Ordering::Relaxed);
                Err(CliError::Transient(
                    "the writer thread is gone; restart engram".to_string(),
                ))
            }
        }
    }

    /// Read on a pooled connection (daemon) or on a throwaway one (CLI, where
    /// the pool was never warmed). Readers never take the write lock, so a
    /// compile job cannot stall `/v1/recall`.
    pub async fn read<T, F>(&self, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&SqliteStore) -> Result<T, CliError> + Send + 'static,
        T: Send + 'static,
    {
        let me = self.clone();
        match tokio::task::spawn_blocking(move || me.read_blocking(f)).await {
            Ok(v) => v,
            Err(e) => Err(CliError::Transient(format!("read task failed: {e}"))),
        }
    }

    pub fn read_blocking<T, F>(&self, f: F) -> Result<T, CliError>
    where
        F: FnOnce(&SqliteStore) -> Result<T, CliError>,
    {
        let checked_out = self.checkout_reader()?;
        let out = f(&checked_out.store);
        self.return_reader(checked_out);
        out
    }

    /// Open `n` reader connections up front. `serve::run` calls this; the CLI
    /// never builds a pool.
    ///
    /// `SqliteStore::open` runs `migrate()`, so the pool would run n
    /// migrations at startup — idempotent but pointless, and it makes "migrate
    /// ran once" untestable. The pool therefore opens its connections AFTER
    /// the context's own `open` has migrated, and the migration fast path
    /// (an up-to-date `user_version` returns immediately, writing nothing)
    /// makes each subsequent open read-only.
    pub fn warm_readers(&self, n: usize) -> Result<(), CliError> {
        if self.inner.readers.get().is_some() {
            return Ok(());
        }
        let n = n.clamp(1, 8);
        let mut conns = Vec::with_capacity(n);
        for _ in 0..n {
            conns.push(SqliteStore::open(&self.inner.db_path)?);
        }
        let _ = self.inner.readers.set(ReaderPool {
            idle: Mutex::new(conns),
            total: n,
        });
        Ok(())
    }

    pub fn metrics(&self) -> CoreMetricsSnapshot {
        let m = &self.inner.metrics;
        CoreMetricsSnapshot {
            writes_total: m.writes_total.load(Ordering::Relaxed),
            writes_failed: m.writes_failed.load(Ordering::Relaxed),
            queue_depth: m.queue_depth.load(Ordering::Relaxed),
            last_write_ms: m.last_write_ms.load(Ordering::Relaxed),
            last_wait_ms: m.last_wait_ms.load(Ordering::Relaxed),
            max_write_ms: m.max_write_ms.load(Ordering::Relaxed),
            writer_started: m.writer_started.load(Ordering::Relaxed),
            writer_alive: self
                .inner
                .writer
                .get()
                .map(|w| w.alive.load(Ordering::Relaxed))
                .unwrap_or(true),
            readers: self.inner.readers.get().map(|r| r.total).unwrap_or(0),
        }
    }

    /// Seconds since this process's last write through `Core`. `None` when it
    /// has never written. A cheap upper bound on idleness and never the only
    /// input to the sleep gate — writes from other processes are invisible
    /// here, which is why the gate also reads the store.
    pub fn seconds_since_last_write(&self) -> Option<u64> {
        let at = self.inner.metrics.last_write_at_ms.load(Ordering::Relaxed);
        if at == 0 {
            return None;
        }
        Some(now_ms().saturating_sub(at) / 1000)
    }

    /// Drain the queue and join the writer thread. A job mid-checkpoint
    /// finishes its transaction; one that has not started does not.
    pub async fn shutdown(&self, grace: Duration) {
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || me.shutdown_blocking(grace)).await;
    }

    pub fn shutdown_blocking(&self, grace: Duration) {
        let Some(handle) = self.inner.writer.get() else {
            return;
        };
        // Dropping every sender closes the channel and ends the loop. The
        // handle holds one, so hand the thread a sentinel instead and wait.
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let sentinel: WriteTask = Box::new(move |_| {
            let _ = done_tx.send(());
        });
        if handle.tx.try_send(sentinel).is_ok() {
            let _ = done_rx.recv_timeout(grace);
        }
        // The thread itself only ends when every sender drops, which happens
        // when the last `Core` clone goes. Taking the handle here means a
        // second shutdown is a no-op rather than a second wait.
        if let Ok(mut t) = handle.thread.lock() {
            let _ = t.take();
        }
    }

    fn writer(&self) -> Result<&WriterHandle, CliError> {
        if let Some(reason) = self.inner.detached {
            return Err(CliError::BadInput(format!(
                "this context cannot write: {reason}"
            )));
        }
        if let Some(h) = self.inner.writer.get() {
            return Ok(h);
        }
        let _init = self
            .inner
            .writer_init
            .lock()
            .map_err(|_| CliError::Transient("writer init lock poisoned".to_string()))?;
        if let Some(h) = self.inner.writer.get() {
            return Ok(h);
        }
        let store = SqliteStore::open(&self.inner.db_path)?;
        store.ensure_jobs_schema()?;
        let (tx, rx) = sync_channel::<WriteTask>(WRITE_QUEUE_CAPACITY);
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = alive.clone();
        let metrics = self.inner.clone();
        let thread = std::thread::Builder::new()
            .name("engram-writer".to_string())
            .spawn(move || {
                while let Ok(task) = rx.recv() {
                    metrics.metrics.queue_depth.fetch_sub(1, Ordering::Relaxed);
                    // `with_tx_res` already rolls back on unwind; this is the
                    // second layer, because a writer connection left with an
                    // open transaction turns every later write into a
                    // transaction nothing will ever commit.
                    let caught =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(&store)));
                    if caught.is_err() {
                        store.rollback_if_open();
                        tracing::error!("a write task panicked; the transaction was rolled back");
                    }
                }
                thread_alive.store(false, Ordering::Relaxed);
            })
            .map_err(|e| CliError::Transient(format!("could not start the writer thread: {e}")))?;
        self.inner
            .metrics
            .writer_started
            .store(true, Ordering::Relaxed);
        let _ = self.inner.writer.set(WriterHandle {
            tx,
            alive,
            thread: Mutex::new(Some(thread)),
        });
        self.inner
            .writer
            .get()
            .ok_or_else(|| CliError::Transient("writer handle vanished".to_string()))
    }

    /// Take a reader out of the pool by value, for work that must hold the
    /// store across `await` points (retrieval calls a provider mid-query).
    /// `SqliteStore` is `Send` but not `Sync`, so such work runs on a blocking
    /// thread with its own current-thread runtime and hands the connection
    /// back through [`Core::checkin_parts`].
    pub fn checkout_parts(&self) -> Result<(SqliteStore, bool), CliError> {
        let r = self.checkout_reader()?;
        Ok((r.store, r.pooled))
    }

    pub fn checkin_parts(&self, store: SqliteStore, pooled: bool) {
        self.return_reader(CheckedOutReader { store, pooled });
    }

    fn checkout_reader(&self) -> Result<CheckedOutReader, CliError> {
        if let Some(pool) = self.inner.readers.get() {
            if let Ok(mut idle) = pool.idle.lock() {
                if let Some(store) = idle.pop() {
                    return Ok(CheckedOutReader {
                        store,
                        pooled: true,
                    });
                }
            }
        }
        Ok(CheckedOutReader {
            store: SqliteStore::open(&self.inner.db_path)?,
            pooled: false,
        })
    }

    fn return_reader(&self, r: CheckedOutReader) {
        if !r.pooled {
            return;
        }
        if let Some(pool) = self.inner.readers.get() {
            if let Ok(mut idle) = pool.idle.lock() {
                idle.push(r.store);
            }
        }
    }
}

struct CheckedOutReader {
    store: SqliteStore,
    pooled: bool,
}

impl CoreMetrics {
    fn record<T>(&self, waited: Duration, took: Duration, out: &Result<T, CliError>) {
        self.writes_total.fetch_add(1, Ordering::Relaxed);
        if out.is_err() {
            self.writes_failed.fetch_add(1, Ordering::Relaxed);
        }
        let ms = took.as_millis() as u64;
        self.last_write_ms.store(ms, Ordering::Relaxed);
        self.last_wait_ms
            .store(waited.as_millis() as u64, Ordering::Relaxed);
        self.max_write_ms.fetch_max(ms, Ordering::Relaxed);
        self.last_write_at_ms.store(now_ms(), Ordering::Relaxed);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `cli:<diary>` is not knowable before the subcommand is parsed, so the
/// default is plain `cli` and dispatch refines it once it knows.
pub fn default_actor() -> String {
    "cli".to_string()
}

/// Per-KB counters keyed by name, used by `/health.sleep_compile`.
pub type KbCounts = HashMap<String, i64>;
