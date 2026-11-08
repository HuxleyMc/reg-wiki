//! Full-text search over the index.

use anyhow::Result;
use rusqlite::Connection;

use crate::db::{self, PageRow};

/// Turn free text into an FTS5 query that matches any of its words.
pub fn any_words(text: &str) -> Option<String> {
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    (!words.is_empty()).then(|| {
        words
            .iter()
            .map(|w| format!("\"{w}\""))
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

/// Pages whose title or summary look like `text`, best first. Used to warn before
/// creating a duplicate.
pub fn similar(conn: &Connection, text: &str, limit: usize) -> Result<Vec<PageRow>> {
    let Some(q) = any_words(text) else {
        return Ok(Vec::new());
    };
    let sql = format!(
        "SELECT {} FROM {} JOIN pages_fts ON pages_fts.rowid = p.id
         WHERE pages_fts MATCH ?1 ORDER BY bm25(pages_fts, 10.0, 5.0, 3.0, 1.0) LIMIT ?2",
        db::PAGE_COLS,
        db::PAGE_FROM
    );
    // Search titles and summaries only: a shared word deep in a body is not a duplicate.
    let q = format!("{{title summary}} : ({q})");
    let mut st = conn.prepare(&sql)?;
    let rows = st
        .query_map(rusqlite::params![q, limit as i64], db::page_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
