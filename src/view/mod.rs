//! `reg-wiki view`: a self-contained HTML page with a force graph of the wiki.
//!
//! Everything the page needs (pages rendered to HTML, links, styles, script) is
//! embedded in one file, so it opens from disk with no server and no network.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag, TagEnd, html};
use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, PageRow};
use crate::home::SHARED;
use crate::links::{self, Resolution, Resolver};
use crate::page::Page;

const TEMPLATE: &str = include_str!("template.html");
const STYLE: &str = include_str!("style.css");
const SCRIPT: &str = include_str!("app.js");

#[derive(Serialize)]
pub struct Node {
    pub slug: String,
    pub title: String,
    pub summary: String,
    /// Home project, or `_shared`.
    pub project: String,
    /// Extra associations from frontmatter `projects:`.
    pub also: Vec<String>,
    pub is_ref: bool,
    pub parent: Option<String>,
    pub hub: bool,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub updated: Option<String>,
    pub words: i64,
    /// The body rendered to HTML; links to pages point at `#p=<slug>`.
    pub html: String,
}

#[derive(Serialize)]
pub struct Edge {
    /// Indexes into `nodes`.
    pub s: usize,
    pub t: usize,
    /// `link`, or `ref` for a ref's tie to its parent page.
    pub kind: &'static str,
}

#[derive(Serialize)]
pub struct Graph {
    pub scope: Option<String>,
    pub generated: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Links whose target is missing or ambiguous, in pages shown.
    pub broken: usize,
}

/// Collect the pages visible in `scope` (`None` = all) and the links among them.
pub fn graph(conn: &Connection, wiki: &Path, scope: Option<&str>) -> Result<Graph> {
    let pages = db::pages_in_scope(conn, scope)?;
    let resolver = db::resolver(conn)?;
    let index: HashMap<i64, usize> = pages.iter().enumerate().map(|(i, p)| (p.id, i)).collect();
    let by_slug: HashMap<String, usize> = pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.slug.to_lowercase(), i))
        .collect();
    let extras = extras(conn)?;

    let mut nodes = Vec::with_capacity(pages.len());
    for p in &pages {
        let text = std::fs::read_to_string(wiki.join(&p.path)).unwrap_or_default();
        let page = Page::parse(&text).unwrap_or_else(|_| Page::raw(&text));
        let tags = db::tags(conn, p.id)?;
        let ex = extras.get(&p.id);
        nodes.push(Node {
            slug: p.slug.clone(),
            title: p.title.clone(),
            summary: p.summary.clone(),
            project: p.project.clone().unwrap_or_else(|| SHARED.to_string()),
            also: ex.map(|e| e.also.clone()).unwrap_or_default(),
            is_ref: p.is_ref,
            parent: p.parent.clone(),
            hub: is_hub(p, &tags),
            status: p.status.clone(),
            updated: ex.and_then(|e| e.updated.clone()),
            words: ex.map_or(0, |e| e.words),
            html: render(&page.body, &p.title, p, &resolver, wiki),
            tags,
        });
    }

    let mut seen = HashSet::new();
    let mut edges = Vec::new();
    let mut st = conn.prepare("SELECT src_id, dst_id, resolution FROM links")?;
    let rows = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut broken = 0;
    for (src, dst, res) in rows {
        let Some(&s) = index.get(&src) else { continue };
        if res != "ok" {
            broken += 1;
            continue;
        }
        let Some(&t) = dst.and_then(|d| index.get(&d)) else {
            continue;
        };
        if s != t && seen.insert((s.min(t), s.max(t))) {
            edges.push(Edge { s, t, kind: "link" });
        }
    }
    for (i, p) in pages.iter().enumerate() {
        if let Some(&t) = p
            .parent
            .as_ref()
            .and_then(|s| by_slug.get(&s.to_lowercase()))
        {
            let key = (i.min(t), i.max(t));
            if seen.insert(key) {
                edges.push(Edge {
                    s: t,
                    t: i,
                    kind: "ref",
                });
            } else if let Some(e) = edges
                .iter_mut()
                .find(|e| (e.s.min(e.t), e.s.max(e.t)) == key)
            {
                e.kind = "ref";
            }
        }
    }

    Ok(Graph {
        scope: scope.map(str::to_string),
        generated: crate::util::now(),
        nodes,
        edges,
        broken,
    })
}

