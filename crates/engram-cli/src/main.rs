//! engram — agent-native memory CLI.

use engram_cli::dispatch;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .compact()
        .init();

    // Not `Cli::parse()`: that exits 2 on a typo, which our own exit table
    // reads as "your setup is broken". See cli::parse_or_exit.
    let cli = engram_cli::cli::parse_or_exit();
    match dispatch(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            err.emit_to_stderr();
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
