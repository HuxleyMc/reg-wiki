//! Full-text search over the index, and packing results into a token budget.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, PageRow};
use crate::page::Page;

/// Column weights for bm25: title, summary, tags, body.
const BM25: &str = "bm25(pages_fts, 10.0, 5.0, 3.0, 1.0)";

/// Words that say nothing about what a question is about.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from",
    "how", "i", "if", "in", "is", "it", "of", "on", "or", "should", "that", "the", "this", "to",
    "we", "what", "when", "where", "which", "who", "why", "with", "you",
];

fn words(text: &str, min: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let all: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= min)
        .map(str::to_lowercase)
        .filter(|w| seen.insert(w.clone()))
        .collect();
    let content: Vec<String> = all
        .iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .cloned()
        .collect();
    // A query made only of stopwords ("how to") still searches for them.
    if content.is_empty() { all } else { content }
}

/// Turn free text into an FTS5 query that matches any of its words.
pub fn any_words(text: &str) -> Option<String> {
    let w = words(text, 3);
    (!w.is_empty()).then(|| {
        w.iter()
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
         WHERE pages_fts MATCH ?1 ORDER BY {BM25} LIMIT ?2",
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

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub page: PageRow,
    pub score: f64,
    pub snippet: String,
}

#[derive(Clone, Copy)]
pub struct Query<'a> {
    pub text: &'a str,
    pub scope: Option<&'a str>,
    pub tags: &'a [String],
    pub limit: usize,
}

/// Ranked search. Every word must match (as a prefix, after stemming); if nothing does,
/// any word may. Returns the hits and whether the strict pass matched.
pub fn query(conn: &Connection, q: &Query) -> Result<(Vec<Hit>, bool)> {
    let w = words(q.text, 1);
    if w.is_empty() {
        bail!("nothing to search for in `{}`", q.text);
    }
    let terms: Vec<String> = w.iter().map(|w| format!("\"{w}\"*")).collect();
    let all = run(conn, q, &terms.join(" "))?;
    if !all.is_empty() || terms.len() == 1 {
        return Ok((all, true));
    }
    // Fallback: any word may match, but pages matching more of the words rank first.
    // bm25 alone would let one rare word outrank a page that matches everything else.
    let wide = Query {
        limit: q.limit.saturating_mul(5).max(50),
        ..*q
    };
    let mut hits = run(conn, &wide, &terms.join(" OR "))?;
    // Per page: (words matched anywhere, words matched in title/summary/tags).
    let mut matched: HashMap<i64, (usize, usize)> = HashMap::new();
    let mut st = conn.prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH ?1")?;
    for t in &terms {
        for id in st.query_map([t], |r| r.get::<_, i64>(0))? {
            matched.entry(id?).or_default().0 += 1;
        }
        let strong = format!("{{title summary tags}} : {t}");
        for id in st.query_map([&strong], |r| r.get::<_, i64>(0))? {
            matched.entry(id?).or_default().1 += 1;
        }
    }
    let n = |h: &Hit| matched.get(&h.page.id).copied().unwrap_or_default();
    hits.sort_by(|a, b| n(b).cmp(&n(a)).then(b.score.total_cmp(&a.score)));
    hits.truncate(q.limit);
    Ok((hits, false))
}

fn run(conn: &Connection, q: &Query, fts: &str) -> Result<Vec<Hit>> {
    let mut sql = format!(
        "SELECT {}, -{BM25}, snippet(pages_fts, 3, '«', '»', '…', 12)
         FROM {} JOIN pages_fts ON pages_fts.rowid = p.id WHERE pages_fts MATCH ?1",
        db::PAGE_COLS,
        db::PAGE_FROM
    );
    let mut args: Vec<String> = vec![fts.to_string()];
    if let Some(p) = q.scope {
        args.push(p.to_string());
        sql.push_str(&format!(" AND {}", db::scope_sql(args.len())));
    }
    for t in q.tags {
        args.push(t.clone());
        sql.push_str(&format!(
            " AND p.id IN (SELECT page_id FROM tags WHERE tag = ?{})",
            args.len()
        ));
    }
    sql.push_str(&format!(" ORDER BY {BM25} LIMIT {}", q.limit));
    let mut st = conn.prepare(&sql)?;
    let hits = st
        .query_map(rusqlite::params_from_iter(&args), |r| {
            Ok(Hit {
                page: db::page_row(r)?,
                score: r.get(10)?,
                snippet: r
                    .get::<_, String>(11)?
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(hits)
}

/// Words common in prompts that say little about which page is relevant.
const PROMPT_STOPWORDS: &[&str] = &[
    "add", "also", "any", "but", "change", "changes", "code", "could", "did", "done", "file",
    "files", "fix", "function", "get", "have", "just", "let", "lib", "like", "line", "lines",
    "look", "make", "need", "not", "now", "please", "run", "see", "some", "src", "then", "there",
    "they", "try", "use", "want", "way", "will", "work", "would", "your",
];

/// Pages relevant to an agent's prompt, best first, for the UserPromptSubmit hook.
///
/// Stricter than `query`, because most prompts should match nothing: words that occur
/// in more than a third of the pages are ignored, a page must match at least one word in
/// its title, summary or tags (two of them for prompts of four or more words), and for
/// prompts of three or more words it must match two words in all. In a small wiki the
/// first rule can't apply, so the others carry the weight.
/// Project hubs are left out (the session start already lists them).
pub fn relevant(
    conn: &Connection,
    text: &str,
    scope: Option<&str>,
    limit: usize,
) -> Result<Vec<PageRow>> {
    let mut w: Vec<String> = words(text, 3)
        .into_iter()
        .filter(|w| !STOPWORDS.contains(&w.as_str()) && !PROMPT_STOPWORDS.contains(&w.as_str()))
        .collect();
    w.truncate(40);
    if w.is_empty() {
        return Ok(Vec::new());
    }
    let visible: HashMap<i64, PageRow> = db::pages_in_scope(conn, scope)?
        .into_iter()
        .filter(|p| p.slug.contains('/'))
        .map(|p| (p.id, p))
        .collect();
    let n = visible.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    #[derive(Default)]
    struct Score {
        strong: usize,
        any: usize,
        weight: f64,
    }
    let mut scores: HashMap<i64, Score> = HashMap::new();
    let mut st = conn.prepare("SELECT rowid FROM pages_fts WHERE pages_fts MATCH ?1")?;
    let mut ids = |q: &str| -> Result<HashSet<i64>> {
        let rows = st.query_map([q], |r| r.get::<_, i64>(0))?;
        let mut out = HashSet::new();
        for id in rows {
            let id = id?;
            if visible.contains_key(&id) {
                out.insert(id);
            }
        }
        Ok(out)
    };
    for word in &w {
        let any = ids(&format!("\"{word}\""))?;
        let df = any.len();
        if df == 0 || (n >= 6 && df * 3 > n) {
            continue;
        }
        let idf = ((n as f64 + 1.0) / df as f64).ln().max(0.1);
        let strong = ids(&format!("{{title summary tags}} : \"{word}\""))?;
        for id in any {
            let s = scores.entry(id).or_default();
            s.any += 1;
            s.weight += idf;
            if strong.contains(&id) {
                s.strong += 1;
                s.weight += 2.0 * idf;
            }
        }
    }
    let need_any = if w.len() <= 2 { 1 } else { 2 };
    let need_strong = if w.len() >= 4 { 2 } else { 1 };
    let mut keep: Vec<(f64, i64)> = scores
        .iter()
        .filter(|(_, s)| s.strong >= need_strong && s.any >= need_any)
        .map(|(id, s)| (s.weight, *id))
        .collect();
    keep.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(keep
        .into_iter()
        .take(limit)
        .filter_map(|(_, id)| visible.get(&id).cloned())
        .collect())
}

/// Rough token count: about four characters per token.
pub fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// `4k`, `1.5k` or `4000`.
pub fn parse_budget(s: &str) -> Result<usize> {
    let t = s.trim().to_lowercase();
    let n = match t.strip_suffix('k') {
        Some(k) => k.parse::<f64>().map(|v| v * 1000.0),
        None => t.parse::<f64>(),
    };
    match n {
        Ok(v) if v >= 1.0 => Ok(v as usize),
        _ => bail!("budget `{s}` should look like 4k, 1.5k or 4000"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextPage {
    pub slug: String,
    pub title: String,
    pub summary: String,
    pub score: f64,
    pub body: String,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct Related {
    pub slug: String,
    pub summary: String,
    /// The included page that links to it, or `match` for search hits that did not fit.
    pub via: String,
}

#[derive(Debug, Serialize)]
pub struct Context {
    pub budget: usize,
    pub used: usize,
    pub pages: Vec<ContextPage>,
    pub related: Vec<Related>,
}

pub fn section(p: &ContextPage) -> String {
    let mut s = format!(
        "## {} ({})\n{}\n\n{}\n",
        p.title,
        p.slug,
        p.summary,
        p.body.trim()
    );
    if p.truncated {
        s.push_str(&format!(
            "\n… truncated; `reg-wiki read {}` for the rest\n",
            p.slug
        ));
    }
    s
}

pub fn related_line(r: &Related) -> String {
    format!("- {} — {}\n", r.slug, r.summary)
}

/// Fill `budget` with the best hits' bodies, best first, then the summaries of what they
/// link to. A hit that doesn't fit is listed by summary instead; the first hit is cut to
/// fit rather than dropped.
pub fn context(
    conn: &Connection,
    wiki: &std::path::Path,
    hits: Vec<Hit>,
    budget: usize,
) -> Result<Context> {
    let mut ctx = Context {
        budget,
        used: 0,
        pages: Vec::new(),
        related: Vec::new(),
    };
    let mut skipped = Vec::new();
    for h in hits {
        let text = std::fs::read_to_string(wiki.join(&h.page.path)).unwrap_or_default();
        let body = Page::parse(&text).map(|p| p.body).unwrap_or(text);
        let mut cp = ContextPage {
            slug: h.page.slug.clone(),
            title: h.page.title.clone(),
            summary: h.page.summary.clone(),
            score: h.score,
            body,
            truncated: false,
        };
        let cost = tokens(&section(&cp));
        let left = budget.saturating_sub(ctx.used);
        if cost <= left {
            ctx.used += cost;
            ctx.pages.push(cp);
        } else if ctx.pages.is_empty() && left > 100 {
            let header = tokens(&section(&ContextPage {
                body: String::new(),
                truncated: true,
                ..cp.clone()
            }));
            let keep_chars = left.saturating_sub(header) * 4;
            cp.body = cut(&cp.body, keep_chars);
            cp.truncated = true;
            ctx.used += tokens(&section(&cp));
            ctx.pages.push(cp);
        } else {
            skipped.push(h.page);
        }
    }

    let mut seen: HashSet<String> = ctx.pages.iter().map(|p| p.slug.to_lowercase()).collect();
    let mut add = |ctx: &mut Context, p: &PageRow, via: &str| {
        if !seen.insert(p.slug.to_lowercase()) {
            return;
        }
        let r = Related {
            slug: p.slug.clone(),
            summary: p.summary.clone(),
            via: via.to_string(),
        };
        let cost = tokens(&related_line(&r));
        if ctx.used + cost <= budget {
            ctx.used += cost;
            ctx.related.push(r);
        }
    };
    let included: Vec<String> = ctx.pages.iter().map(|p| p.slug.clone()).collect();
    for slug in &included {
        let Some(page) = db::page_by_slug(conn, slug)? else {
            continue;
        };
        for r in db::refs_of(conn, slug)? {
            add(&mut ctx, &r, slug);
        }
        for l in db::outgoing(conn, page.id)? {
            if let Some(p) = l.page {
                add(&mut ctx, &p, slug);
            }
        }
    }
    for p in &skipped {
        add(&mut ctx, p, "match");
    }
    Ok(ctx)
}

/// The first `max` characters of `text`, cut back to a line break when there is one.
fn cut(text: &str, max: usize) -> String {
    let end = text.char_indices().nth(max).map_or(text.len(), |(i, _)| i);
    let head = &text[..end];
    match head.rfind('\n') {
        Some(i) if i > end / 2 => head[..i].to_string(),
        _ => head.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets() {
        assert_eq!(parse_budget("4k").unwrap(), 4000);
        assert_eq!(parse_budget("1.5K").unwrap(), 1500);
        assert_eq!(parse_budget("800").unwrap(), 800);
        assert!(parse_budget("lots").is_err());
    }

    #[test]
    fn cut_prefers_line_breaks() {
        assert_eq!(cut("aaaa\nbbbb\ncccc", 12), "aaaa\nbbbb");
        assert_eq!(cut("abc", 10), "abc");
    }
}
