use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{Ctx, link_problems, print_json, warn};
use crate::page::{self, Page};
use crate::{db, util};

/// Exit code for a `--base` conflict, distinct from ordinary errors.
const CONFLICT: u8 = 3;

pub fn run(ctx: &mut Ctx, input: &str, base: Option<&str>, message: &str) -> Result<ExitCode> {
    if message.trim().is_empty() {
        bail!("-m needs a reason: it becomes the commit message people review");
    }
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the page from stdin")?;
    if text.trim().is_empty() {
        bail!("no content on stdin");
    }

    let _lock = ctx.home.lock()?;
    ctx.sync()?;

    let existing = ctx.find(input)?;
    let (slug, rel) = match &existing {
        Some(p) => (p.slug.clone(), p.path.clone()),
        None => {
            let slug = ctx.qualify(input);
            page::check_slug(&slug)?;
            let rel = ctx.path_for(&slug);
            (slug, rel)
        }
    };

    let path = ctx.home.wiki.join(&rel);
    let current = match &existing {
        Some(_) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?,
        ),
        None => None,
    };

    if let Some(base) = base {
        let base = base.trim();
        if base.len() < 4 {
            bail!("--base needs at least 4 characters of the hash printed by `reg-wiki read`");
        }
        match &current {
            Some(cur) => {
                let hash = util::hash_hex(cur.as_bytes());
                if !hash.starts_with(base) {
                    return conflict(ctx, &slug, util::short(&hash), Some(cur));
                }
            }
            None => return conflict(ctx, &slug, "none", None),
        }
    }

    let mut pg = Page::parse(&text)?;
    let old = current
        .as_deref()
        .map(Page::parse)
        .transpose()
        .ok()
        .flatten();
    if !pg.has_fm {
        match &old {
            // Body only: keep the page's frontmatter.
            Some(o) if o.has_fm => pg.fm = o.fm.clone(),
            _ => bail!("new pages need frontmatter with at least `title` and `summary`"),
        }
        pg.has_fm = true;
    }
    if pg.get("created").is_none() {
        let created = old
            .as_ref()
            .and_then(|o| o.str("created"))
            .map(str::to_string)
            .unwrap_or_else(util::today);
        pg.set("created", created);
    }
    let errs = pg.validate();
    if !errs.is_empty() {
        bail!("refused to write {slug}:\n  {}", errs.join("\n  "));
    }

    // Unchanged apart from the date reg would set? Then there is nothing to write.
    if let (Some(cur), Some(o)) = (&current, &old) {
        let mut same = pg.clone();
        match o.get("updated") {
            Some(v) => same.set("updated", v.clone()),
            None => {
                same.fm.shift_remove("updated");
            }
        }
        if same.render() == *cur {
            let hash = util::short(&util::hash_hex(cur.as_bytes())).to_string();
            if ctx.json {
                print_json(
                    &json!({ "slug": slug, "hash": hash, "commit": null, "unchanged": true }),
                );
            } else {
                println!("unchanged {slug} · hash {hash}");
            }
            return Ok(ExitCode::SUCCESS);
        }
    }
    pg.set("updated", util::today());

    let out = pg.render();
    let commit = ctx.store(&rel, &out, &slug, message.trim())?;
    ctx.sync()?;
    let row = db::page_by_slug(&ctx.conn, &slug)?
        .context("page vanished from the index after writing")?;
    ctx.event("write", Some(&row));
    let problems = link_problems(ctx, &row)?;

    if ctx.json {
        print_json(&json!({
            "slug": slug, "path": rel, "hash": row.hash, "commit": commit,
            "created": existing.is_none(), "link_problems": problems,
        }));
    } else {
        let verb = if existing.is_some() {
            "wrote"
        } else {
            "created"
        };
        let commit = commit.as_deref().unwrap_or("none");
        println!("{verb} {slug} · hash {} · commit {commit}", row.hash);
        for p in &problems {
            warn(p);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn conflict(ctx: &Ctx, slug: &str, hash: &str, current: Option<&str>) -> Result<ExitCode> {
    if ctx.json {
        print_json(&json!({ "conflict": true, "slug": slug, "hash": hash, "content": current }));
    } else {
        match current {
            Some(cur) => {
                eprintln!(
                    "reg-wiki: conflict: {slug} changed since your --base (now {hash}). \
                     Its current content is on stdout; merge your edit into it and retry with --base {hash}."
                );
                print!("{cur}");
            }
            None => {
                eprintln!("reg-wiki: conflict: {slug} no longer exists; drop --base to create it")
            }
        }
    }
    Ok(ExitCode::from(CONFLICT))
}
