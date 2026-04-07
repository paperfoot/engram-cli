//! engram v2 — agent-native memory CLI.

use clap::Parser;
use engram_cli::{cli::Cli, dispatch};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let cli = Cli::parse();
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            err.emit_to_stderr();
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
