//! `mv`: rename a page with everything under it, and rewrite links so they keep
//! pointing at the same pages.
//!
//! The approach: work out every file's path after the move, build a resolver for the
//! old layout and one for the new, then for each link in the wiki compare what it means
//! now with what it would mean after. Only links whose meaning would change are
//! rewritten, using the shortest form that resolves correctly from the linking page.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use super::{Ctx, print_json, warn};
use crate::db;
use crate::links::{self, Kind, Resolution, Resolver};
use crate::page::{self, Page};
use crate::util;

pub fn run(ctx: &mut Ctx, old: &str, new: &str, message: Option<&str>) -> Result<ExitCode> {
    let _lock = ctx.home.lock()?;
    ctx.sync()?;
    let wiki = ctx.home.wiki.clone();

    let src = ctx.resolve(old)?;
    let new_slug = ctx.qualify(new);
    page::check_slug(&new_slug)?;
    if !src.slug.contains('/') || !new_slug.contains('/') {
        bail!("project hubs can't be moved, and pages can't become one");
    }
    if new_slug.eq_ignore_ascii_case(&src.slug) {
        bail!("`{}` is already called that", src.slug);
    }
    if new_slug
        .to_lowercase()
        .starts_with(&format!("{}/", src.slug.to_lowercase()))
    {
        bail!("can't move `{}` inside itself", src.slug);
    }
    if let Some(p) = db::page_by_slug(&ctx.conn, &new_slug)? {
        bail!("`{}` already exists ({})", p.slug, p.path);
    }

    // Filesystem moves, as (from, to) paths relative to the wiki.
    let mut moves: Vec<(String, String)> = Vec::new();
    if src.is_ref {
        // A ref stays a ref if its new parent is a main page; otherwise it becomes one.
        let (parent, base) = new_slug.rsplit_once('/').unwrap_or(("", &new_slug));
        let to = match db::page_by_slug(&ctx.conn, parent)? {
            Some(pp) if !pp.is_ref => page::ref_path(&pp.path, base),
            _ => ctx.path_for(&new_slug),
        };
        moves.push((src.path.clone(), to));
    } else if let Some(dir) = src.path.strip_suffix("/index.md") {
        moves.push((dir.to_string(), new_slug.clone()));
    } else {
        moves.push((src.path.clone(), format!("{new_slug}.md")));
        let dir = src.path.trim_end_matches(".md");
        if wiki.join(dir).is_dir() {
            moves.push((dir.to_string(), new_slug.clone()));
        }
    }
    for (_, to) in &moves {
        if wiki.join(to).exists() {
            bail!("{to} already exists in the wiki");
        }
    }
    let map_path = |p: &str| -> String {
        for (from, to) in &moves {
            if p == from {
                return to.clone();
            }
            if let Some(rest) = p.strip_prefix(&format!("{from}/")) {
                return format!("{to}/{rest}");
            }
        }
        p.to_string()
    };

    // The wiki before and after.
    let pages = db::pages_in_scope(&ctx.conn, None)?;
    struct After {
        slug: String,
        path: String,
        project: Option<String>,
    }
    let mut after: HashMap<String, After> = HashMap::new(); // keyed by old slug, lowercased
    for p in &pages {
        let path = map_path(&p.path);
        let loc = page::locate(&path).context("moved page has no slug")?;
        after.insert(
            p.slug.to_lowercase(),
            After {
                slug: loc.slug,
                path,
                project: loc.project,
            },
        );
    }
    let mut new_slugs = HashSet::new();
    for a in after.values() {
        if !new_slugs.insert(a.slug.to_lowercase()) {
            bail!("the move would give two pages the slug `{}`", a.slug);
        }
    }
    let old_r = Resolver::new(pages.iter().map(|p| (p.slug.clone(), p.project.clone())));
    let new_r = Resolver::new(after.values().map(|a| (a.slug.clone(), a.project.clone())));
    let renamed = |slug: &str| after.get(&slug.to_lowercase());
    let moved: Vec<(&str, &str)> = pages
        .iter()
        .filter_map(|p| {
            let a = renamed(&p.slug)?;
            (a.slug != p.slug).then_some((p.slug.as_str(), a.slug.as_str()))
        })
        .collect();

    let before = problems(ctx, &|s| {
        renamed(s).map_or(s.to_string(), |a| a.slug.clone())
    })?;

    // Rewrite links in every page, reading from the old location.
    let mut rewrites: Vec<(String, String)> = Vec::new(); // (new path, new text)
    let mut links_changed = 0;
    for p in &pages {
        let a = renamed(&p.slug).expect("every page has an after-state");
        let text = std::fs::read_to_string(wiki.join(&p.path))
            .with_context(|| format!("reading {}", p.path))?;
        let body_at = text.len() - Page::parse(&text).map_or(text.len(), |pg| pg.body.len());
        let body = &text[body_at..];
        let mut edits = Vec::new();
        for span in links::extract_spans(body, &p.path) {
            let Some(range) = span.range.clone() else {
                continue;
            };
            let l = &span.link;
            let replacement = match l.kind {
                Kind::Wiki => {
                    let Resolution::Found(target) = old_r.resolve(&l.target, p.project.as_deref())
                    else {
                        continue; // broken or ambiguous already: leave it alone
                    };
                    let want = renamed(&target).map_or(target.clone(), |t| t.slug.clone());
                    match new_r.resolve(&l.target, a.project.as_deref()) {
                        Resolution::Found(s) if s.eq_ignore_ascii_case(&want) => continue,
                        _ if l.target.eq_ignore_ascii_case(&target) => want,
                        _ => new_r.shortest_link(&want, a.project.as_deref()),
                    }
                }
                Kind::Md => {
                    let Some(target) = old_r.exact(&l.target) else {
                        continue;
                    };
                    let Some(t) = renamed(target) else { continue };
                    let url = links::relative_path(&a.path, &t.path);
                    if body[range.clone()].replace("%20", " ") == url {
                        continue;
                    }
                    url.replace(' ', "%20")
                }
            };
            edits.push((range, replacement));
        }
        if !edits.is_empty() {
            links_changed += edits.len();
            let out = format!("{}{}", &text[..body_at], links::apply_edits(body, edits));
            rewrites.push((a.path.clone(), out));
        }
    }

    // Do it: move, write the rewrites, tidy empty folders, commit.
    for (from, to) in &moves {
        let dest = wiki.join(to);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(wiki.join(from), &dest)
            .with_context(|| format!("moving {from} to {to}"))?;
        let mut dir = wiki.join(from);
        while dir.pop() && dir != wiki && std::fs::remove_dir(&dir).is_ok() {}
    }
    for (path, text) in &rewrites {
        util::atomic_write(&wiki.join(path), text.as_bytes())?;
    }
    let mut paths: Vec<&str> = moves
        .iter()
        .flat_map(|(f, t)| [f.as_str(), t.as_str()])
        .collect();
    paths.extend(rewrites.iter().map(|(p, _)| p.as_str()));
    paths.sort();
    paths.dedup();
    let msg = format!("{} → {new_slug}: {}", src.slug, message.unwrap_or("rename"));
    let commit = ctx.commit(&paths, &msg)?;
    ctx.sync()?;

    let after_problems = problems(ctx, &|s| s.to_string())?;
    let introduced: Vec<String> = after_problems
        .into_iter()
        .filter(|p| !before.contains(p))
        .map(|(src, target)| format!("{src}: [[{target}]] no longer resolves to one page"))
        .collect();
    let row = db::page_by_slug(&ctx.conn, &new_slug)?;
    ctx.event("write", row.as_ref());

    if ctx.json {
        print_json(&json!({
            "from": src.slug, "to": new_slug, "commit": commit,
            "moved": moved.iter().map(|(a, b)| json!({ "from": a, "to": b })).collect::<Vec<_>>(),
            "pages_rewritten": rewrites.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            "links_rewritten": links_changed, "new_link_problems": introduced,
        }));
    } else {
        for (a, b) in &moved {
            println!("moved {a} → {b}");
        }
        println!(
            "rewrote {links_changed} link(s) in {} page(s) · commit {}",
            rewrites.len(),
            commit.as_deref().unwrap_or("none")
        );
        for p in &introduced {
            warn(p);
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// (source slug, target) of every link that is broken or ambiguous, with source slugs
/// passed through `name` so problems before and after a move can be compared.
fn problems(ctx: &Ctx, name: &dyn Fn(&str) -> String) -> Result<HashSet<(String, String)>> {
    let mut st = ctx.conn.prepare(
        "SELECT p.slug, l.dst_slug FROM links l JOIN pages p ON p.id = l.src_id
         WHERE l.resolution != 'ok'",
    )?;
    let rows = st
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().map(|(s, t)| (name(&s), t)).collect())
}
