//! engram-cli — library half of the binary, makes commands testable.

pub mod agent_info;
pub mod cli;
pub mod commands;
pub mod context;
pub mod error;
pub mod output;
pub mod retrieval;

use crate::cli::{
    BudgetCommand, Cli, Command, ConfigCommand, DocumentsCommand, EntitiesCommand, FactsCommand,
    GraphCommand, JobsCommand, KbCommand, MemoryCommand,
};
use crate::error::CliError;

pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    let format = output::OutputFormat::detect(cli.json);
    let ctx = context::AppContext::new(format, cli.quiet)?;

    match cli.command {
        Command::AgentInfo => agent_info::run(&ctx),
        Command::Remember {
            content,
            importance,
            tag,
            diary,
            kb,
            no_facts,
            extract_facts,
        } => {
            commands::remember::run(
                &ctx,
                content,
                importance,
                tag,
                diary,
                kb,
                extract_facts && !no_facts,
            )
            .await
        }
        Command::Recall {
            query,
            top_k,
            layer,
            mode,
            profile,
            kb,
            all_kbs,
            diary,
            rerank_top_n,
            graph_hops,
            allow_mixed_embeddings,
            since,
            until,
        } => {
            commands::recall::run(
                &ctx,
                query,
                top_k,
                layer,
                mode,
                profile,
                kb,
                all_kbs,
                diary,
                rerank_top_n,
                graph_hops,
                allow_mixed_embeddings,
                since,
                until,
            )
            .await
        }
        Command::Ingest {
            path,
            mode,
            diary,
            kb,
            compile,
            dry_run,
            include,
            exclude,
            max_files,
            all,
        } => {
            commands::ingest::run(
                &ctx, path, mode, diary, kb, compile, dry_run, include, exclude, max_files, all,
            )
            .await
        }
        Command::Documents(sub) => match sub {
            DocumentsCommand::List { kb, all_kbs, limit } => {
                commands::documents::list(&ctx, kb, all_kbs, limit)
            }
            DocumentsCommand::Show { id } => commands::documents::show(&ctx, id),
            DocumentsCommand::Delete { id, confirm } => {
                commands::documents::delete(&ctx, id, confirm)
            }
        },
        Command::Kb(sub) => match sub {
            KbCommand::Create { name, description } => {
                commands::kb::create(&ctx, name, description)
            }
            KbCommand::List => commands::kb::list(&ctx),
            KbCommand::Show { name } => commands::kb::show(&ctx, name),
            KbCommand::Delete { name, confirm } => commands::kb::delete(&ctx, name, confirm),
        },
        Command::Compile {
            kb,
            all,
            llm,
            extraction_model,
            synthesis_model,
            max_llm_chunks,
        } => {
            commands::compile::run(
                &ctx,
                kb,
                all,
                llm,
                extraction_model,
                synthesis_model,
                max_llm_chunks,
            )
            .await
        }
        Command::Research {
            query,
            kb,
            all_kbs,
            diary,
            top_k,
            profile,
            allow_mixed_embeddings,
        } => {
            commands::research::run(
                &ctx,
                query,
                kb,
                all_kbs,
                diary,
                top_k,
                profile,
                allow_mixed_embeddings,
            )
            .await
        }
        Command::Jobs(sub) => match sub {
            JobsCommand::List { kb, all_kbs, limit } => {
                commands::jobs::list(&ctx, kb, all_kbs, limit)
            }
            JobsCommand::Show { id } => commands::jobs::show(&ctx, id),
        },
        Command::Reindex { kb, all } => commands::reindex::run(&ctx, kb, all).await,
        Command::Wiki { kb, path } => commands::wiki::run(&ctx, kb, path),
        Command::Graph(sub) => match sub {
            GraphCommand::Neighbors {
                name,
                kb,
                hops,
                min_weight,
            } => commands::graph::neighbors(&ctx, name, kb, hops, min_weight),
        },
        Command::Doctor {
            compiler,
            integrity,
        } => commands::doctor::run(&ctx, compiler, integrity),
        Command::Repair { dry_run, confirm } => commands::repair::run(&ctx, dry_run, confirm),
        Command::Memory(sub) => match sub {
            MemoryCommand::Add {
                content,
                importance,
                tag,
                diary,
                kb,
                extract_facts,
            } => {
                commands::remember::run(&ctx, content, importance, tag, diary, kb, extract_facts)
                    .await
            }
            MemoryCommand::Search {
                query,
                top_k,
                mode,
                profile,
                kb,
                all_kbs,
                diary,
            } => {
                commands::recall::run(
                    &ctx,
                    query,
                    top_k,
                    "topic".to_string(),
                    mode,
                    profile,
                    kb,
                    all_kbs,
                    diary,
                    None,
                    1,
                    false,
                    None,
                    None,
                )
                .await
            }
            MemoryCommand::Update {
                id,
                content,
                importance,
            } => commands::edit::run(&ctx, id, content, importance).await,
            MemoryCommand::Delete { id, confirm } => commands::forget::run(&ctx, id, confirm),
            MemoryCommand::List { diary, limit } => commands::memory::list(&ctx, diary, limit),
        },
        Command::Serve { host, port, token } => commands::serve::run(&ctx, host, port, token).await,
        Command::Usage { kb, since } => commands::usage::run(&ctx, kb, since),
        Command::Budget(sub) => match sub {
            BudgetCommand::Show { kb } => commands::budget::show(&ctx, kb),
            BudgetCommand::Set {
                kb,
                daily_usd,
                monthly_usd,
            } => commands::budget::set(&ctx, kb, daily_usd, monthly_usd),
            BudgetCommand::Clear { kb } => commands::budget::clear(&ctx, kb),
        },
        Command::Forget { id, confirm } => commands::forget::run(&ctx, id, confirm),
        Command::Edit {
            id,
            content,
            importance,
        } => commands::edit::run(&ctx, id, content, importance).await,
        Command::Entities(sub) => match sub {
            EntitiesCommand::List {
                limit,
                min_mentions,
                kb,
            } => commands::entities::list(&ctx, limit, min_mentions, kb),
            EntitiesCommand::Show { name, kb } => commands::entities::show(&ctx, name, kb),
        },
        Command::Facts(sub) => match sub {
            FactsCommand::List {
                subject,
                diary,
                kb,
                all,
                limit,
            } => commands::facts::list(&ctx, subject, diary, kb, all, limit),
            FactsCommand::Show { subject, diary, kb } => {
                commands::facts::show(&ctx, subject, diary, kb)
            }
            FactsCommand::Conflicts { limit } => commands::facts::conflicts(&ctx, limit),
        },
        Command::Export { format, kb } => commands::export::run(&ctx, format, kb),
        Command::Import { file } => commands::import::run(&ctx, file),
        Command::Bench {
            suite,
            mab_split,
            download,
            limit,
            answerer,
            judge,
            ragas,
            top_k,
            save,
        } => {
            commands::bench::run(
                &ctx, suite, mab_split, download, limit, answerer, judge, ragas, top_k, save,
            )
            .await
        }
        Command::Config(sub) => match sub {
            ConfigCommand::Show => commands::config::show(&ctx),
            ConfigCommand::Set { key, value } => commands::config::set(&ctx, key, value),
            ConfigCommand::Check => commands::config::check(&ctx).await,
        },
        Command::Skill(sub) => commands::skill::run(&ctx, sub),
        Command::Update { check } => commands::update::run(&ctx, check).await,
    }
}
