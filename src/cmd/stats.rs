use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::Result;
use rusqlite::params_from_iter;
use serde::Serialize;
use serde_json::json;

use super::{Ctx, print_json};
use crate::db;
use crate::home::SHARED;

#[derive(Serialize)]
struct Count {
    name: String,
    count: i64,
}

#[derive(Serialize)]
struct Month {
    month: String,
    added: i64,
    total: i64,
}

pub fn run(ctx: &Ctx) -> Result<ExitCode> {
    let scope = ctx.scope();
    let (cond, args): (String, Vec<String>) = match scope {
        Some(p) => (db::scope_sql(1), vec![p.to_string()]),
        None => ("1=1".into(), vec![]),
    };
    let one = |sql: &str| -> Result<Vec<i64>> {
        let mut st = ctx.conn.prepare(sql)?;
        let n = st.column_count();
        Ok(st.query_row(params_from_iter(&args), |r| {
            (0..n)
                .map(|i| r.get::<_, Option<i64>>(i).map(|v| v.unwrap_or(0)))
                .collect()
        })?)
    };
    let counts = |sql: &str| -> Result<Vec<Count>> {
        let mut st = ctx.conn.prepare(sql)?;
        let rows = st
            .query_map(params_from_iter(&args), |r| {
                Ok(Count {
                    name: r.get(0)?,
                    count: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let from = "pages p LEFT JOIN projects pr ON pr.id = p.project_id";
    // Joins that go through links or events attach the page first, then its project.
    let via = |on: &str| format!("pages p ON {on} LEFT JOIN projects pr ON pr.id = p.project_id");

    let t = one(&format!(
        "SELECT count(*), sum(p.is_ref), sum(p.words), sum(p.chars), sum(p.bytes) FROM {from} WHERE {cond}"
    ))?;
    let l = one(&format!(
        "SELECT count(*), sum(l.resolution != 'ok') FROM links l JOIN {} WHERE {cond}",
        via("p.id = l.src_id")
    ))?;
    let most_linked = counts(&format!(
        "SELECT p.slug, count(DISTINCT l.src_id) n FROM links l JOIN {}
         WHERE l.src_id != l.dst_id AND {cond} GROUP BY p.id ORDER BY n DESC, p.slug LIMIT 5",
        via("p.id = l.dst_id")
    ))?;
    let most_read = counts(&format!(
        "SELECT p.slug, count(*) n FROM events e JOIN {}
         WHERE e.kind = 'read' AND {cond} GROUP BY p.id ORDER BY n DESC, p.slug LIMIT 5",
        via("p.slug = e.slug")
    ))?;
    // Searches that found nothing are the clearest sign of what the wiki is missing.
    let gap_cond = match scope {
        Some(_) => "AND e.project_id = (SELECT id FROM projects WHERE name = ?1)",
        None => "",
    };
    let gaps = counts(&format!(
        "SELECT min(e.query), count(*) n FROM events e
         WHERE e.kind = 'query' AND e.hits = 0 {gap_cond}
         GROUP BY lower(e.query) ORDER BY n DESC, max(e.ts) DESC LIMIT 10"
    ))?;
    // Parts of questions the librarian found no answer to.
    let ai_gaps = counts(&format!(
        "SELECT min(e.query), count(*) n FROM events e
         WHERE e.kind = 'gap' {gap_cond}
         GROUP BY lower(e.query) ORDER BY n DESC, max(e.ts) DESC LIMIT 10"
    ))?;
    let months = counts(&format!(
        "SELECT substr(p.created, 1, 7) m, count(*) FROM {from}
         WHERE p.created IS NOT NULL AND {cond} GROUP BY m ORDER BY m"
    ))?;
    let mut total = 0;
    let growth: Vec<Month> = months
        .into_iter()
        .map(|c| {
            total += c.count;
            Month {
                month: c.name,
                added: c.count,
                total,
            }
        })
        .collect();

    let paths: Vec<&str> = match scope {
        Some(p) => vec![p, SHARED],
        None => vec![],
    };
    let mut writers: BTreeMap<String, i64> = BTreeMap::new();
    for a in crate::git::authors_since(&ctx.home.wiki, "30.days", &paths)? {
        *writers.entry(a).or_default() += 1;
    }
    let mut writers: Vec<Count> = writers
        .into_iter()
        .map(|(name, count)| Count { name, count })
        .collect();
    writers.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));

    let issues = ctx.lint()?;
    let kind = |k: &str| issues.iter().filter(|i| i.kind == k).count();
    let (orphans, stale) = (kind("orphan"), kind("stale"));
    let scope_name = scope.map_or("all projects".to_string(), |p| format!("{p} + {SHARED}"));

    if ctx.json {
        print_json(&json!({
            "scope": scope, "pages": t[0], "refs": t[1], "words": t[2], "chars": t[3], "bytes": t[4],
            "links": l[0], "broken_links": l[1], "orphans": orphans, "stale": stale,
            "most_linked": most_linked, "most_read": most_read, "writes_30d": writers,
            "queries_without_results": gaps, "librarian_gaps": ai_gaps, "growth": growth,
        }));
        return Ok(ExitCode::SUCCESS);
    }

    println!("{scope_name}");
    println!(
        "  pages     {} ({} refs) · {} words · {} chars · {} bytes",
        t[0], t[1], t[2], t[3], t[4]
    );
    println!("  links     {} ({} broken or ambiguous)", l[0], l[1]);
    println!("  health    {orphans} orphan(s) · {stale} stale");
    list("most linked", &most_linked, |c| {
        format!("{} ({})", c.name, c.count)
    });
    list("most read", &most_read, |c| {
        format!("{} ({})", c.name, c.count)
    });
    list("writes, 30 days", &writers, |c| {
        format!("{} ({})", c.name, c.count)
    });
    list("searches with no results", &gaps, |c| {
        format!("\"{}\" ×{}", c.name, c.count)
    });
    list("not in the wiki, per the librarian", &ai_gaps, |c| {
        format!("\"{}\" ×{}", c.name, c.count)
    });
    if !growth.is_empty() {
        println!("  growth");
        for m in &growth {
            println!("    {}  +{:<4} {}", m.month, m.added, m.total);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn list<T>(title: &str, items: &[T], f: impl Fn(&T) -> String) {
    if items.is_empty() {
        return;
    }
    println!("  {title}");
    for i in items {
        println!("    {}", f(i));
    }
}
