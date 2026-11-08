use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;

use super::{Ctx, print_json};
use crate::{git, lint};

pub fn run(ctx: &Ctx) -> Result<ExitCode> {
    let (pages, refs): (i64, i64) = ctx.conn.query_row(
        "SELECT count(*), coalesce(sum(is_ref), 0) FROM pages",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let project = ctx
        .current
        .as_ref()
        .and_then(|n| ctx.cfg.projects().into_iter().find(|p| &p.name == n));
    let pending = git::pending(&ctx.home.wiki)?;
    let issues = ctx.lint()?;
    let (errors, warnings) = lint::counts(&issues);
    let s = &ctx.stats;

    if ctx.json {
        print_json(&json!({
            "wiki": ctx.home.wiki, "pages": pages, "refs": refs,
            "project": ctx.current, "project_config": project,
            "reconciled": s, "pending": pending,
            "lint": { "errors": errors, "warnings": warnings },
        }));
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "wiki     {} ({pages} pages, {refs} of them refs)",
        ctx.home.wiki.display()
    );
    match (&ctx.current, &project) {
        (Some(n), Some(p)) => {
            let id = p.remote.as_deref().or(p.root.as_deref()).unwrap_or("");
            println!("project  {n} ({id})");
        }
        (Some(n), None) => println!("project  {n}"),
        (None, _) => println!("project  none here; commands cover every project"),
    }
    let changes = s.added + s.updated + s.removed;
    if changes == 0 {
        println!("index    up to date");
    } else {
        println!(
            "index    up to date; just picked up {} added, {} updated, {} removed",
            s.added, s.updated, s.removed
        );
    }
    if pending.is_empty() {
        println!("pending  nothing uncommitted");
    } else {
        println!(
            "pending  {} uncommitted change(s) in the wiki (hand edits?):",
            pending.len()
        );
        for p in pending.iter().take(10) {
            println!("           {p}");
        }
        println!(
            "         commit them with `git -C {} commit -am \"…\"`",
            ctx.home.wiki.display()
        );
    }
    if errors + warnings == 0 {
        println!("lint     no problems");
    } else {
        println!("lint     {errors} error(s), {warnings} warning(s); see `reg-wiki lint`");
    }
    Ok(ExitCode::SUCCESS)
}
