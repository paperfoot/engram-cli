//! engram-cli — library half of the binary, makes commands testable.

pub mod agent_info;
pub mod cli;
pub mod commands;
pub mod context;
pub mod error;
pub mod output;
pub mod retrieval;

use crate::cli::{Cli, Command, ConfigCommand, EntitiesCommand};
use crate::error::CliError;

pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    let format = output::OutputFormat::detect(cli.json);
    let ctx = context::AppContext::new(format, cli.quiet)?;

    match cli.command {
        Command::AgentInfo => agent_info::run(&ctx),
        Command::Remember { content, importance, tag, diary } => {
            commands::remember::run(&ctx, content, importance, tag, diary).await
        }
        Command::Recall { query, top_k, layer, diary, since, until } => {
            commands::recall::run(&ctx, query, top_k, layer, diary, since, until).await
        }
        Command::Ingest { path, mode, diary } => {
            commands::ingest::run(&ctx, path, mode, diary).await
        }
        Command::Forget { id, confirm } => commands::forget::run(&ctx, id, confirm),
        Command::Edit { id, content, importance } => {
            commands::edit::run(&ctx, id, content, importance).await
        }
        Command::Entities(sub) => match sub {
            EntitiesCommand::List { limit, min_mentions } => {
                commands::entities::list(&ctx, limit, min_mentions)
            }
            EntitiesCommand::Show { name } => commands::entities::show(&ctx, name),
        },
        Command::Export { format } => commands::export::run(&ctx, format),
        Command::Import { file } => commands::import::run(&ctx, file),
        Command::Bench {
            suite,
            download,
            limit,
            answerer,
            judge,
            ragas,
            top_k,
            save,
        } => {
            commands::bench::run(
                &ctx, suite, download, limit, answerer, judge, ragas, top_k, save,
            )
            .await
        }
        Command::Config(sub) => match sub {
            ConfigCommand::Show => commands::config::show(&ctx),
            ConfigCommand::Set { key, value } => commands::config::set(&ctx, key, value),
            ConfigCommand::Check => commands::config::check(&ctx).await,
        },
        Command::Skill(sub) => commands::skill::run(&ctx, sub),
        Command::Update { check } => commands::update::run(&ctx, check),
    }
}