struct Extra {
    also: Vec<String>,
    updated: Option<String>,
    words: i64,
}

fn extras(conn: &Connection) -> Result<HashMap<i64, Extra>> {
    let mut out: HashMap<i64, Extra> = HashMap::new();
    let mut st = conn.prepare("SELECT id, updated, words FROM pages")?;
    for r in st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (id, updated, words) = r?;
        out.insert(
            id,
            Extra {
                also: Vec::new(),
                updated,
                words,
            },
        );
    }
    let mut st = conn.prepare(
        "SELECT pp.page_id, x.name FROM page_projects pp JOIN projects x ON x.id = pp.project_id
         ORDER BY x.name",
    )?;
    for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, name) = r?;
        if let Some(e) = out.get_mut(&id) {
            e.also.push(name);
        }
    }
    Ok(out)
}

/// Project hubs and pages tagged `hub` (the same rule `lint` uses for entry points).
fn is_hub(p: &PageRow, tags: &[String]) -> bool {
    !p.slug.contains('/') || tags.iter().any(|t| t.eq_ignore_ascii_case("hub"))
}

/// Render a page body to HTML for the side panel.
///
/// - Links to pages (`[[wikilinks]]` and relative `.md` links) become `#p=<slug>`, or
///   `#missing=<target>` when they don't resolve, so the page script can follow them.
/// - Raw HTML is shown as text: pages are written by agents, and the view must not run
///   whatever they contain.
/// - A leading `# Title` that repeats the page title is dropped; the panel shows it.
pub fn render(body: &str, title: &str, page: &PageRow, resolver: &Resolver, wiki: &Path) -> String {
    let body = strip_title(body, title);
    let project = page.project.as_deref();
    let mut ends: Vec<bool> = Vec::new(); // per open link/image: was an image turned into a link?
    let events = Parser::new_ext(body, Options::all()).map(|ev| match ev {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            ends.push(false);
            let dest = rewrite_link(&dest_url, link_type, page, project, resolver);
            Event::Start(Tag::Link {
                link_type,
                dest_url: dest.map_or(dest_url, CowStr::from),
                title,
                id,
            })
        }
        // `![[diagram]]` embeds a page; show it as a link to that page.
        Event::Start(Tag::Image {
            link_type: link_type @ LinkType::WikiLink { .. },
            dest_url,
            title,
            id,
        }) => {
            ends.push(true);
            let dest = rewrite_link(&dest_url, link_type, page, project, resolver)
                .unwrap_or_else(|| dest_url.to_string());
            Event::Start(Tag::Link {
                link_type,
                dest_url: dest.into(),
                title,
                id,
            })
        }
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => {
            ends.push(false);
            let dest = local_file_url(&dest_url, &page.path, wiki).unwrap_or(dest_url);
            Event::Start(Tag::Image {
                link_type,
                dest_url: dest,
                title,
                id,
            })
        }
        Event::End(TagEnd::Image) if ends.pop() == Some(true) => Event::End(TagEnd::Link),
        Event::End(TagEnd::Link) => {
            ends.pop();
            ev
        }
        ev => ev,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn strip_title<'a>(body: &'a str, title: &str) -> &'a str {
    let trimmed = body.trim_start();
    if let Some(rest) = trimmed.strip_prefix("# ") {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        if line.trim().eq_ignore_ascii_case(title.trim()) {
            return after;
        }
    }
    body
}

