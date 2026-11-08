//! Link extraction (`[[wikilinks]]` and relative `.md` links) and resolution.

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

use crate::home::SHARED;
use crate::page;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Wiki,
    Md,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Wiki => "wiki",
            Kind::Md => "md",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawLink {
    /// For wikilinks, the target as written; for `.md` links, the slug of the file.
    pub target: String,
    pub heading: Option<String>,
    pub kind: Kind,
}

/// Every link in `body`, skipping code spans and code blocks. `src_rel` is the page's
/// path relative to the wiki root, used to resolve relative `.md` links.
pub fn extract(body: &str, src_rel: &str) -> Vec<RawLink> {
    let mut code: Vec<Range<usize>> = Vec::new();
    let mut out = Vec::new();
    for (ev, range) in Parser::new_ext(body, Options::all()).into_offset_iter() {
        match ev {
            Event::Code(_)
            | Event::Start(Tag::CodeBlock(_))
            | Event::InlineHtml(_)
            | Event::Html(_) => code.push(range),
            Event::Start(Tag::Link { dest_url, .. }) => {
                if let Some(l) = md_link(&dest_url, src_rel) {
                    out.push(l);
                }
            }
            _ => {}
        }
    }

    let in_code = |i: usize| code.iter().any(|r| r.contains(&i));
    let mut wiki = Vec::new();
    let mut i = 0;
    while let Some(off) = body[i..].find("[[") {
        let start = i + off;
        let Some(len) = body[start + 2..].find("]]") else {
            break;
        };
        let inner = &body[start + 2..start + 2 + len];
        i = start + 2 + len + 2;
        if inner.contains('\n') || inner.contains("[[") || in_code(start) {
            i = start + 2;
            continue;
        }
        let target_part = inner.split('|').next().unwrap_or("");
        let (target, heading) = match target_part.split_once('#') {
            Some((t, h)) => (t, Some(h.trim().to_string()).filter(|h| !h.is_empty())),
            None => (target_part, None),
        };
        let target = target.trim();
        let target = target.strip_suffix(".md").unwrap_or(target);
        if target.is_empty() {
            continue; // [[#Heading]]: a link within the page
        }
        wiki.push(RawLink {
            target: target.to_string(),
            heading,
            kind: Kind::Wiki,
        });
    }
    wiki.extend(out);
    wiki
}

