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
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .compact()
        .init();

    let args = Args::parse();
    let ctx = match AppContext::new(OutputFormat::Json, true) {
        Ok(ctx) => ctx,
        Err(err) => {
            err.emit_to_stderr();
            return ExitCode::from(err.exit_code() as u8);
        }
    };
    match serve::run(&ctx, args.host, args.port, args.token).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            err.emit_to_stderr();
            ExitCode::from(err.exit_code() as u8)
        }
    }
}
