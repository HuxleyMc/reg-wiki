//! Page files: frontmatter round-trip, validation and slugs.

use anyhow::{Result, bail};
use serde_yaml_ng::{Mapping, Value};

use crate::home::SHARED;

pub const MAX_SUMMARY: usize = 200;
pub const STATUSES: &[&str] = &["draft", "stable", "stale"];

/// A page split into frontmatter and body. Key order and unknown keys are kept.
#[derive(Debug, Clone)]
pub struct Page {
    pub fm: Mapping,
    /// Everything after the closing `---` line.
    pub body: String,
    pub has_fm: bool,
}

impl Page {
    pub fn parse(text: &str) -> Result<Page> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let Some(rest) = text
            .strip_prefix("---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
        else {
            return Ok(Page::raw(text));
        };
        let mut pos = 0;
        loop {
            let end = rest[pos..].find('\n').map_or(rest.len(), |i| pos + i + 1);
            let line = rest[pos..end].trim_end_matches(['\r', '\n']);
            if line == "---" || line == "..." {
                let yaml = &rest[..pos];
                let fm = if yaml.trim().is_empty() {
                    Mapping::new()
                } else {
                    match serde_yaml_ng::from_str::<Value>(yaml) {
                        Ok(Value::Mapping(m)) => m,
                        Ok(_) => bail!("frontmatter is not a mapping of keys to values"),
                        Err(e) => bail!("invalid frontmatter: {e}"),
                    }
                };
                return Ok(Page {
                    fm,
                    body: rest[end..].to_string(),
                    has_fm: true,
                });
            }
            if end == rest.len() {
                bail!("frontmatter is not closed with a `---` line");
            }
            pos = end;
        }
    }

    /// A page with no frontmatter, e.g. one whose frontmatter could not be parsed.
    pub fn raw(text: &str) -> Page {
        Page {
            fm: Mapping::new(),
            body: text.to_string(),
            has_fm: false,
        }
    }

    pub fn render(&self) -> String {
        let mut s = String::from("---\n");
        for (k, v) in &self.fm {
            s.push_str(&emit_entry(k, v));
        }
        s.push_str("---\n");
        s.push_str(&self.body);
        s
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.fm.get(key)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// A list of strings; a single string is read as a one-element list.
    pub fn list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::Sequence(seq)) => seq.iter().filter_map(scalar_string).collect(),
            Some(v) => scalar_string(v).into_iter().collect(),
            None => Vec::new(),
        }
    }

    /// Set a key, keeping its position if it already exists.
    pub fn set(&mut self, key: &str, v: impl Into<Value>) {
        self.fm.insert(Value::String(key.to_string()), v.into());
    }

    pub fn title(&self) -> Option<String> {
        self.str("title")
            .map(str::to_string)
            .or_else(|| first_heading(&self.body))
    }

    /// Problems that make the page unfit to write.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if !self.has_fm {
            errs.push("missing frontmatter (a `---` block with title and summary)".into());
            return errs;
        }
        if self.str("title").is_none() {
            errs.push("`title` is required".into());
        }
        match self.str("summary") {
            None => errs.push("`summary` is required: say when to read this page".into()),
            Some(s) if s.chars().count() > MAX_SUMMARY => errs.push(format!(
                "`summary` is {} characters; the limit is {MAX_SUMMARY}",
                s.chars().count()
            )),
            Some(_) => {}
        }
        if let Some(v) = self.get("status") {
            match v.as_str() {
                Some(s) if STATUSES.contains(&s) => {}
                _ => errs.push(format!("`status` must be one of {}", STATUSES.join(", "))),
            }
        }
        for key in ["tags", "projects", "sources"] {
            if let Some(v) = self.get(key) {
                let ok = match v {
                    Value::Sequence(seq) => seq.iter().all(|x| scalar_string(x).is_some()),
                    other => scalar_string(other).is_some(),
                };
                if !ok {
                    errs.push(format!("`{key}` must be a list of strings"));
                }
            }
        }
        errs
    }
}

fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

pub fn first_heading(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.strip_prefix("# "))
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
}

/// One `key: value` entry. Lists of scalars use flow style (`[a, b]`), as people write them.
fn emit_entry(k: &Value, v: &Value) -> String {
    if let Value::Sequence(seq) = v {
        let items: Option<Vec<String>> = seq.iter().map(flow_scalar).collect();
        if let (Some(items), Some(key)) = (items, k.as_str().and_then(plain_key)) {
            return format!("{key}: [{}]\n", items.join(", "));
        }
    }
    if let (Value::String(s), Some(key)) = (v, k.as_str().and_then(plain_key))
        && is_date(s)
    {
        return format!("{key}: {s}\n");
    }
    let mut m = Mapping::new();
    m.insert(k.clone(), v.clone());
    serde_yaml_ng::to_string(&m).unwrap_or_default()
}

