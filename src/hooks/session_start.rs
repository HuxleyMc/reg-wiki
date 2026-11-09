//! SessionStart: the current project's pages, one line each, hubs first, capped at about
//! 1.5k tokens. Also ties the session to wiki writes through `CLAUDE_ENV_FILE`.

use std::collections::{HashMap, HashSet};
use std::io::Write;

use anyhow::Result;

use super::{Input, Output, SESSION_ENV, state};
use crate::cmd::{Ctx, line};
use crate::db::{self, PageRow};
use crate::search::tokens;
use crate::util;

const DEFAULT_TOKENS: usize = 1500;

pub fn run(ctx: &Ctx, input: &Input) -> Result<Option<Output>> {
    if let Some(id) = &input.session_id {
        let tagged = tag_session(id);
        let _ = state::update(&ctx.home, id, |s| {
            s.started.get_or_insert_with(util::now);
            s.tagged |= tagged;
        });
        state::prune(&ctx.home);
    }
    let budget = ctx
        .cfg
        .int("limits", "session_start_tokens")
        .map_or(DEFAULT_TOKENS, |n| n.max(100) as usize);
    Ok(index(ctx, budget)?.map(Output::Context))
}

/// Export `REG_SESSION` into the shell the agent's commands run in.
fn tag_session(id: &str) -> bool {
    let Some(file) = std::env::var_os("CLAUDE_ENV_FILE") else {
        return false;
    };
    let safe = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !safe || id.is_empty() {
        return false;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut f| writeln!(f, "export {SESSION_ENV}={id}"))
        .is_ok()
}

/// The index text, or `None` when there's nothing worth showing.
pub fn index(ctx: &Ctx, budget: usize) -> Result<Option<String>> {
    let current = ctx.current.as_deref();
    // Outside a project, only shared pages: other projects' pages aren't relevant here.
    let pages: Vec<PageRow> = db::pages_in_scope(&ctx.conn, ctx.scope())?
        .into_iter()
        .filter(|p| !p.is_ref && (current.is_some() || p.project.is_none()))
        .collect();
    let (own, shared): (Vec<&PageRow>, Vec<&PageRow>) = pages
        .iter()
        .partition(|p| p.project.is_some() && p.project.as_deref() == current);
    if current.is_none() && pages.is_empty() {
        return Ok(None);
    }
    if current.is_some() && own.is_empty() && shared.is_empty() {
        return Ok(None);
    }

    let hubs = hub_ids(ctx)?;
    let inbound = inbound_counts(ctx)?;
    let is_hub = |p: &PageRow| !p.slug.contains('/') || hubs.contains(&p.id);
    let mut order: Vec<&PageRow> = pages.iter().collect();
    // Hubs first (this project's, then others'), then this project's pages, then the
    // rest; most linked-to first within each group.
    order.sort_by_key(|p| {
        let mine = p.project.is_some() && p.project.as_deref() == current;
        (
            !is_hub(p),
            !mine,
            std::cmp::Reverse(inbound.get(&p.id).copied().unwrap_or(0)),
            p.slug.to_lowercase(),
        )
    });

    let mut out = match current {
        Some(name) => {
            let mut s = format!(
                "reg-wiki: the wiki has {} page(s) for project `{name}` and {} shared or \
                 associated. Each line is `slug — when to read it`. No need to read \
                 any up front: open one (`reg-wiki read <slug>`) or ask \
                 `reg-wiki context \"<question>\"` when you need it, e.g. to answer a \
                 question, when stuck on a problem, or to pick up earlier work. When you \
                 learn something worth keeping, record it with `reg-wiki write` or \
                 `reg-wiki new` (see the reg-wiki skill).\n",
                own.len(),
                shared.len()
            );
            if own.iter().all(|p| is_hub(p)) {
                s.push_str(
                    "Nothing is recorded about this project beyond its hub yet: as you learn \
                     how it works, write it down.\n",
                );
            }
            s
        }
        None => "reg-wiki: this directory isn't a registered wiki project. Shared pages \
                 (`reg-wiki read <slug>` to open one):\n"
            .to_string(),
    };
    out.push('\n');
    let mut used = tokens(&out);
    for (i, p) in order.iter().enumerate() {
        let l = format!("{}\n", line(p));
        let cost = tokens(&l);
        // Keep room for the "more" line.
        if used + cost + 15 > budget {
            out.push_str(&format!(
                "… and {} more: `reg-wiki list` shows them all\n",
                order.len() - i
            ));
            break;
        }
        used += cost;
        out.push_str(&l);
    }
    Ok(Some(out))
}

fn hub_ids(ctx: &Ctx) -> Result<HashSet<i64>> {
    let mut st = ctx
        .conn
        .prepare("SELECT page_id FROM tags WHERE tag = 'hub'")?;
    let ids = st
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<HashSet<i64>>>()?;
    Ok(ids)
}

fn inbound_counts(ctx: &Ctx) -> Result<HashMap<i64, i64>> {
    let mut st = ctx.conn.prepare(
        "SELECT dst_id, count(DISTINCT src_id) FROM links
         WHERE dst_id IS NOT NULL AND dst_id != src_id GROUP BY dst_id",
    )?;
    let m = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<HashMap<i64, i64>>>()?;
    Ok(m)
}
