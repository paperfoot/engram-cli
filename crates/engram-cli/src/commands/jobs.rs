//! `engram jobs` — inspect long-running/inline compiler job history.

use crate::context::AppContext;
use crate::error::CliError;
use crate::output::{print_success, Metadata};
use serde_json::json;
use uuid::Uuid;

pub fn list(ctx: &AppContext, kb: String, all_kbs: bool, limit: usize) -> Result<(), CliError> {
    let jobs = ctx
        .store
        .list_compile_jobs(if all_kbs { None } else { Some(kb.as_str()) }, limit)?;
    let mut meta = Metadata::default();
    meta.add("jobs", jobs.len());
    meta.add("kb", if all_kbs { "*" } else { kb.as_str() });
    print_success(
        ctx.format,
        json!({
            "kb": if all_kbs { "*" } else { kb.as_str() },
            "jobs": jobs,
        }),
        meta,
        |data| println!("{}", serde_json::to_string_pretty(data).unwrap()),
    );
    Ok(())
}

pub fn show(ctx: &AppContext, id: String) -> Result<(), CliError> {
    let id = Uuid::parse_str(&id).map_err(|_| CliError::BadInput(format!("invalid UUID: {id}")))?;
    let Some(job) = ctx.store.get_compile_job(id)? else {
        return Err(CliError::BadInput(format!("job not found: {id}")));
    };
    let mut meta = Metadata::default();
    meta.add("job_id", id.to_string());
    meta.add("status", job.status.clone());
    print_success(ctx.format, json!({ "job": job }), meta, |data| {
        println!("{}", serde_json::to_string_pretty(data).unwrap())
    });
    Ok(())
}
