use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;

use super::{Ctx, line, link_problems, print_json};
use crate::db;

pub fn links(ctx: &Ctx, input: &str) -> Result<ExitCode> {
    let page = ctx.resolve(input)?;
    let links = db::outgoing(&ctx.conn, page.id)?;
    if ctx.json {
        print_json(&json!({ "slug": page.slug, "links": links }));
        return Ok(ExitCode::SUCCESS);
    }
    let mut seen = std::collections::HashSet::new();
    for l in &links {
        if let Some(p) = &l.page
            && seen.insert(p.id)
        {
            let heading = l
                .heading
                .as_deref()
                .map(|h| format!(" #{h}"))
                .unwrap_or_default();
            println!("{}{heading}", line(p));
        }
    }
    for p in link_problems(ctx, &page)? {
        println!("! {p}");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn backlinks(ctx: &Ctx, input: &str) -> Result<ExitCode> {
    let page = ctx.resolve(input)?;
    let back = db::backlinks(&ctx.conn, page.id)?;
    if ctx.json {
        print_json(&json!({ "slug": page.slug, "backlinks": back }));
    } else if back.is_empty() {
        eprintln!("nothing links to {}", page.slug);
    } else {
        for p in &back {
            println!("{}", line(p));
        }
    }
    Ok(ExitCode::SUCCESS)
}
