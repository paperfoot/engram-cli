//! Binary alias for `engram serve`.

use clap::Parser;
use engram_cli::commands::serve;
use engram_cli::context::AppContext;
use engram_cli::output::OutputFormat;
use std::process::ExitCode;

#[derive(Parser, Debug)]
#[command(name = "engramd", version, about = "Local engram daemon/API")]
struct Args {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 8768)]
    port: u16,
    /// Auth token required when binding to non-local addresses.
    #[arg(long)]
    token: Option<String>,
    /// Root every path (db, vectors, config, cache) at DIR. Overrides ENGRAM_STORE.
    ///
    /// Applied before `AppContext::new`, so `AppContext::new` is still what
    /// runs. Deliberately NOT `AppContext::with_db`: that overrides the db path
    /// only, while vectors, config and cache keep resolving through `paths::`,
    /// which would split the daemon across two stores.
    #[arg(long, value_name = "DIR")]
    store: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let args = Args::parse();
    if let Err(err) = engram_cli::apply_store_override(args.store.as_deref()) {
        err.emit_to_stderr();
        return ExitCode::from(err.exit_code() as u8);
    }
    let ctx = match AppContext::new(OutputFormat::Json, true) {
        Ok(ctx) => ctx,
        Err(err) => {
            err.emit_to_stderr();
            return ExitCode::from(err.exit_code() as u8);
        }
    };
    // A signal is a shutdown, not a kill: the descriptor file is removed and
    // the writer drains, so the next start does not have to reconcile rows the
    // daemon could have finished.
    let core = ctx.core.clone();
    tokio::select! {
        outcome = serve::run(&ctx, args.host, args.port, args.token) => match outcome {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                err.emit_to_stderr();
                ExitCode::from(err.exit_code() as u8)
            }
        },
        _ = shutdown_signal() => {
            serve::remove_descriptor();
            core.shutdown(std::time::Duration::from_secs(5)).await;
            ExitCode::SUCCESS
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return std::future::pending().await,
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
