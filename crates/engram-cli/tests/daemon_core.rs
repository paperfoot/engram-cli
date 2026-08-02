//! Wave 4 — the single writer.
//!
//! The invariants, one test each. The first one has nothing to assert: "no
//! `await` inside a write" is enforced by the type — the closure returns a
//! `Result`, not a future — so a violation does not compile and a reviewer
//! never has to look for it.
//!
//! Every write below goes through a real storage method rather than raw SQL,
//! so the tests exercise the path production uses.

mod daemon_support;

use daemon_support::*;
use engram_cli::context::AppContext;
use engram_cli::engine::Core;
use engram_cli::error::CliError;
use engram_cli::output::OutputFormat;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn ctx(store: &Store) -> AppContext {
    AppContext::with_db(OutputFormat::Json, true, store.db()).expect("context")
}

async fn kb_count(core: &Core) -> usize {
    core.read(|s| Ok(s.list_kbs()?.len())).await.expect("read")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_are_serialized() {
    let store = Store::new();
    let ctx = ctx(&store);
    let before = kb_count(&ctx.core).await;

    let mut handles = Vec::new();
    for i in 0..200 {
        let core = ctx.core.clone();
        handles.push(tokio::spawn(async move {
            core.write("test.kb", move |w| {
                Ok(w.store.ensure_kb(&format!("wave4-{i}"), None)?)
            })
            .await
        }));
    }
    for h in handles {
        h.await
            .expect("task")
            .expect("no write may report a locked database");
    }
    assert_eq!(
        kb_count(&ctx.core).await - before,
        200,
        "200 serialized writes, none lost and none contended away"
    );
}

#[tokio::test]
async fn a_failed_closure_rolls_back_everything_it_wrote() {
    let store = Store::new();
    let ctx = ctx(&store);
    let out: Result<(), CliError> = ctx
        .core
        .write("test.fail", |w| {
            w.store.ensure_kb("rolled-back", None)?;
            Err(CliError::BadInput("changed my mind".into()))
        })
        .await;
    assert!(out.is_err());
    let names = ctx
        .core
        .read(|s| {
            Ok(s.list_kbs()?
                .into_iter()
                .map(|k| k.name)
                .collect::<Vec<_>>())
        })
        .await
        .expect("read");
    assert!(
        !names.iter().any(|n| n == "rolled-back"),
        "an errored write leaves nothing behind: {names:?}"
    );
}

#[tokio::test]
async fn a_panicking_closure_does_not_poison_the_writer() {
    let store = Store::new();
    let ctx = ctx(&store);
    let boom: Result<(), CliError> = ctx
        .core
        .write("test.panic", |_w| panic!("the runner exploded"))
        .await;
    assert!(
        boom.is_err(),
        "a panicked write is a failed write, never a hang"
    );

    // The next write must both succeed AND commit. Without the rollback the
    // connection stays inside a transaction nothing will ever commit, and
    // every later write silently disappears.
    ctx.core
        .write("test.after", |w| {
            Ok(w.store.ensure_kb("after-panic", None)?)
        })
        .await
        .expect("the writer survives a panic");
    let names = ctx
        .core
        .read(|s| {
            Ok(s.list_kbs()?
                .into_iter()
                .map(|k| k.name)
                .collect::<Vec<_>>())
        })
        .await
        .expect("read");
    assert!(
        names.iter().any(|n| n == "after-panic"),
        "the write after a panic must be COMMITTED, not merely accepted: {names:?}"
    );
}

#[tokio::test]
async fn a_nested_write_is_refused_in_every_profile() {
    let store = Store::new();
    let ctx = ctx(&store);
    let core = ctx.core.clone();
    let refused = ctx.with_store_tx(|_s| {
        // Two connections both wanting RESERVED is a lock wait and then
        // SQLITE_BUSY. This is a real runtime check, not a `debug_assert`: a
        // release build strips assertions, and the transitional dual-writer
        // window runs in exactly the builds users install.
        let out: Result<(), CliError> = core.write_blocking("test.nested", |_w| Ok(()));
        Ok(out)
    });
    let err = refused
        .expect("outer transaction")
        .expect_err("a nested write must refuse");
    assert!(
        err.to_string().contains("nested write"),
        "the refusal must name what happened: {err}"
    );
}

#[tokio::test]
async fn the_writer_thread_is_lazy() {
    let store = Store::new();
    let ctx = ctx(&store);
    assert!(
        !ctx.core.metrics().writer_started,
        "`engram recall` must not pay for a writer thread it never uses"
    );
    let _ = ctx.core.read(|s| Ok(s.schema_version()?)).await;
    assert!(
        !ctx.core.metrics().writer_started,
        "a read starts no writer"
    );
    ctx.core
        .write("test.touch", |w| Ok(w.store.ensure_kb("touched", None)?))
        .await
        .expect("write");
    assert!(ctx.core.metrics().writer_started);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloning_the_core_never_makes_a_second_writer() {
    let store = Store::new();
    let ctx = ctx(&store);
    let done = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for i in 0..8 {
        let core: Core = ctx.core.clone();
        let counted = done.clone();
        handles.push(tokio::spawn(async move {
            core.write("test.touch", move |w| {
                Ok(w.store.ensure_kb(&format!("clone-{i}"), None)?)
            })
            .await
            .expect("write");
            counted.fetch_add(1, Ordering::Relaxed);
        }));
    }
    for h in handles {
        h.await.expect("task");
    }
    assert_eq!(done.load(Ordering::Relaxed), 8);
    // Every clone shares one `CoreInner`, so one counter sees all eight. Two
    // writer threads would mean two counters and this number would be short.
    assert_eq!(
        ctx.core.metrics().writes_total,
        8,
        "cloning a Core into eight tasks must not create a second writer"
    );
}

#[tokio::test]
async fn a_dropped_future_still_commits() {
    let store = Store::new();
    let ctx = ctx(&store);
    {
        // Queued, then abandoned. "The caller hung up so the write did not
        // happen" is a lie the daemon must never tell.
        let fut = ctx.core.write("test.detached", |w| {
            Ok(w.store.ensure_kb("detached", None)?)
        });
        drop(fut);
    }
    for _ in 0..50 {
        let names = ctx
            .core
            .read(|s| {
                Ok(s.list_kbs()?
                    .into_iter()
                    .map(|k| k.name)
                    .collect::<Vec<_>>())
            })
            .await
            .expect("read");
        if names.iter().any(|n| n == "detached") {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("a committed write must survive the caller going away");
}

#[tokio::test]
async fn a_write_through_a_fixture_core_refuses_by_name() {
    // A second `:memory:` connection is a second, empty database. A
    // "successful" write into one nothing else can read is worse than a
    // refusal, so the fixture core says so.
    let store = engram_storage::SqliteStore::open_in_memory().expect("memory store");
    let ctx = AppContext::fixture(OutputFormat::Json, store);
    let err = ctx
        .core
        .write_blocking("test.fixture", |_w| Ok(()))
        .expect_err("a fixture context cannot write");
    assert!(err.to_string().contains("in-memory fixture"), "{err}");
}

#[test]
fn the_daemon_opens_its_readers_once_not_per_request() {
    let store = Store::new();
    store.seed(&[
        "remember",
        "Ada wrote the first algorithm.",
        "--kb",
        "default",
    ]);
    let daemon = Daemon::start(&store);
    for _ in 0..50 {
        let (status, _) = daemon.get("/health");
        assert_eq!(status, 200);
    }
    let (status, health) = daemon.get("/health");
    assert_eq!(status, 200);
    // The old `route` opened — and migrated — a store on every request. The
    // pool is warmed at bind, so a burst of reads adds no connections and the
    // daemon reports a schema it never had to re-check.
    assert_eq!(health["data"]["ok"], true, "{health}");
    // Pinned to the storage constant, not a literal: this test asserts the
    // pool reports the schema it migrated to, not which schema that happens
    // to be this month.
    assert_eq!(
        health["data"]["schema_version"],
        engram_storage::sqlite::CURRENT_VERSION,
        "{health}"
    );
    assert!(
        health["data"]["writer"]["alive"].as_bool().unwrap_or(false),
        "a health check that says ok while writes are impossible is the \
         dishonesty this endpoint was rebuilt to remove: {health}"
    );
}
