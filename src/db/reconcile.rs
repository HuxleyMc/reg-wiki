//! Bring the index in line with the files. Unchanged (mtime, size) pairs are skipped;
//! anything else is re-hashed and, if the content differs, re-parsed.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use walkdir::WalkDir;

use crate::home::ProjectCfg;
use crate::links::{self, Kind, Resolution, Resolver};
use crate::page::{self, Page};
use crate::util;

#[derive(Debug, Default, Serialize)]
pub struct Stats {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub pages: usize,
    pub warnings: Vec<String>,
}

pub fn reconcile(
    conn: &mut rusqlite::Connection,
    wiki: &Path,
    projects: &[ProjectCfg],
) -> Result<Stats> {
    let tx = conn.transaction()?;
    let stats = run(&tx, wiki, projects)?;
    tx.commit()?;
    Ok(stats)
}

/// Drop every indexed page and rebuild from the files. Events are kept.
pub fn reindex(
    conn: &mut rusqlite::Connection,
    wiki: &Path,
    projects: &[ProjectCfg],
) -> Result<Stats> {
    let tx = conn.transaction()?;
    tx.execute_batch("DELETE FROM pages; DELETE FROM pages_fts;")?;
    let stats = run(&tx, wiki, projects)?;
    tx.commit()?;
    Ok(stats)
}

fn run(tx: &Transaction, wiki: &Path, projects: &[ProjectCfg]) -> Result<Stats> {
    let mut stats = Stats::default();
    sync_projects(tx, projects)?;

    let mut known: HashMap<String, (i64, i64, i64, String)> = HashMap::new();
    {
        let mut st = tx.prepare("SELECT path, id, mtime, size, hash FROM pages")?;
        let rows = st.query_map([], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        })?;
        for row in rows {
            let (path, v) = row?;
            known.insert(path, v);
        }
    }

    let mut changed = false;
    let walker = WalkDir::new(wiki)
        .min_depth(1)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'));
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|x| x != "md") {
            continue;
        }
        let Some(rel) = util::rel_str(wiki, entry.path()) else {
            continue;
        };
        let meta = entry.metadata()?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        let size = meta.len() as i64;

        let existing = known.remove(&rel);
        if let Some((_, m, s, _)) = &existing
            && *m == mtime
            && *s == size
        {
            continue;
        }
        let bytes = fs::read(entry.path())?;
        let hash = util::hash_hex(&bytes);
        if let Some((id, _, _, h)) = &existing
            && *h == hash
        {
            tx.execute(
                "UPDATE pages SET mtime = ?1, size = ?2 WHERE id = ?3",
                params![mtime, size, id],
            )?;
            continue;
        }
        let file = FileInfo {
            rel: &rel,
            bytes: &bytes,
            hash: &hash,
            mtime,
            size,
        };
        if upsert(
            tx,
            existing.as_ref().map(|e| e.0),
            &file,
            &mut stats.warnings,
        )? {
            changed = true;
            if existing.is_some() {
                stats.updated += 1
            } else {
                stats.added += 1
            }
        }
    }

    for (_, (id, ..)) in known {
        tx.execute("DELETE FROM pages_fts WHERE rowid = ?1", [id])?;
        tx.execute("DELETE FROM pages WHERE id = ?1", [id])?;
        stats.removed += 1;
        changed = true;
    }

    if changed {
        resolve_all(tx)?;
    }
    stats.pages = tx.query_row("SELECT count(*) FROM pages", [], |r| r.get::<_, i64>(0))? as usize;
    Ok(stats)
}

fn sync_projects(tx: &Transaction, projects: &[ProjectCfg]) -> Result<()> {
    for p in projects {
        tx.execute(
            "INSERT INTO projects(name, remote, root_path, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET remote = excluded.remote, root_path = excluded.root_path",
            params![p.name, p.remote, p.root, util::now()],
        )?;
    }
    Ok(())
}

/// Projects named by a folder or by frontmatter don't have to be registered.
fn project_id(tx: &Transaction, name: &str) -> Result<i64> {
    if let Some(id) = tx
        .query_row("SELECT id FROM projects WHERE name = ?1", [name], |r| {
            r.get(0)
        })
        .optional()?
    {
        return Ok(id);
    }
    tx.execute(
        "INSERT INTO projects(name, created_at) VALUES (?1, ?2)",
        params![name, util::now()],
    )?;
    Ok(tx.last_insert_rowid())
}

struct FileInfo<'a> {
    rel: &'a str,
    bytes: &'a [u8],
    hash: &'a str,
    mtime: i64,
    size: i64,
}

