use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;

use super::{Ctx, line, print_json};
use crate::search::{self, Query};

const DEFAULT_BUDGET: usize = 4000;

pub fn query(ctx: &Ctx, text: &str, limit: usize, tags: &[String]) -> Result<ExitCode> {
    let q = Query {
        text,
        scope: ctx.scope(),
        tags,
        limit: limit.max(1),
    };
    let (hits, strict) = search::query(&ctx.conn, &q)?;
    ctx.query_event(text, hits.len());

    if ctx.json {
        print_json(&json!({ "query": text, "matched_all_words": strict, "hits": hits }));
        return Ok(ExitCode::SUCCESS);
    }
    if hits.is_empty() {
        eprintln!(
            "no pages match `{text}`; if you work out the answer, record it with `reg-wiki new`"
        );
        return Ok(ExitCode::SUCCESS);
    }
    if !strict {
        eprintln!("no page matches every word; showing pages that match some of them");
    }
    for h in &hits {
        println!("{}", line(&h.page));
        if !h.snippet.is_empty() {
            println!("    {}", h.snippet);
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn context(
    ctx: &Ctx,
    question: &str,
    budget: Option<&str>,
    candidates: usize,
) -> Result<ExitCode> {
    let budget = match budget {
        Some(b) => search::parse_budget(b)?,
        None => ctx
            .cfg
            .int("limits", "context_budget")
            .map_or(DEFAULT_BUDGET, |n| n.max(1) as usize),
    };
    let q = Query {
        text: question,
        scope: ctx.scope(),
        tags: &[],
        limit: candidates.max(1),
    };
    let (hits, _) = search::query(&ctx.conn, &q)?;
    ctx.query_event(question, hits.len());
    let c = search::context(&ctx.conn, &ctx.home.wiki, hits, budget)?;
    for p in &c.pages {
        if let Ok(Some(row)) = crate::db::page_by_slug(&ctx.conn, &p.slug) {
            ctx.event("read", Some(&row));
        }
    }

    if ctx.json {
        print_json(&json!({ "question": question, "context": c }));
        return Ok(ExitCode::SUCCESS);
    }
    if c.pages.is_empty() && c.related.is_empty() {
        eprintln!(
            "no pages match `{question}`; if you work out the answer, record it with `reg-wiki new`"
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "# Wiki context: {question} (~{} of {} tokens)\n",
        c.used, c.budget
    );
    for p in &c.pages {
        println!("{}", search::section(p));
    }
    if !c.related.is_empty() {
        println!("## Related (summaries; `reg-wiki read <slug>` for more)");
        for r in &c.related {
            print!("{}", search::related_line(r));
        }
    }
    Ok(ExitCode::SUCCESS)
}
