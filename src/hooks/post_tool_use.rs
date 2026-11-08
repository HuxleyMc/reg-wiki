//! PostToolUse on Edit/Write: if the edited file is one of a page's `sources`, remind
//! the agent to update that page (once per page per session). Also notes the edit, for
//! the Stop hook.

use std::path::{Path, PathBuf};

use anyhow::Result;
use globset::GlobBuilder;

use super::{Input, Output, state};
use crate::cmd::Ctx;
use crate::{project, util};

const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

pub fn run(ctx: &Ctx, input: &Input) -> Result<Option<Output>> {
    let tool = input.tool_name.as_deref().unwrap_or("");
    if !EDIT_TOOLS.contains(&tool) {
        return Ok(None);
    }
    let Some(ti) = &input.tool_input else {
        return Ok(None);
    };
    let Some(raw) = ["file_path", "notebook_path"]
        .iter()
        .find_map(|k| ti.get(*k).and_then(|v| v.as_str()))
    else {
        return Ok(None);
    };
    let file = absolute(&ctx.cwd, Path::new(raw));
    let sid = input.session_id.as_deref();

    let wiki = ctx
        .home
        .wiki
        .canonicalize()
        .unwrap_or(ctx.home.wiki.clone());
    if file.starts_with(&wiki) {
        return hand_edit(ctx, sid, &file, &wiki);
    }

    let Some(root) = file.parent().and_then(project::git_root) else {
        return Ok(None);
    };
    let Some(proj) = project::detect(&ctx.cfg.projects(), &root) else {
        return Ok(None);
    };
    let Some(rel) = util::rel_str(&root, &file) else {
        return Ok(None);
    };
    let pages = covering(ctx, &proj.name, &rel)?;

    let file_str = file.to_string_lossy().into_owned();
    let fresh: Vec<Covering> = match sid {
        Some(id) => state::update(&ctx.home, id, |s| {
            s.started.get_or_insert_with(util::now);
            if !s.edited.contains(&file_str) {
                s.edited.push(file_str.clone());
            }
            let fresh: Vec<Covering> = pages
                .iter()
                .filter(|c| !s.reminded.contains(&c.slug))
                .cloned()
                .collect();
            s.reminded.extend(fresh.iter().map(|c| c.slug.clone()));
            fresh
        })?,
        None => pages,
    };
    if fresh.is_empty() {
        return Ok(None);
    }
    let mut out = format!("reg-wiki: {rel} is documented by:\n");
    for c in &fresh {
        out.push_str(&format!(
            "- {} — {} (sources: {}; hash {})\n",
            c.slug, c.summary, c.glob, c.hash
        ));
    }
    out.push_str(
        "If your change makes what the page says wrong or incomplete, update it when you're \
         done: `reg-wiki read <slug>`, then `reg-wiki write <slug> --base <hash> -m \"why\"`.",
    );
    Ok(Some(Output::Context(out)))
}

/// The agent edited a wiki file with its own tools: that counts as writing to the wiki,
/// but the change isn't checked or committed, so say so once.
fn hand_edit(ctx: &Ctx, sid: Option<&str>, file: &Path, wiki: &Path) -> Result<Option<Output>> {
    let first = match sid {
        Some(id) => state::update(&ctx.home, id, |s| {
            s.wrote = true;
            !std::mem::replace(&mut s.warned_hand_edit, true)
        })?,
        None => true,
    };
    if !first {
        return Ok(None);
    }
    let rel = util::rel_str(wiki, file).unwrap_or_default();
    Ok(Some(Output::Context(format!(
        "reg-wiki: you edited the wiki file {rel} directly. That change is indexed but not \
         validated or committed. Prefer `reg-wiki write <slug> -m \"why\"`, which checks the \
         page and commits it; `reg-wiki status` lists uncommitted hand edits."
    ))))
}

#[derive(Clone, Debug)]
struct Covering {
    slug: String,
    summary: String,
    hash: String,
    glob: String,
}

/// Pages of `project` (by folder or `projects:`) with a `sources` glob matching `rel`.
fn covering(ctx: &Ctx, project: &str, rel: &str) -> Result<Vec<Covering>> {
    let mut st = ctx.conn.prepare(
        "SELECT p.slug, p.summary, p.hash, s.glob FROM sources s
         JOIN pages p ON p.id = s.page_id
         LEFT JOIN projects pr ON pr.id = p.project_id
         WHERE pr.name = ?1 OR p.id IN (
            SELECT pp.page_id FROM page_projects pp JOIN projects x ON x.id = pp.project_id
            WHERE x.name = ?1)
         ORDER BY p.slug",
    )?;
    let rows = st
        .query_map([project], |r| {
            Ok(Covering {
                slug: r.get(0)?,
                summary: r.get(1)?,
                hash: util::short(&r.get::<_, String>(2)?).to_string(),
                glob: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out: Vec<Covering> = Vec::new();
    for c in rows {
        if glob_matches(&c.glob, rel) && !out.iter().any(|o| o.slug == c.slug) {
            out.push(c);
        }
    }
    Ok(out)
}

/// Match a path relative to the project root the way git's `:(glob)` pathspecs do (as
/// `lint` uses them): `*` stays within a directory, `**` crosses directories, and a
/// pattern without wildcards also matches everything under it.
pub fn glob_matches(glob: &str, rel: &str) -> bool {
    let g = glob.trim();
    let g = g.strip_prefix("./").unwrap_or(g).trim_start_matches('/');
    if g.is_empty() {
        return false;
    }
    if !g.contains(['*', '?', '[', '{']) {
        let dir = g.trim_end_matches('/');
        return rel == dir || rel.starts_with(&format!("{dir}/"));
    }
    let g = g
        .strip_suffix('/')
        .map_or(g.to_string(), |d| format!("{d}/**"));
    GlobBuilder::new(&g)
        .literal_separator(true)
        .build()
        .is_ok_and(|m| m.compile_matcher().is_match(rel))
}

fn absolute(cwd: &Path, p: &Path) -> PathBuf {
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    // A file that's gone (or a path we can't resolve): canonicalise what exists.
    match (
        p.parent().and_then(|d| d.canonicalize().ok()),
        p.file_name(),
    ) {
        (Some(d), Some(f)) => d.join(f),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::glob_matches as m;

    #[test]
    fn globs_follow_git_pathspecs() {
        assert!(m("src/billing/**", "src/billing/stripe.rs"));
        assert!(m("src/billing/**", "src/billing/a/b.rs"));
        assert!(m("src/billing", "src/billing/a/b.rs"));
        assert!(m("src/billing/", "src/billing/x.rs"));
        assert!(m("./src/*.rs", "src/main.rs"));
        assert!(!m("src/*.rs", "src/cmd/mod.rs"));
        assert!(m("**/*.sql", "db/migrations/1.sql"));
        assert!(m("Cargo.toml", "Cargo.toml"));
        assert!(!m("src/bill", "src/billing/x.rs"));
        assert!(!m("", "x"));
    }
}
