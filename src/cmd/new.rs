use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use serde_json::json;
use serde_yaml_ng::{Mapping, Value};

use super::{Ctx, line, print_json, warn};
use crate::page::{self, Page};
use crate::{db, search, util};

pub struct Args {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub parent: Option<String>,
    pub tags: Vec<String>,
    pub message: Option<String>,
}

pub fn run(ctx: &mut Ctx, a: Args) -> Result<ExitCode> {
    let _lock = ctx.home.lock()?;
    ctx.sync()?;

    let (slug, rel) = match &a.parent {
        Some(parent) => {
            let p = ctx.resolve(parent)?;
            if p.is_ref {
                bail!("`{}` is a ref; refs can't have refs of their own", p.slug);
            }
            let base = a
                .slug
                .trim_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&a.slug);
            let base = base.strip_suffix(".md").unwrap_or(base);
            // A page's refs live next to it: `x/billing/index.md` → `x/billing/refs/`,
            // and a flat `x/billing.md` → `x/billing/refs/` too.
            let dir = match p.path.strip_suffix("index.md") {
                Some(d) => d.trim_end_matches('/').to_string(),
                None => p.path.trim_end_matches(".md").to_string(),
            };
            let rel = if dir.is_empty() {
                format!("refs/{base}.md")
            } else {
                format!("{dir}/refs/{base}.md")
            };
            (format!("{}/{base}", p.slug), rel)
        }
        None => {
            let slug = ctx.qualify(&a.slug);
            let rel = ctx.path_for(&slug);
            (slug, rel)
        }
    };
    page::check_slug(&slug)?;
    if let Some(existing) = db::page_by_slug(&ctx.conn, &slug)? {
        bail!(
            "`{}` already exists ({}); update it with `reg-wiki write`",
            existing.slug,
            existing.path
        );
    }
    if ctx.home.wiki.join(&rel).exists() {
        bail!("{rel} already exists");
    }

    let parent_slug = slug
        .rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .filter(|_| a.parent.is_some());
    let similar: Vec<_> = search::similar(&ctx.conn, &a.title, 3)?
        .into_iter()
        .filter(|p| Some(&p.slug) != parent_slug.as_ref())
        .collect();
    if !similar.is_empty() && !ctx.json {
        warn("similar pages exist; consider updating one of them instead:");
        for p in &similar {
            eprintln!("  {}", line(p));
        }
    }

    let today = util::today();
    let mut pg = Page {
        fm: Mapping::new(),
        body: format!("\n# {}\n\n", a.title.trim()),
        has_fm: true,
    };
    pg.set("title", a.title.trim());
    pg.set("summary", a.summary.trim());
    pg.set(
        "tags",
        Value::Sequence(a.tags.iter().map(|t| Value::from(t.as_str())).collect()),
    );
    pg.set("created", today.as_str());
    pg.set("updated", today.as_str());
    pg.set("status", "draft");
    let errs = pg.validate();
    if !errs.is_empty() {
        bail!("invalid page:\n  {}", errs.join("\n  "));
    }

    let text = pg.render();
    let msg = a.message.as_deref().unwrap_or("create page");
    let commit = ctx.store(&rel, &text, &slug, msg)?;
    ctx.sync()?;
    let row = db::page_by_slug(&ctx.conn, &slug)?;
    ctx.event("write", row.as_ref());

    let hash = util::short(&util::hash_hex(text.as_bytes())).to_string();
    if ctx.json {
        print_json(
            &json!({ "slug": slug, "path": rel, "hash": hash, "commit": commit, "similar": similar }),
        );
    } else {
        println!(
            "created {slug} ({}) · hash {hash}",
            Path::new("wiki").join(&rel).display()
        );
        println!("next: fill it in with `reg-wiki write {slug} --base {hash} -m \"…\" < page.md`");
    }
    Ok(ExitCode::SUCCESS)
}
