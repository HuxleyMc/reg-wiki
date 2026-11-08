use std::collections::HashSet;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde_json::json;

use super::{Ctx, line, link_problems, print_json};
use crate::db;

pub fn run(ctx: &Ctx, input: &str) -> Result<ExitCode> {
    let page = ctx.resolve(input)?;
    let path = ctx.home.wiki.join(&page.path);
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let refs = db::refs_of(&ctx.conn, &page.slug)?;

    let ref_slugs: HashSet<&str> = refs.iter().map(|r| r.slug.as_str()).collect();
    let mut seen = HashSet::new();
    let links: Vec<db::PageRow> = db::outgoing(&ctx.conn, page.id)?
        .into_iter()
        .filter_map(|l| l.page)
        .filter(|p| p.id != page.id && !ref_slugs.contains(p.slug.as_str()))
        .filter(|p| seen.insert(p.id))
        .collect();
    let problems = link_problems(ctx, &page)?;
    let backlinks = db::backlinks(&ctx.conn, page.id)?;
    let parent = match &page.parent {
        Some(s) => db::page_by_slug(&ctx.conn, s)?,
        None => None,
    };
    ctx.event("read", Some(&page));

    if ctx.json {
        let tags = db::tags(&ctx.conn, page.id)?;
        print_json(&json!({
            "page": page, "tags": tags, "content": content, "parent": parent,
            "refs": refs, "links": links, "link_problems": problems, "backlinks": backlinks,
        }));
        return Ok(ExitCode::SUCCESS);
    }

    print!("{content}");
    if !content.ends_with('\n') {
        println!();
    }
    println!("\n── reg-wiki ── {} · hash {}", page.slug, page.hash);
    if let Some(p) = &parent {
        println!("parent:\n  {}", line(p));
    }
    section("refs", &refs);
    section("links", &links);
    for p in &problems {
        println!("  ! {p}");
    }
    section("backlinks", &backlinks);
    Ok(ExitCode::SUCCESS)
}

fn section(name: &str, pages: &[db::PageRow]) {
    if pages.is_empty() {
        return;
    }
    println!("{name}:");
    for p in pages {
        println!("  {}", line(p));
    }
}