/// Index one file. Returns false if it was skipped.
fn upsert(
    tx: &Transaction,
    id: Option<i64>,
    f: &FileInfo,
    warnings: &mut Vec<String>,
) -> Result<bool> {
    let Some(loc) = page::locate(f.rel) else {
        return Ok(false);
    };
    let text = String::from_utf8_lossy(f.bytes);
    let page = Page::parse(&text).unwrap_or_else(|e| {
        warnings.push(format!("{}: {e}", f.rel));
        Page::raw(&text)
    });

    let clash: Option<String> = tx
        .query_row(
            "SELECT path FROM pages WHERE slug = ?1 AND path != ?2",
            params![loc.slug, f.rel],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(other) = clash {
        warnings.push(format!(
            "{}: slug `{}` is already used by {other}; skipped",
            f.rel, loc.slug
        ));
        return Ok(false);
    }

    let basename = loc.slug.rsplit('/').next().unwrap_or(&loc.slug).to_string();
    let title = page.title().unwrap_or(basename);
    let summary = page.str("summary").unwrap_or("").to_string();
    let tags = page.list("tags");
    let project = loc
        .project
        .as_deref()
        .map(|n| project_id(tx, n))
        .transpose()?;
    let words = page.body.split_whitespace().count() as i64;
    let chars = page.body.chars().count() as i64;

    let cols = params![
        loc.slug,
        f.rel,
        project,
        loc.is_ref,
        loc.parent,
        title,
        summary,
        page.str("status"),
        page.str("created"),
        page.str("updated"),
        words,
        chars,
        f.bytes.len() as i64,
        f.mtime,
        f.size,
        f.hash,
    ];
    let id = match id {
        Some(id) => {
            tx.execute(
                "UPDATE pages SET slug=?1, path=?2, project_id=?3, is_ref=?4, parent_slug=?5, title=?6,
                 summary=?7, status=?8, created=?9, updated=?10, words=?11, chars=?12, bytes=?13,
                 mtime=?14, size=?15, hash=?16 WHERE id = ?17",
                rusqlite::params_from_iter(cols.iter().copied().chain([&id as &dyn rusqlite::ToSql])),
            )?;
            for t in ["page_projects", "tags", "links", "sources"] {
                let col = if t == "links" { "src_id" } else { "page_id" };
                tx.execute(&format!("DELETE FROM {t} WHERE {col} = ?1"), [id])?;
            }
            tx.execute("DELETE FROM pages_fts WHERE rowid = ?1", [id])?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO pages(slug, path, project_id, is_ref, parent_slug, title, summary, status,
                 created, updated, words, chars, bytes, mtime, size, hash)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                cols,
            )?;
            tx.last_insert_rowid()
        }
    };

    for tag in &tags {
        tx.execute(
            "INSERT OR IGNORE INTO tags(page_id, tag) VALUES (?1, ?2)",
            params![id, tag],
        )?;
    }
    for name in page.list("projects") {
        if Some(name.as_str()) == loc.project.as_deref() {
            continue;
        }
        let pid = project_id(tx, &name)?;
        tx.execute(
            "INSERT OR IGNORE INTO page_projects(page_id, project_id) VALUES (?1, ?2)",
            params![id, pid],
        )?;
    }
    for glob in page.list("sources") {
        tx.execute(
            "INSERT INTO sources(page_id, glob) VALUES (?1, ?2)",
            params![id, glob],
        )?;
    }
    for l in links::extract(&page.body, f.rel) {
        tx.execute(
            "INSERT INTO links(src_id, dst_slug, heading, kind) VALUES (?1, ?2, ?3, ?4)",
            params![id, l.target, l.heading, l.kind.as_str()],
        )?;
    }
    tx.execute(
        "INSERT INTO pages_fts(rowid, title, summary, tags, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, title, summary, tags.join(" "), page.body],
    )?;
    Ok(true)
}

/// Re-resolve every link and parent. Cheap: the wiki is small, and a new page can fix
/// (or make ambiguous) links on any other page.
fn resolve_all(tx: &Transaction) -> Result<()> {
    let pages: Vec<(i64, String, Option<String>)> = {
        let mut st = tx.prepare("SELECT p.id, p.slug, pr.name FROM pages p LEFT JOIN projects pr ON pr.id = p.project_id")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let ids: HashMap<String, i64> = pages
        .iter()
        .map(|(id, s, _)| (s.to_lowercase(), *id))
        .collect();
    let project_of: HashMap<i64, Option<String>> =
        pages.iter().map(|(id, _, p)| (*id, p.clone())).collect();
    let resolver = Resolver::new(pages.iter().map(|(_, s, p)| (s.clone(), p.clone())));

    tx.execute(
        "UPDATE pages SET parent_id = (SELECT q.id FROM pages q WHERE q.slug = pages.parent_slug)",
        [],
    )?;

    let links: Vec<(i64, i64, String, String)> = {
        let mut st = tx.prepare("SELECT rowid, src_id, dst_slug, kind FROM links")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut up = tx.prepare("UPDATE links SET dst_id = ?1, resolution = ?2 WHERE rowid = ?3")?;
    for (rowid, src, target, kind) in links {
        let res = if kind == Kind::Md.as_str() {
            resolver
                .exact(&target)
                .map_or(Resolution::Broken, |s| Resolution::Found(s.to_string()))
        } else {
            resolver.resolve(&target, project_of.get(&src).and_then(|p| p.as_deref()))
        };
        let dst = match &res {
            Resolution::Found(s) => ids.get(&s.to_lowercase()).copied(),
            _ => None,
        };
        up.execute(params![dst, res.label(), rowid])?;
    }
    Ok(())
}
