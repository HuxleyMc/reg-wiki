use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use serde_json::json;

use super::{Ctx, print_json};
use crate::links::{self, Kind};
use crate::lint::{self, Fix, Issue};
use crate::page::Page;
use crate::util;

pub fn run(ctx: &mut Ctx, fix: bool) -> Result<ExitCode> {
    let mut fixed = Vec::new();
    let mut commit = None;
    if fix {
        let _lock = ctx.home.lock()?;
        ctx.sync()?;
        let (paths, applied) = apply(ctx, &ctx.lint()?)?;
        if !paths.is_empty() {
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let msg = format!(
                "lint --fix: {} fix{}",
                applied.len(),
                if applied.len() == 1 { "" } else { "es" }
            );
            commit = ctx.commit(&refs, &msg)?;
            ctx.sync()?;
        }
        fixed = applied;
    }

    let issues = ctx.lint()?;
    let (errors, warnings) = lint::counts(&issues);
    let fixable = issues.iter().filter(|i| i.fixable).count();

    if ctx.json {
        print_json(&json!({
            "issues": issues, "errors": errors, "warnings": warnings,
            "fixed": fixed, "commit": commit,
        }));
    } else {
        for f in &fixed {
            println!("fixed  {f}");
        }
        if commit.is_some() || !fixed.is_empty() {
            println!();
        }
        print(&issues);
        if issues.is_empty() {
            println!("no problems");
        } else {
            let hint = if fixable > 0 && !fix {
                format!(" ({fixable} fixable with `reg-wiki lint --fix`)")
            } else {
                String::new()
            };
            println!(
                "{errors} error{}, {warnings} warning{}{hint}",
                plural(errors),
                plural(warnings)
            );
        }
    }
    Ok(if errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

pub fn print(issues: &[Issue]) {
    let mut last = None;
    for i in issues {
        if last != Some(&i.slug) {
            println!("{}", i.slug);
            last = Some(&i.slug);
        }
        let sev = match i.severity {
            lint::Severity::Error => "error",
            lint::Severity::Warning => "warning",
        };
        println!("  {sev:<8} {:<15} {}", i.kind, i.message);
    }
}

/// Apply every fix, page by page. Returns the changed paths and a line per fix.
fn apply(ctx: &Ctx, issues: &[Issue]) -> Result<(Vec<String>, Vec<String>)> {
    let mut by_path: BTreeMap<&str, Vec<&Issue>> = BTreeMap::new();
    for i in issues.iter().filter(|i| i.fix.is_some()) {
        by_path.entry(&i.path).or_default().push(i);
    }
    let today = util::today();
    let mut paths = Vec::new();
    let mut done = Vec::new();
    for (path, list) in by_path {
        let file = ctx.home.wiki.join(path);
        let text = std::fs::read_to_string(&file)
            .with_context(|| format!("reading {}", file.display()))?;
        let mut page = Page::parse(&text)?;
        let mut fm_changed = false;
        let mut relinks = Vec::new();
        for i in &list {
            match i.fix.as_ref() {
                Some(Fix::MarkStale) => {
                    page.set("status", "stale");
                    fm_changed = true;
                    done.push(format!("{}: marked stale", i.slug));
                }
                Some(Fix::FillDates) => {
                    let updated = page.str("updated").map(str::to_string);
                    if page.get("created").is_none() {
                        page.set("created", updated.clone().unwrap_or_else(|| today.clone()));
                    }
                    if page.get("updated").is_none() {
                        page.set("updated", today.as_str());
                    }
                    fm_changed = true;
                    done.push(format!("{}: filled in dates", i.slug));
                }
                Some(Fix::Relink { from, to }) => {
                    relinks.push((from.clone(), to.clone()));
                    done.push(format!("{}: [[{from}]] → [[{to}]]", i.slug));
                }
                None => {}
            }
        }
        let mut out = if fm_changed {
            page.render()
        } else {
            text.clone()
        };
        if !relinks.is_empty() {
            let body_at = out.len() - Page::parse(&out)?.body.len();
            let body = &out[body_at..];
            let edits = links::extract_spans(body, path)
                .into_iter()
                .filter(|s| s.link.kind == Kind::Wiki)
                .filter_map(|s| {
                    let to = relinks.iter().find(|(f, _)| *f == s.link.target)?;
                    Some((s.range?, to.1.clone()))
                })
                .collect();
            out = format!("{}{}", &out[..body_at], links::apply_edits(body, edits));
        }
        if out != text {
            util::atomic_write(&file, out.as_bytes())?;
            paths.push(path.to_string());
        }
    }
    Ok((paths, done))
}
