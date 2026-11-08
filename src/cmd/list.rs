use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;

use super::{Ctx, line, print_json};
use crate::db;

pub fn run(ctx: &Ctx, tags: &[String], refs: bool) -> Result<ExitCode> {
    let mut sql = format!("SELECT {} FROM {} WHERE 1=1", db::PAGE_COLS, db::PAGE_FROM);
    let mut args: Vec<String> = Vec::new();
    if !refs {
        sql.push_str(" AND p.is_ref = 0");
    }
    if let Some(proj) = ctx.scope() {
        args.push(proj.to_string());
        let n = args.len();
        sql.push_str(&format!(" AND {}", db::scope_sql(n)));
    }
    for t in tags {
        args.push(t.clone());
        sql.push_str(&format!(
            " AND p.id IN (SELECT page_id FROM tags WHERE tag = ?{})",
            args.len()
        ));
    }
    // The current project's pages first (its hub sorts first), then shared pages.
    sql.push_str(" ORDER BY p.project_id IS NULL, pr.name, p.slug");

    let mut st = ctx.conn.prepare(&sql)?;
    let pages = st
        .query_map(rusqlite::params_from_iter(&args), db::page_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if ctx.json {
        let items: Vec<_> = pages
            .iter()
            .map(|p| {
                let tags = db::tags(&ctx.conn, p.id).unwrap_or_default();
                json!({ "slug": p.slug, "title": p.title, "summary": p.summary,
                        "project": p.project, "is_ref": p.is_ref, "tags": tags })
            })
            .collect();
        print_json(&json!(items));
    } else if pages.is_empty() {
        eprintln!("no pages yet; create one with `reg-wiki new <slug> --title … --summary …`");
    } else {
        for p in &pages {
            println!("{}", line(p));
        }
    }
    Ok(ExitCode::SUCCESS)
}