fn rewrite_link(
    dest: &str,
    link_type: LinkType,
    page: &PageRow,
    project: Option<&str>,
    resolver: &Resolver,
) -> Option<String> {
    if !matches!(link_type, LinkType::WikiLink { .. }) {
        let l = links::md_link(dest, &page.path)?;
        return Some(format!("#p={}", encode(&l.target)));
    }
    let t = dest.split('#').next().unwrap_or("").trim();
    if t.is_empty() {
        return Some("#".to_string()); // [[#Heading]]: a link within the page
    }
    let target = t.strip_suffix(".md").unwrap_or(t);
    Some(match resolver.resolve(target, project) {
        Resolution::Found(s) => format!("#p={}", encode(&s)),
        _ => format!("#missing={}", encode(target)),
    })
}

/// A relative image path in the wiki, as a `file://` URL the page can load.
fn local_file_url<'a>(dest: &str, src_rel: &str, wiki: &Path) -> Option<CowStr<'a>> {
    if dest.contains(':') || dest.starts_with('#') || dest.starts_with("//") {
        return None;
    }
    let rel = links::join_rel(src_rel, dest)?;
    let abs = wiki.join(rel);
    let mut url = String::from("file://");
    for c in abs.to_string_lossy().replace('\\', "/").chars() {
        match c {
            ' ' => url.push_str("%20"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            c => url.push(c),
        }
    }
    if !url.starts_with("file:///") {
        url.insert(7, '/'); // Windows drive paths: file:///C:/...
    }
    Some(url.into())
}

/// Percent-encode the characters that would break a URL fragment.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The complete HTML document.
pub fn document(g: &Graph) -> Result<String> {
    // `</` inside a <script> would end it early; `\u003c` is the same character to JSON.
    let data = serde_json::to_string(g)?.replace('<', "\\u003c");
    let title = match &g.scope {
        Some(p) => format!("{p} · reg-wiki"),
        None => "reg-wiki".to_string(),
    };
    Ok(TEMPLATE
        .replace("{{TITLE}}", &escape(&title))
        .replace("{{STYLE}}", STYLE)
        .replace("{{SCRIPT}}", SCRIPT)
        .replace("{{DATA}}", &data))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(slug: &str, path: &str, project: Option<&str>) -> PageRow {
        PageRow {
            id: 1,
            slug: slug.into(),
            path: path.into(),
            project: project.map(str::to_string),
            is_ref: false,
            parent: None,
            title: "Auth".into(),
            summary: String::new(),
            status: None,
            hash: String::new(),
        }
    }

    #[test]
    fn renders_links_to_pages_and_escapes_html() {
        let r = Resolver::new([
            ("myapp/auth".to_string(), Some("myapp".to_string())),
            ("myapp/billing".to_string(), Some("myapp".to_string())),
            ("_shared/rust".to_string(), None),
        ]);
        let p = row("myapp/auth", "myapp/auth.md", Some("myapp"));
        let body = "# Auth\n\nSee [[billing|bills]], [[rust#Errors]], [[nope]] and \
                    [b](billing.md).\n\n<script>alert(1)</script>\n\n![[billing]]\n";
        let html = render(body, "Auth", &p, &r, Path::new("/w"));
        assert!(!html.contains("<h1>"), "{html}");
        assert!(
            html.contains(r##"<a href="#p=myapp/billing">bills</a>"##),
            "{html}"
        );
        assert!(html.contains(r##"href="#p=_shared/rust""##), "{html}");
        assert!(html.contains(r##"href="#missing=nope""##), "{html}");
        assert_eq!(
            html.matches(r##"href="#p=myapp/billing""##).count(),
            3,
            "{html}"
        );
        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(!html.contains("<img"), "{html}");
    }

    #[test]
    fn local_images_load_from_the_wiki() {
        let r = Resolver::new([]);
        let p = row("myapp/auth", "myapp/auth.md", Some("myapp"));
        let html = render(
            "![d](<img/flow chart.png>) ![w](https://x.y/a.png)",
            "Other",
            &p,
            &r,
            Path::new("/w"),
        );
        assert!(
            html.contains(r#"src="file:///w/myapp/img/flow%20chart.png""#),
            "{html}"
        );
        assert!(html.contains(r#"src="https://x.y/a.png""#), "{html}");
    }
}
