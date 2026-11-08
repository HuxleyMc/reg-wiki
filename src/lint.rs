//! Wiki health checks, shared by `lint`, `status` and `stats`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, PageRow};
use crate::home::ProjectCfg;
use crate::links::Resolution;
use crate::page::Page;
use crate::{git, project};

/// Main pages longer than this many body lines should move detail into refs.
pub const MAIN_PAGE_LINES: usize = 150;
/// Summaries shorter than this rarely say when to read the page.
const MIN_SUMMARY: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fix {
    /// Rewrite `[[from]]` to `[[to]]`.
    Relink {
        from: String,
        to: String,
    },
    MarkStale,
    FillDates,
}

#[derive(Debug, Clone, Serialize)]
pub struct Issue {
    pub severity: Severity,
    pub slug: String,
    pub kind: &'static str,
    pub message: String,
    pub fixable: bool,
    #[serde(skip)]
    pub fix: Option<Fix>,
    #[serde(skip)]
    pub path: String,
}

pub struct Opts<'a> {
    pub scope: Option<&'a str>,
    pub main_page_lines: usize,
    pub projects: &'a [ProjectCfg],
    pub today: &'a str,
}

pub fn check(conn: &Connection, wiki: &Path, o: &Opts) -> Result<Vec<Issue>> {
    let pages = db::pages_in_scope(conn, o.scope)?;
    let resolver = db::resolver(conn)?;
    let roots: HashMap<&str, PathBuf> = o
        .projects
        .iter()
        .filter_map(|p| Some((p.name.as_str(), PathBuf::from(p.root.as_deref()?))))
        .collect();
    let mut out = Vec::new();

    for p in &pages {
        let mut add = |severity, kind, message: String, fix: Option<Fix>| {
            out.push(Issue {
                severity,
                slug: p.slug.clone(),
                kind,
                message,
                fixable: fix.is_some(),
                fix,
                path: p.path.clone(),
            })
        };
        let text = std::fs::read_to_string(wiki.join(&p.path)).unwrap_or_default();
        let page = match Page::parse(&text) {
            Ok(pg) => pg,
            Err(e) => {
                add(Severity::Error, "frontmatter", e.to_string(), None);
                continue;
            }
        };

        for e in page.validate() {
            add(Severity::Error, "frontmatter", e, None);
        }
        if let Some(s) = page.str("summary")
            && s.chars().count() < MIN_SUMMARY
        {
            add(
                Severity::Warning,
                "summary",
                format!(
                    "summary is only {} characters; say when to read the page",
                    s.chars().count()
                ),
                None,
            );
        }
        if page.has_fm && (page.get("created").is_none() || page.get("updated").is_none()) {
            add(
                Severity::Warning,
                "dates",
                "missing `created` or `updated`".into(),
                Some(Fix::FillDates),
            );
        }

        let lines = page.body.trim().lines().count();
        if !p.is_ref && lines > o.main_page_lines {
            add(
                Severity::Warning,
                "long",
                format!(
                    "{lines} lines (limit {}); move detail into refs",
                    o.main_page_lines
                ),
                None,
            );
        }

        for l in db::outgoing(conn, p.id)? {
            match l.resolution.as_str() {
                "broken" => {
                    let fix = (l.kind == "wiki")
                        .then(|| loose_match(&resolver, &l.target, p.project.as_deref()))
                        .flatten()
                        .map(|to| Fix::Relink {
                            from: l.target.clone(),
                            to,
                        });
                    let hint = match &fix {
                        Some(Fix::Relink { to, .. }) => format!("; did you mean [[{to}]]?"),
                        _ => String::new(),
                    };
                    add(
                        Severity::Error,
                        "broken-link",
                        format!("broken link [[{}]]{hint}", l.target),
                        fix,
                    );
                }
                "ambiguous" => {
                    let cands = match resolver.resolve(&l.target, p.project.as_deref()) {
                        Resolution::Ambiguous(c) => c.join(", "),
                        _ => String::new(),
                    };
                    add(
                        Severity::Error,
                        "ambiguous-link",
                        format!("ambiguous link [[{}]] could be {cands}", l.target),
                        None,
                    );
                }
                _ => {}
            }
        }

        if p.is_ref {
            if let Some(parent) = &p.parent
                && db::page_by_slug(conn, parent)?.is_none()
            {
                add(
                    Severity::Warning,
                    "orphan",
                    format!("ref of `{parent}`, which doesn't exist"),
                    None,
                );
            }
        } else if !is_hub(p, &page) && db::backlinks(conn, p.id)?.is_empty() {
            add(
                Severity::Warning,
                "orphan",
                "no page links here".into(),
                None,
            );
        }

        let sources = page.list("sources");
        if !sources.is_empty() {
            let owner = p.project.iter().cloned().chain(page.list("projects"));
            let root = owner
                .into_iter()
                .find_map(|n| roots.get(n.as_str()).cloned());
            if let Some(root) = root.filter(|r| project::git_root(r).is_some()) {
                let updated = page
                    .str("updated")
                    .or(page.str("created"))
                    .unwrap_or("0000-00-00");
                if let Some(changed) = git::last_change(&root, &sources, o.today)?
                    && changed.as_str() > updated
                {
                    let fix = (page.str("status") != Some("stale")).then_some(Fix::MarkStale);
                    add(
                        Severity::Warning,
                        "stale",
                        format!(
                            "sources ({}) changed {changed}, after the page was updated {updated}",
                            sources.join(", ")
                        ),
                        fix,
                    );
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.slug, a.severity).cmp(&(&b.slug, b.severity)));
    Ok(out)
}

/// Project hubs, `_shared/index.md`, and pages tagged `hub` are entry points, not orphans.
fn is_hub(p: &PageRow, page: &Page) -> bool {
    !p.slug.contains('/')
        || page
            .list("tags")
            .iter()
            .any(|t| t.eq_ignore_ascii_case("hub"))
}

/// `[[Stripe Webhooks]]` → `stripe-webhooks`, if that resolves.
fn loose_match(r: &crate::links::Resolver, target: &str, project: Option<&str>) -> Option<String> {
    let norm: String = target
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect();
    if norm == target {
        return None;
    }
    match r.resolve(&norm, project) {
        Resolution::Found(s) => Some(r.shortest_link(&s, project)),
        _ => None,
    }
}

pub fn counts(issues: &[Issue]) -> (usize, usize) {
    let errors = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .count();
    (errors, issues.len() - errors)
}
