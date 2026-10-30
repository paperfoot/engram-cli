//! engram-cli — library half of the binary, makes commands testable.

// `agent_info`'s manifest is one `json!` literal and every command adds a
// level of macro recursion to it. The default limit of 128 is reached at the
// size the manifest already is, so a new command otherwise fails to compile
// for a reason that names neither the command nor the manifest.
#![recursion_limit = "512"]

pub mod agent_info;
pub mod cli;
pub mod commands;
pub mod context;
pub mod engine;
pub mod error;
pub mod fact_llm;
pub mod fact_write;
pub mod judge;
pub mod output;
pub mod read_surface;
pub mod respond;
pub mod retrieval;

use crate::cli::{
    AdminCommand, BudgetCommand, ChunksCommand, Cli, Command, ConfigCommand, DocumentsCommand,
    FactsCommand, JobsCommand, KbCommand, LessonsCommand,
    MemoryCommand, PeopleCommand,
};
use crate::error::CliError;

pub async fn dispatch(cli: Cli) -> Result<(), CliError> {
    // FIRST — before any path is resolved, before mark_command_start, and
    // before the AgentInfo early return below, so `engram --store /tmp/x
    // agent-info` reports /tmp/x's paths rather than the user's real ones.
    apply_store_override(cli.store.as_deref())?;
    output::mark_command_start();
    let format = output::OutputFormat::detect(cli.json);
    // Store-free commands answer BEFORE any store is opened: constructing
    // AppContext runs migrations, and "run agent-info to check the setup"
    // must never be the call that migrates (or creates) a database. A
    // benchmark measures fixtures and datasets, not the user's memory, so it
    // must not be that call either; the one suite that reads a KB opens the
    // store itself.
    let command = match cli.command {
        Command::AgentInfo {
            schema,
            command,
            index,
        } => {
            let view = match (schema, command) {
                (Some(path), _) => agent_info::View::Schema(path),
                (None, Some(path)) => agent_info::View::Command(path),
                (None, None) if index => agent_info::View::Index,
                (None, None) => agent_info::View::Full,
            };
            return agent_info::run(view, format);
        }
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
            return commands::bench::run(
                format,
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
        command => command,
    };
    let ctx = context::AppContext::new(format, cli.quiet)?;
    ctx.core.set_actor(actor_for(&command));

    match command {
        Command::AgentInfo { .. } | Command::Bench { .. } => {
            unreachable!("store-free commands return before the store opens")
        }
        Command::Remember {
            content,
            importance,
            tag,
            diary,
            kb,
            no_extract_facts,
            extract_facts,
            event_time,
            valid_until,
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
                    valid_until,
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
            allow_mixed_embeddings,
            since,
            until,
            include_expired,
            full,
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
                    allow_mixed_embeddings,
                    since,
                    until,
                    include_expired,
                    full,
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
            dry_run,
            include,
            exclude,
            max_files,
            all,
            offline,
        } => {
            commands::ingest::run(
                &ctx, path, mode, diary, kb, dry_run, include, exclude, max_files, all,
                offline,
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
        Command::Jobs(sub) => match sub {
            JobsCommand::List {
                kb,
                all_kbs,
                limit,
                kind,
                status,
            } => commands::jobs::list(&ctx, kb, all_kbs, limit, kind, status),
            JobsCommand::Show { id } => commands::jobs::show(&ctx, id),
            JobsCommand::Submit {
                kind,
                kb,
                params,
                detach,
                wait,
            } => commands::jobs::submit(&ctx, kind, kb, params, detach, wait).await,
            JobsCommand::Cancel { id } => commands::jobs::cancel(&ctx, id).await,
            JobsCommand::Resume { id, force } => commands::jobs::resume(&ctx, id, force).await,
            JobsCommand::Watch { id } => commands::jobs::watch(&ctx, id).await,
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
        Command::Lessons(sub) => match sub {
            LessonsCommand::Add {
                lesson,
                trigger,
                kb,
                diary,
                evidence,
                confidence,
                offline,
            } => {
                commands::lessons::add(
                    &ctx,
                    commands::lessons::AddArgs {
                        lesson,
                        trigger,
                        kb,
                        diary,
                        evidence,
                        confidence,
                        offline,
                    },
                )
                .await
            }
            LessonsCommand::List {
                kb,
                trigger,
                status,
                limit,
                cursor,
            } => commands::lessons::list(
                &ctx,
                commands::lessons::ListArgs {
                    kb,
                    trigger,
                    status,
                    limit,
                    cursor,
                },
            ),
            LessonsCommand::Confirm { id, note, offline } => {
                commands::lessons::confirm(&ctx, id, note, offline).await
            }
            LessonsCommand::Retract { id, reason } => commands::lessons::retract(&ctx, id, reason),
        },
        Command::Reflect {
            kb,
            since,
            window,
            dry_run,
        } => commands::reflect::run(
            &ctx,
            commands::reflect::ReflectArgs {
                kb,
                since,
                window,
                dry_run,
            },
        ),
        Command::Reindex { kb, all, offline } => {
            commands::reindex::run(&ctx, kb, all, offline).await
        }
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
                valid_until,
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
                        valid_until,
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
                include_expired,
                full,
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
                        allow_mixed_embeddings: false,
                        since,
                        until,
                        include_expired,
                        full,
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
                commands::facts::resolve(&ctx, id, action, note).await
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
                offline,
            } => commands::chunks::repair(&ctx, kb, all_kbs, dry_run, offline).await,
        },
    }
}

/// `--store <dir>` > `ENGRAM_STORE` > ProjectDirs (interfaces §5).
///
/// Implemented by exporting the flag's value into `ENGRAM_STORE`, because
/// `paths::store_override()` reads the environment on every call and because
/// the export is load-bearing on its own: `POST /v1/ingest` re-enters the CLI
/// as a child process, and without it a `--store`-isolated daemon would ingest
/// into the user's real store.
///
/// It creates NOTHING. The path is made absolute without touching the
/// filesystem (`canonicalize` would require it to exist), and directory
/// creation stays in `paths::ensure()`, which runs on the first real path
/// resolution — so `engram --store X agent-info` still leaves X non-existent.
/// A discovery command that materialises a store directory is the same class
/// of bug as one that migrates a database.
pub fn apply_store_override(dir: Option<&std::path::Path>) -> Result<(), CliError> {
    let Some(dir) = dir else { return Ok(()) };
    if dir.as_os_str().is_empty() {
        return Err(CliError::Config(
            "--store needs a directory; an empty value is not the same as leaving it unset".into(),
        ));
    }
    if dir.exists() && !dir.is_dir() {
        return Err(CliError::Config(format!(
            "--store {} exists and is not a directory",
            dir.display()
        )));
    }
    let absolute = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(dir)
    };
    // One call, at process start, before this lane spawns any thread or task.
    std::env::set_var("ENGRAM_STORE", &absolute);
    Ok(())
}

/// `cli:<diary>` where the subcommand carries a diary, plain `cli` otherwise.
/// The journal's `actor` column is how a rebuild says who did what, and
/// "someone using the CLI" is a worse answer than the diary they were in.
fn actor_for(command: &Command) -> String {
    let diary = match command {
        Command::Remember { diary, .. } => Some(diary.clone()),
        Command::Recall { diary, .. } => Some(diary.clone()),
        _ => None,
    };
    match diary {
        Some(d) if !d.is_empty() && d != "*" => format!("cli:{d}"),
        _ => "cli".to_string(),
    }
}