fn md_link(dest: &str, src_rel: &str) -> Option<RawLink> {
    if dest.contains("://") || dest.starts_with("mailto:") || dest.starts_with('/') {
        return None;
    }
    let (path, heading) = match dest.split_once('#') {
        Some((p, h)) => (p, Some(h.to_string()).filter(|h| !h.is_empty())),
        None => (dest, None),
    };
    if !path.ends_with(".md") {
        return None;
    }
    let path = path.replace("%20", " ");
    let mut parts: Vec<&str> = src_rel.split('/').collect();
    parts.pop(); // the source file itself
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    let loc = page::locate(&parts.join("/"))?;
    Some(RawLink {
        target: loc.slug,
        heading,
        kind: Kind::Md,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Found(String),
    Broken,
    Ambiguous(Vec<String>),
}

impl Resolution {
    pub fn label(&self) -> &'static str {
        match self {
            Resolution::Found(_) => "ok",
            Resolution::Broken => "broken",
            Resolution::Ambiguous(_) => "ambiguous",
        }
    }
}

/// Resolves link targets against the set of known pages, the way Obsidian does.
pub struct Resolver {
    /// (lowercased slug, slug, home project)
    pages: Vec<(String, String, Option<String>)>,
    exact: HashMap<String, usize>,
}

impl Resolver {
    pub fn new(pages: impl IntoIterator<Item = (String, Option<String>)>) -> Self {
        let pages: Vec<_> = pages
            .into_iter()
            .map(|(s, p)| (s.to_lowercase(), s, p))
            .collect();
        let exact = pages
            .iter()
            .enumerate()
            .map(|(i, p)| (p.0.clone(), i))
            .collect();
        Resolver { pages, exact }
    }

    pub fn exact(&self, slug: &str) -> Option<&str> {
        self.exact
            .get(&slug.to_lowercase())
            .map(|&i| self.pages[i].1.as_str())
    }

    /// Order: `<project>/t`, `t` as a full slug, `_shared/t`, then a unique match on the
    /// trailing path segments in the project, in `_shared`, and finally anywhere.
    pub fn resolve(&self, target: &str, src_project: Option<&str>) -> Resolution {
        let t = target.trim_matches('/').to_lowercase();
        let mut exact_tries = Vec::new();
        if let Some(p) = src_project {
            exact_tries.push(format!("{}/{t}", p.to_lowercase()));
        }
        exact_tries.push(t.clone());
        exact_tries.push(format!("{SHARED}/{t}"));
        for k in &exact_tries {
            if let Some(&i) = self.exact.get(k) {
                return Resolution::Found(self.pages[i].1.clone());
            }
        }

        let suffix = format!("/{t}");
        let cands: Vec<&(String, String, Option<String>)> = self
            .pages
            .iter()
            .filter(|p| p.0.ends_with(&suffix))
            .collect();
        type Tier<'a> = &'a dyn Fn(&&(String, String, Option<String>)) -> bool;
        let tiers: [Tier; 3] = [
            &|p| {
                src_project.is_some()
                    && p.2.as_deref().map(str::to_lowercase) == src_project.map(str::to_lowercase)
            },
            &|p| p.2.is_none(),
            &|_| true,
        ];
        for tier in tiers {
            let hits: Vec<String> = cands
                .iter()
                .filter(|p| tier(p))
                .map(|p| p.1.clone())
                .collect();
            match hits.len() {
                0 => continue,
                1 => return Resolution::Found(hits.into_iter().next().unwrap()),
                _ => return Resolution::Ambiguous(hits),
            }
        }
        Resolution::Broken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_wikilinks_and_md_links_outside_code() {
        let body = "See [[billing]], [[auth#Tokens|tokens]] and ![[diagram]].\n\
                    `[[not-a-link]]`\n\n```\n[[nor-this]]\n```\n\
                    Also [refs](billing/refs/stripe.md#Retries) and [web](https://x.y/a.md) and [[#local]].\n";
        let links = extract(body, "myapp/auth.md");
        let got: Vec<(&str, Option<&str>, Kind)> = links
            .iter()
            .map(|l| (l.target.as_str(), l.heading.as_deref(), l.kind))
            .collect();
        assert_eq!(
            got,
            vec![
                ("billing", None, Kind::Wiki),
                ("auth", Some("Tokens"), Kind::Wiki),
                ("diagram", None, Kind::Wiki),
                ("myapp/billing/stripe", Some("Retries"), Kind::Md),
            ]
        );
    }

    #[test]
    fn resolution_order() {
        let r = Resolver::new([
            ("myapp/billing".to_string(), Some("myapp".to_string())),
            ("other/billing".to_string(), Some("other".to_string())),
            ("_shared/rust".to_string(), None),
            (
                "myapp/billing/stripe".to_string(),
                Some("myapp".to_string()),
            ),
            ("other/x/stripe".to_string(), Some("other".to_string())),
            ("a/dup".to_string(), Some("a".to_string())),
            ("b/dup".to_string(), Some("b".to_string())),
        ]);
        let found = |s: &str| Resolution::Found(s.to_string());
        assert_eq!(r.resolve("billing", Some("myapp")), found("myapp/billing"));
        assert_eq!(r.resolve("billing", Some("other")), found("other/billing"));
        assert_eq!(r.resolve("Rust", Some("myapp")), found("_shared/rust"));
        assert_eq!(
            r.resolve("other/billing", Some("myapp")),
            found("other/billing")
        );
        assert_eq!(
            r.resolve("stripe", Some("myapp")),
            found("myapp/billing/stripe")
        );
        assert_eq!(r.resolve("stripe", Some("other")), found("other/x/stripe"));
        assert!(matches!(
            r.resolve("stripe", None),
            Resolution::Ambiguous(_)
        ));
        assert!(matches!(
            r.resolve("dup", Some("myapp")),
            Resolution::Ambiguous(_)
        ));
        assert_eq!(r.resolve("nope", Some("myapp")), Resolution::Broken);
    }
}
