use std::process::ExitCode;

use anyhow::{Result, bail};
use serde_json::json;

use super::{Ctx, print_json};
use crate::home::{ProjectCfg, SHARED};
use crate::project::{git_remote, git_root, normalize_remote};
use crate::util;

pub fn add(ctx: &mut Ctx, name: Option<&str>) -> Result<ExitCode> {
    let root = git_root(&ctx.cwd).unwrap_or_else(|| ctx.cwd.clone());
    let root = root.canonicalize().unwrap_or(root);
    let remote = git_remote(&root).map(|r| normalize_remote(&r));
    let name = match name {
        Some(n) => n.trim().to_string(),
        None => root
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase().replace(' ', "-"))
            .unwrap_or_default(),
    };
    if name.is_empty()
        || name == SHARED
        || name == "refs"
        || name.starts_with('.')
        || name.contains(['/', '\\'])
    {
        bail!("`{name}` can't be a project name");
    }
    crate::page::check_slug(&name)?;

    let root_str = root.to_string_lossy().into_owned();
    let new = ProjectCfg {
        name: name.clone(),
        remote: remote.clone(),
        root: Some(root_str),
    };
    let same_repo = |p: &ProjectCfg| match (&p.remote, &new.remote) {
        (Some(a), Some(b)) => a == b,
        _ => p.root == new.root,
    };
    let projects = ctx.cfg.projects();
    if let Some(p) = projects.iter().find(|p| same_repo(p)) {
        if p.name == name {
            report(ctx, &name, &new, "already registered", None)?;
            return Ok(ExitCode::SUCCESS);
        }
        bail!("this repo is already registered as `{}`", p.name);
    }
    if projects.iter().any(|p| p.name == name) {
        bail!("a different repo is already registered as `{name}`; pass another name");
    }

    let _lock = ctx.home.lock()?;
    ctx.cfg.add_project(&new)?;
    ctx.cfg.save()?;
    ctx.current = Some(name.clone());

    // Every project gets a hub page, unless its folder already has one.
    let has_hub = ["index.md"]
        .iter()
        .any(|f| ctx.home.wiki.join(&name).join(f).exists())
        || ctx.home.wiki.join(format!("{name}.md")).exists();
    let mut commit = None;
    if !has_hub {
        let today = util::today();
        let hub = format!(
            "---\ntitle: {name}\nsummary: Hub for the {name} project. Start here for an overview and links to its pages.\n\
             tags: [hub]\ncreated: {today}\nupdated: {today}\nstatus: draft\n---\n\n# {name}\n\n\
             What {name} is, how it is laid out, and links to its pages.\n"
        );
        commit = ctx.store(&format!("{name}/index.md"), &hub, &name, "add project hub")?;
    }
    ctx.sync()?;
    report(ctx, &name, &new, "registered", commit)?;
    Ok(ExitCode::SUCCESS)
}

fn report(ctx: &Ctx, name: &str, p: &ProjectCfg, what: &str, commit: Option<String>) -> Result<()> {
    if ctx.json {
        print_json(&json!({ "project": p, "status": what, "commit": commit }));
    } else {
        let id = p.remote.as_deref().or(p.root.as_deref()).unwrap_or("");
        println!("{what} project {name} ({id})");
        if commit.is_some() {
            println!(
                "hub page: {name} — fill it in with `reg-wiki read {name}` then `reg-wiki write {name}`"
            );
        }
    }
    Ok(())
}

pub fn list(ctx: &Ctx) -> Result<ExitCode> {
    let projects = ctx.cfg.projects();
    let mut rows = Vec::new();
    for p in &projects {
        let pages: i64 = ctx.conn.query_row(
            "SELECT count(*) FROM pages p JOIN projects pr ON pr.id = p.project_id WHERE pr.name = ?1",
            [&p.name],
            |r| r.get(0),
        )?;
        rows.push((p, pages));
    }
    if ctx.json {
        let items: Vec<_> = rows
            .iter()
            .map(|(p, n)| {
                json!({ "name": p.name, "remote": p.remote, "root": p.root, "pages": n,
                                  "current": ctx.current.as_deref() == Some(p.name.as_str()) })
            })
            .collect();
        print_json(&json!(items));
        return Ok(ExitCode::SUCCESS);
    }
    if rows.is_empty() {
        eprintln!("no projects yet; run `reg-wiki project add` inside a repo");
    }
    for (p, n) in rows {
        let mark = if ctx.current.as_deref() == Some(p.name.as_str()) {
            "*"
        } else {
            " "
        };
        let id = p.remote.as_deref().or(p.root.as_deref()).unwrap_or("");
        println!("{mark} {} — {id} ({n} pages)", p.name);
    }
    Ok(ExitCode::SUCCESS)
}

pub fn rm(ctx: &mut Ctx, name: &str) -> Result<ExitCode> {
    let _lock = ctx.home.lock()?;
    if !ctx.cfg.remove_project(name) {
        bail!("no registered project `{name}`");
    }
    ctx.cfg.save()?;
    if ctx.json {
        print_json(&json!({ "removed": name }));
    } else {
        println!("removed project {name}; its pages in wiki/{name}/ were kept");
    }
    Ok(ExitCode::SUCCESS)
}
