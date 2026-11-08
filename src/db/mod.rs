//! The SQLite index: derived from the files, plus usage events that exist nowhere else.

pub mod reconcile;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;

use crate::links::Resolver;
use crate::util;

const SCHEMA_VERSION: i32 = 2;

const FTS_TOKENIZE: &str = "tokenize = 'porter unicode61'";

fn schema() -> String {
    format!(
        "
CREATE TABLE projects(
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  remote TEXT,
  root_path TEXT,
  created_at TEXT NOT NULL
);
CREATE TABLE pages(
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE COLLATE NOCASE,
  path TEXT NOT NULL UNIQUE,
  project_id INTEGER REFERENCES projects(id),
  is_ref INTEGER NOT NULL DEFAULT 0,
  parent_slug TEXT COLLATE NOCASE,
  parent_id INTEGER,
  title TEXT NOT NULL,
  summary TEXT NOT NULL DEFAULT '',
  status TEXT,
  created TEXT,
  updated TEXT,
  words INTEGER NOT NULL,
  chars INTEGER NOT NULL,
  bytes INTEGER NOT NULL,
  mtime INTEGER NOT NULL,
  size INTEGER NOT NULL,
  hash TEXT NOT NULL
);
CREATE TABLE page_projects(
  page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  project_id INTEGER NOT NULL REFERENCES projects(id),
  PRIMARY KEY(page_id, project_id)
);
CREATE TABLE tags(
  page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  tag TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY(page_id, tag)
);
CREATE TABLE links(
  src_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  dst_slug TEXT NOT NULL,
  dst_id INTEGER,
  heading TEXT,
  kind TEXT NOT NULL,
  resolution TEXT NOT NULL DEFAULT 'broken'
);
CREATE INDEX links_src ON links(src_id);
CREATE INDEX links_dst ON links(dst_id);
CREATE TABLE sources(
  page_id INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  glob TEXT NOT NULL
);
CREATE VIRTUAL TABLE pages_fts USING fts5(title, summary, tags, body, {FTS_TOKENIZE});
CREATE TABLE events(
  id INTEGER PRIMARY KEY,
  ts TEXT NOT NULL,
  kind TEXT NOT NULL,
  slug TEXT,
  page_id INTEGER,
  project_id INTEGER,
  agent TEXT,
  query TEXT,
  hits INTEGER
);
"
    )
}

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(Duration::from_secs(10))?;
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL;")?;
    let version: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        0 => {
            conn.execute_batch(&format!(
                "BEGIN; {} PRAGMA user_version={SCHEMA_VERSION}; COMMIT;",
                schema()
            ))?;
        }
        // v2: stemming in the full-text index. Drop the pages so the next reconcile
        // rebuilds them; events are kept.
        1 => conn.execute_batch(&format!(
            "BEGIN; DROP TABLE pages_fts; DELETE FROM pages;
             CREATE VIRTUAL TABLE pages_fts USING fts5(title, summary, tags, body, {FTS_TOKENIZE});
             PRAGMA user_version={SCHEMA_VERSION}; COMMIT;"
        ))?,
        SCHEMA_VERSION => {}
        v => bail!(
            "{} has schema version {v}, but this reg-wiki knows {SCHEMA_VERSION}",
            path.display()
        ),
    }
    Ok(conn)
}

#[derive(Debug, Clone, Serialize)]
pub struct PageRow {
    #[serde(skip)]
    pub id: i64,
    pub slug: String,
    pub path: String,
    pub project: Option<String>,
    pub is_ref: bool,
    pub parent: Option<String>,
    pub title: String,
    pub summary: String,
    pub status: Option<String>,
    pub hash: String,
}

pub const PAGE_COLS: &str =
    "p.id, p.slug, p.path, pr.name, p.is_ref, p.parent_slug, p.title, p.summary, p.status, p.hash";
pub const PAGE_FROM: &str = "pages p LEFT JOIN projects pr ON pr.id = p.project_id";

/// SQL condition (over `p` and `pr`) for pages visible from a project: its own pages,
/// shared pages, and pages associated with it in frontmatter. `?{n}` is the project name.
pub fn scope_sql(n: usize) -> String {
    format!(
        "(pr.name = ?{n} OR p.project_id IS NULL OR p.id IN (
            SELECT pp.page_id FROM page_projects pp JOIN projects x ON x.id = pp.project_id
            WHERE x.name = ?{n}))"
    )
}