fn plain_key(k: &str) -> Option<&str> {
    let ok = !k.is_empty()
        && k.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        && !k.starts_with('-');
    ok.then_some(k)
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn flow_scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let plain = serde_yaml_ng::to_string(v).ok()?;
            let plain = plain.trim_end();
            let flow_unsafe = s.contains([',', '[', ']', '{', '}', '#', '\n']);
            if flow_unsafe && !plain.starts_with(['\'', '"']) {
                serde_json::to_string(s).ok()
            } else {
                Some(plain.to_string())
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {
            Some(serde_yaml_ng::to_string(v).ok()?.trim_end().to_string())
        }
        _ => None,
    }
}

/// Where a file sits in the wiki, derived from its path.
#[derive(Debug, Clone, PartialEq)]
pub struct Loc {
    pub slug: String,
    /// Home project: the top-level folder. `None` for `_shared` and top-level files.
    pub project: Option<String>,
    pub is_ref: bool,
    /// For refs, the slug of the page they support.
    pub parent: Option<String>,
}

/// `myapp/billing/index.md` → `myapp/billing`; `myapp/billing/refs/x.md` → ref `myapp/billing/x`.
pub fn locate(rel: &str) -> Option<Loc> {
    let stem = rel.strip_suffix(".md")?;
    let mut parts: Vec<&str> = stem.split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    let project = (parts.len() > 1 && parts[0] != SHARED).then(|| parts[0].to_string());
    let mut is_ref = false;
    let mut parent = None;
    if let Some(i) = parts.iter().position(|p| *p == "refs")
        && i > 0
        && i + 1 < parts.len()
    {
        is_ref = true;
        parent = Some(parts[..i].join("/"));
        parts.remove(i);
    }
    if parts.len() > 1 && parts.last() == Some(&"index") {
        parts.pop();
    }
    Some(Loc {
        slug: parts.join("/"),
        project,
        is_ref,
        parent,
    })
}

/// Reject slugs that can't be file paths or would break wikilink syntax.
pub fn check_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("empty slug");
    }
    for part in slug.split('/') {
        if part.is_empty() || part.starts_with('.') || part == "refs" {
            bail!("invalid slug `{slug}`: bad segment `{part}`");
        }
        if part.contains(['[', ']', '|', '#', '^', '\\', ':', '*', '?', '"', '<', '>']) {
            bail!("invalid slug `{slug}`: `{part}` contains a character not allowed in page names");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
title: Billing
summary: 'How it works: read before touching src/billing.'
tags: [payments, stripe]
custom:
  nested: true
created: 2026-11-08
---

# Billing

Body.
";

    #[test]
    fn round_trip_keeps_keys_and_body() {
        let p = Page::parse(SAMPLE).unwrap();
        assert_eq!(p.str("title"), Some("Billing"));
        assert_eq!(p.list("tags"), vec!["payments", "stripe"]);
        assert_eq!(p.str("created"), Some("2026-11-08"));
        let out = p.render();
        assert_eq!(out, SAMPLE);
    }

    #[test]
    fn flow_lists_quote_when_needed() {
        let mut p = Page::parse("---\ntitle: x\n---\n").unwrap();
        p.set("tags", Value::Sequence(vec!["a, b".into(), "c".into()]));
        let out = p.render();
        assert!(out.contains(r#"tags: ["a, b", c]"#), "{out}");
        assert_eq!(Page::parse(&out).unwrap().list("tags"), vec!["a, b", "c"]);
    }

    #[test]
    fn no_frontmatter() {
        let p = Page::parse("# Hi\n").unwrap();
        assert!(!p.has_fm);
        assert_eq!(p.title().as_deref(), Some("Hi"));
        assert!(Page::parse("---\ntitle: x\n").is_err());
    }

    #[test]
    fn validation() {
        let p = Page::parse("---\ntitle: x\nstatus: done\n---\n").unwrap();
        let errs = p.validate();
        assert!(errs.iter().any(|e| e.contains("summary")));
        assert!(errs.iter().any(|e| e.contains("status")));
        let long = format!("---\ntitle: x\nsummary: {}\n---\n", "a".repeat(201));
        assert!(Page::parse(&long).unwrap().validate()[0].contains("201"));
    }

    #[test]
    fn locations() {
        let l = locate("myapp/billing/index.md").unwrap();
        assert_eq!(
            (l.slug.as_str(), l.project.as_deref(), l.is_ref),
            ("myapp/billing", Some("myapp"), false)
        );
        let l = locate("myapp/billing/refs/stripe-webhooks.md").unwrap();
        assert_eq!(l.slug, "myapp/billing/stripe-webhooks");
        assert!(l.is_ref);
        assert_eq!(l.parent.as_deref(), Some("myapp/billing"));
        assert_eq!(locate("myapp/index.md").unwrap().slug, "myapp");
        assert_eq!(
            locate("myapp/index.md").unwrap().project.as_deref(),
            Some("myapp")
        );
        let l = locate("_shared/rust.md").unwrap();
        assert_eq!((l.slug.as_str(), l.project), ("_shared/rust", None));
        assert!(locate("notes.txt").is_none());
    }
}
