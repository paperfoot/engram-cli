//! engram-cli — library half of the binary, makes commands testable.

pub mod agent_info;
pub mod cli;
pub mod commands;
pub mod context;
pub mod error;
pub mod fact_llm;
pub mod fact_write;
pub mod output;
pub mod read_surface;
pub mod respond;
pub mod retrieval;

use crate::cli::{
    AdminCommand, BudgetCommand, ChunksCommand, Cli, Command, ConfigCommand, DocumentsCommand,
    EntitiesCommand, FactsCommand, GraphCommand, JobsCommand, KbCommand, MemoryCommand,
    PeopleCommand,
};
use crate::error::CliError;

pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    output::mark_command_start();
    let format = output::OutputFormat::detect(cli.json);
    // The discovery surface answers BEFORE any store is opened: constructing
    // AppContext runs migrations, and "run agent-info to check the setup"
    // must never be the call that migrates (or creates) a database.
    if matches!(cli.command, Command::AgentInfo) {
        return agent_info::run();
    }
    let ctx = context::AppContext::new(format, cli.quiet)?;

    match cli.command {
        Command::AgentInfo => agent_info::run(),
        Command::Remember {
            content,
            importance,
            tag,
            diary,
            kb,
            no_extract_facts,
            extract_facts,
            event_time,
            allow_duplicate,
            autonomy,
            facts_json,
            offline,
        } => {
            commands::remember::run(
                &ctx,
                commands::remember::RememberArgs {
                    content,
                    importance,
                    tags: tag,
                    diary,
                    kb,
                    no_extract_facts,
                    force_extract_facts: extract_facts,
                    allow_duplicate,
                    autonomy,
                    facts_json,
                    event_time,
                    offline,
                },
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
                commands::recall::RecallArgs {
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
                    alias: "recall",
                },
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
            DocumentsCommand::List {
                kb,
                all_kbs,
                limit,
                cursor,
            } => commands::documents::list(&ctx, kb, all_kbs, limit, cursor),
            DocumentsCommand::Show { id } => commands::documents::show(&ctx, id),
            DocumentsCommand::Delete { id, confirm } => {
                commands::documents::delete(&ctx, id, confirm)
            }
        },
        Command::Admin(sub) => match sub {
            AdminCommand::Rebuild {
                kb,
                dry_run,
                confirm,
            } => commands::admin::rebuild(&ctx, kb, dry_run, confirm),
        },
        Command::Kb(sub) => match sub {
            KbCommand::Create { name, description } => {
                commands::kb::create(&ctx, name, description)
            }
            KbCommand::List => commands::kb::list(&ctx),
            KbCommand::Show { name } => commands::kb::show(&ctx, name),
            KbCommand::Delete { name, confirm } => commands::kb::delete(&ctx, name, confirm),
            KbCommand::Repair { dry_run } => commands::kb::repair(&ctx, dry_run),
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
        Command::State {
            subject,
            kb,
            diary,
            relation,
            as_of,
            include_closed,
            limit,
            cursor,
        } => commands::state::run(
            &ctx,
            commands::state::StateArgs {
                subject,
                kb,
                diary,
                relation,
                as_of,
                include_closed,
                limit,
                cursor,
            },
        ),
        Command::Reindex { kb, all, offline } => {
            commands::reindex::run(&ctx, kb, all, offline).await
        }
        Command::Wiki { kb, path } => commands::wiki::run(&ctx, kb, path),
        Command::Graph(sub) => match sub {
            GraphCommand::Neighbors {
                name,
                kb,
                hops,
                min_weight,
                limit,
                typed_only,
            } => commands::graph::neighbors(&ctx, name, kb, hops, min_weight, limit, typed_only),
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
                no_extract_facts,
                extract_facts,
                event_time,
                autonomy,
                facts_json,
                allow_duplicate,
                offline,
            } => {
                commands::remember::run(
                    &ctx,
                    commands::remember::RememberArgs {
                        content,
                        importance,
                        tags: tag,
                        diary,
                        kb,
                        no_extract_facts,
                        force_extract_facts: extract_facts,
                        allow_duplicate,
                        autonomy,
                        facts_json,
                        event_time,
                        offline,
                    },
                )
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
                since,
                until,
            } => {
                // One code path with `recall`, one envelope, one set of
                // fields. The defaults that still differ (layer, rerank depth,
                // graph fan-out) are preserved deliberately and are now
                // visible in one place instead of hiding in this arm.
                commands::recall::run(
                    &ctx,
                    commands::recall::RecallArgs {
                        query,
                        top_k,
                        layer: "topic".to_string(),
                        mode,
                        profile,
                        kb,
                        all_kbs,
                        diary,
                        rerank_top_n: None,
                        graph_hops: 1,
                        allow_mixed_embeddings: false,
                        since,
                        until,
                        alias: "memory search",
                    },
                )
                .await
            }
            MemoryCommand::Update {
                id,
                content,
                importance,
                offline,
            } => commands::edit::run(&ctx, id, content, importance, offline).await,
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
            offline,
        } => commands::edit::run(&ctx, id, content, importance, offline).await,
        Command::Entities(sub) => match sub {
            EntitiesCommand::List {
                limit,
                min_mentions,
                kb,
                cursor,
            } => commands::entities::list(&ctx, limit, min_mentions, kb, cursor),
            EntitiesCommand::Show { name, kb } => commands::entities::show(&ctx, name, kb),
        },
        Command::People(sub) => match sub {
            PeopleCommand::List {
                kb,
                limit,
                min_mentions,
                cursor,
            } => commands::people::list(&ctx, kb, limit, min_mentions, cursor),
            PeopleCommand::Show { name, kb, snippets } => {
                commands::people::show(&ctx, name, kb, snippets)
            }
        },
        Command::Facts(sub) => match sub {
            FactsCommand::List {
                subject,
                diary,
                kb,
                all,
                limit,
                cursor,
            } => commands::facts::list(&ctx, subject, diary, kb, all, limit, cursor),
            FactsCommand::Show { subject, diary, kb } => {
                commands::facts::show(&ctx, subject, diary, kb)
            }
            FactsCommand::Conflicts {
                limit,
                open,
                diary,
                kb,
                subject,
                cursor,
            } => commands::facts::conflicts(&ctx, limit, open, diary, kb, subject, cursor),
            FactsCommand::Triage {
                kb,
                diary,
                subject,
                limit,
                dry_run,
            } => commands::facts::triage(&ctx, kb, diary, subject, limit, dry_run),
            FactsCommand::Resolve { id, action, note } => {
                commands::facts::resolve(&ctx, id, action, note)
            }
            FactsCommand::Backfill {
                kb,
                diary,
                limit,
                dry_run,
            } => commands::facts::backfill(&ctx, kb, diary, limit, dry_run).await,
        },
        Command::Export { format, kb } => commands::export::run(&ctx, format, kb),
        Command::Import { file } => commands::import::run(&ctx, file),
        Command::Bench {
            suite,
            profile,
            mab_split,
            download,
            limit,
            answerer,
            judge,
            ragas,
            top_k,
            save,
            mode,
            kb,
            corpus,
            dataset,
            rerank,
            year,
        } => {
            commands::bench::run(
                &ctx,
                commands::bench::BenchArgs {
                    suite,
                    profile,
                    mab_split,
                    download,
                    limit,
                    answerer,
                    judge,
                    ragas,
                    top_k,
                    save,
                    mode,
                    kb,
                    corpus,
                    dataset,
                    rerank,
                    year,
                },
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
        Command::Chunks(sub) => match sub {
            ChunksCommand::Repair {
                kb,
                all_kbs,
                dry_run,
            } => commands::chunks::repair(&ctx, kb, all_kbs, dry_run).await,
        },
    }
}