/// Pages visible in `scope` (`None` = every page), main pages and refs alike.
pub fn pages_in_scope(conn: &Connection, scope: Option<&str>) -> Result<Vec<PageRow>> {
    match scope {
        Some(p) => pages_where(conn, &scope_sql(1), &[&p]),
        None => pages_where(conn, "1=1", &[]),
    }
}

pub fn page_row(r: &Row) -> rusqlite::Result<PageRow> {
    let hash: String = r.get(9)?;
    Ok(PageRow {
        id: r.get(0)?,
        slug: r.get(1)?,
        path: r.get(2)?,
        project: r.get(3)?,
        is_ref: r.get(4)?,
        parent: r.get(5)?,
        title: r.get(6)?,
        summary: r.get(7)?,
        status: r.get(8)?,
        hash: util::short(&hash).to_string(),
    })
}

pub fn page_by_slug(conn: &Connection, slug: &str) -> Result<Option<PageRow>> {
    let sql = format!("SELECT {PAGE_COLS} FROM {PAGE_FROM} WHERE p.slug = ?1");
    Ok(conn.query_row(&sql, [slug], page_row).optional()?)
}

pub fn pages_where(
    conn: &Connection,
    cond: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<PageRow>> {
    let sql = format!("SELECT {PAGE_COLS} FROM {PAGE_FROM} WHERE {cond} ORDER BY p.slug");
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(args, page_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn resolver(conn: &Connection) -> Result<Resolver> {
    let mut st = conn.prepare(
        "SELECT p.slug, pr.name FROM pages p LEFT JOIN projects pr ON pr.id = p.project_id",
    )?;
    let rows = st
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Resolver::new(rows))
}

pub fn tags(conn: &Connection, page_id: i64) -> Result<Vec<String>> {
    let mut st = conn.prepare("SELECT tag FROM tags WHERE page_id = ?1 ORDER BY tag")?;
    let rows = st
        .query_map([page_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkRow {
    pub target: String,
    pub heading: Option<String>,
    pub kind: String,
    pub resolution: String,
    pub page: Option<PageRow>,
}

pub fn outgoing(conn: &Connection, page_id: i64) -> Result<Vec<LinkRow>> {
    let mut st = conn.prepare(
        "SELECT dst_slug, heading, kind, resolution, dst_id FROM links WHERE src_id = ?1 ORDER BY rowid",
    )?;
    let raw = st
        .query_map([page_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (target, heading, kind, resolution, dst) in raw {
        let page = match dst {
            Some(id) => pages_where(conn, "p.id = ?1", &[&id])?.into_iter().next(),
            None => None,
        };
        out.push(LinkRow {
            target,
            heading,
            kind,
            resolution,
            page,
        });
    }
    Ok(out)
}

pub fn backlinks(conn: &Connection, page_id: i64) -> Result<Vec<PageRow>> {
    pages_where(
        conn,
        "p.id IN (SELECT src_id FROM links WHERE dst_id = ?1 AND src_id != ?1)",
        &[&page_id],
    )
}

pub fn refs_of(conn: &Connection, slug: &str) -> Result<Vec<PageRow>> {
    pages_where(conn, "p.is_ref = 1 AND p.parent_slug = ?1", &[&slug])
}

pub struct Event<'a> {
    pub kind: &'a str,
    pub page: Option<&'a PageRow>,
    pub project: Option<&'a str>,
    pub agent: &'a str,
    pub query: Option<&'a str>,
    pub hits: Option<i64>,
}

pub fn log_event(conn: &Connection, e: Event) -> Result<()> {
    let project_id: Option<i64> = match e.project {
        Some(name) => conn
            .query_row("SELECT id FROM projects WHERE name = ?1", [name], |r| {
                r.get(0)
            })
            .optional()?,
        None => None,
    };
    conn.execute(
        "INSERT INTO events(ts, kind, slug, page_id, project_id, agent, query, hits)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            util::now(),
            e.kind,
            e.page.map(|p| p.slug.as_str()),
            e.page.map(|p| p.id),
            project_id,
            e.agent,
            e.query,
            e.hits
        ],
    )?;
    Ok(())
}
