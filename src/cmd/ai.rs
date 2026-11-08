//! `reg-wiki ai …`: the librarian. reg-wiki ranks and reads the pages, the librarian
//! (an ACP agent) reads the text it is sent and replies with text, and reg-wiki checks
//! and writes whatever it proposes.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;

use super::{Ctx, link_problems, print_json, warn};
use crate::db::{self, PageRow};
use crate::librarian::{Librarian, Settings, prompts, reply};
use crate::links::{self, Resolution};
use crate::page::{self, Page};
use crate::search::{self, Query, tokens};
use crate::{git, util};

fn scope_label(ctx: &Ctx) -> String {
    match ctx.scope() {
        Some(p) => format!("project `{p}` (its pages plus shared ones under `_shared/`)"),
        None => "all projects".to_string(),
    }
}

fn read_body(ctx: &Ctx, p: &PageRow) -> (String, String) {
    let text = std::fs::read_to_string(ctx.home.wiki.join(&p.path)).unwrap_or_default();
    let body = Page::parse(&text)
        .map(|pg| pg.body)
        .unwrap_or_else(|_| text.clone());
    (text, body)
}

// ---- ai query ----

#[derive(Serialize)]
struct Source {
    slug: String,
    title: String,
    summary: String,
}

pub fn query(ctx: &Ctx, question: &str, limit: usize) -> Result<ExitCode> {
    let st = Settings::load(&ctx.cfg);
    let q = Query {
        text: question,
        scope: ctx.scope(),
        tags: &[],
        limit: st.candidates.max(limit),
    };
    let (hits, strict) = search::query(&ctx.conn, &q)?;
    ctx.query_event(question, hits.len());
    if hits.is_empty() {
        return fallback(ctx, question, &hits, strict, limit, None);
    }

    let c = search::context(&ctx.conn, &ctx.home.wiki, hits.clone(), st.budget_tokens)?;
    let mut shown_full: HashSet<String> = c
        .pages
        .iter()
        .filter(|p| !p.truncated)
        .map(|p| p.slug.to_lowercase())
        .collect();
    let mut text = String::from("# Pages, best match first\n\n");
    for p in &c.pages {
        text.push_str(&format!(
            "## {} ({})\nSummary: {}\n\n{}\n",
            p.title,
            p.slug,
            p.summary,
            p.body.trim()
        ));
        if p.truncated {
            text.push_str("\n[… cut to fit; name it in <need> for the rest]\n");
        }
        text.push('\n');
    }
    if !c.related.is_empty() {
        text.push_str("# Related pages (summary only; name them in <need> for the full text)\n\n");
        for r in &c.related {
            text.push_str(&search::related_line(r));
        }
    }

    let mut lib = match st.connect() {
        Ok(l) => l,
        Err(e) => return fallback(ctx, question, &hits, strict, limit, Some(format!("{e:#}"))),
    };
    if let Some(n) = &lib.note {
        warn(n);
    }
    let prompt = prompts::query(question, &scope_label(ctx), &text);
    let mut rounds = 1;
    let mut ans = match lib.ask(&prompt).map(|r| reply::answer(&r)) {
        Ok(Some(a)) => a,
        Ok(None) => {
            let why = "the librarian's reply had no <answer> section".to_string();
            return fallback(ctx, question, &hits, strict, limit, Some(why));
        }
        Err(e) => return fallback(ctx, question, &hits, strict, limit, Some(format!("{e:#}"))),
    };

    // One follow-up round, for pages it saw only by summary.
    let mut extra = String::new();
    let mut used = 0;
    for want in &ans.need {
        let Some(p) = find(ctx, want) else { continue };
        if !shown_full.insert(p.slug.to_lowercase()) {
            continue;
        }
        let (_, body) = read_body(ctx, &p);
        let sec = format!(
            "## {} ({})\nSummary: {}\n\n{}\n\n",
            p.title,
            p.slug,
            p.summary,
            body.trim()
        );
        if used + tokens(&sec) > st.budget_tokens {
            continue;
        }
        used += tokens(&sec);
        extra.push_str(&sec);
    }
    if !extra.is_empty() {
        rounds = 2;
        match lib
            .ask(&prompts::query_follow_up(&extra))
            .map(|r| reply::answer(&r))
        {
            Ok(Some(a)) => ans = a,
            // The first answer still stands.
            Ok(None) => warn("the librarian's second reply had no <answer>; using its first"),
            Err(e) => warn(&format!(
                "the librarian's second round failed ({e:#}); using its first answer"
            )),
        }
    }

    let mut sources = Vec::new();
    let mut unknown = Vec::new();
    for s in &ans.sources {
        match find(ctx, s) {
            Some(p) if !sources.iter().any(|x: &Source| x.slug == p.slug) => {
                ctx.event("read", Some(&p));
                sources.push(Source {
                    slug: p.slug,
                    title: p.title,
                    summary: p.summary,
                });
            }
            Some(_) => {}
            None => unknown.push(s.clone()),
        }
    }
    ctx.log("ai_query", None, Some(question), Some(sources.len() as i64));
    for g in &ans.gaps {
        ctx.log("gap", None, Some(g), None);
    }
    let model = lib.model();

    if ctx.json {
        print_json(&json!({
            "question": question, "answer": ans.answer, "sources": sources, "gaps": ans.gaps,
            "unknown_sources": unknown, "model": model, "rounds": rounds, "fallback": null,
        }));
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}\n", ans.answer.trim());
    if !sources.is_empty() {
        println!("Sources:");
        for s in &sources {
            println!("- {} — {}", s.slug, s.summary);
        }
    }
    if !unknown.is_empty() {
        warn(&format!(
            "the librarian cited pages that don't exist: {}",
            unknown.join(", ")
        ));
    }
    if !ans.gaps.is_empty() {
        println!("\nNot in the wiki:");
        for g in &ans.gaps {
            println!("- {g}");
        }
        println!(
            "\nOnce you work these out, record them: `reg-wiki ai record` with your notes \
             on stdin, or `reg-wiki write`."
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// A page by slug as the librarian wrote it: exact first, then as a link would resolve.
fn find(ctx: &Ctx, s: &str) -> Option<PageRow> {
    let s = s.trim().trim_matches('/');
    let s = s.strip_suffix(".md").unwrap_or(s);
    if let Ok(Some(p)) = db::page_by_slug(&ctx.conn, s) {
        return Some(p);
    }
    ctx.find(s).ok().flatten()
}

/// Plain search results, with a one-line note saying why there's no librarian answer.
fn fallback(
    ctx: &Ctx,
    question: &str,
    hits: &[search::Hit],
    strict: bool,
    limit: usize,
    why: Option<String>,
) -> Result<ExitCode> {
    let hits = &hits[..hits.len().min(limit.max(1))];
    if ctx.json {
        print_json(&json!({
            "question": question, "answer": null, "fallback": why.as_deref().unwrap_or("no pages match"),
            "matched_all_words": strict, "hits": hits,
        }));
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(why) = why {
        eprintln!("reg-wiki: no librarian answer ({why}); showing search results instead");
    }
    super::query::print_hits(question, hits, strict);
    Ok(ExitCode::SUCCESS)
}

// ---- ai models ----

pub fn models(ctx: &Ctx) -> Result<ExitCode> {
    let mut st = Settings::load(&ctx.cfg);
    st.enabled = true;
    let lib = st.connect()?;
    let m = &lib.session.models;
    if ctx.json {
        print_json(&json!({
            "agent": st.agent, "configured": st.model, "current": lib.session.model,
            "models": m.available,
        }));
        return Ok(ExitCode::SUCCESS);
    }
    if m.available.is_empty() {
        println!("agent `{}` doesn't offer a choice of model", st.agent);
    }
    for x in &m.available {
        let mark = if lib.session.model.as_deref() == Some(x.id.as_str()) {
            "*"
        } else {
            " "
        };
        let desc = x
            .description
            .as_deref()
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        println!("{mark} {}  ({}){desc}", x.id, x.name);
    }
    if let Some(n) = &lib.note {
        warn(n);
    }
    Ok(ExitCode::SUCCESS)
}

// ---- proposals: shared by record and tend ----

/// A page the librarian was shown in full, so it may change it.
struct Shown {
    slug: String,
    hash: String,
}

/// Pages in full, as `<file>` blocks with their whole text (frontmatter too, since edits
/// must send it back), best first, within `budget`. Returns the text and what fit.
fn files_text(ctx: &Ctx, pages: &[PageRow], budget: usize) -> (String, Vec<Shown>) {
    let mut out = String::new();
    let mut shown = Vec::new();
    let mut used = 0;
    let mut seen = HashSet::new();
    for p in pages {
        if !seen.insert(p.slug.to_lowercase()) {
            continue;
        }
        let (text, _) = read_body(ctx, p);
        let block = format!(
            "<file slug=\"{}\">\n{}\n</file>\n\n",
            p.slug,
            text.trim_end()
        );
        let cost = tokens(&block);
        if used + cost > budget {
            continue;
        }
        used += cost;
        out.push_str(&block);
        shown.push(Shown {
            slug: p.slug.clone(),
            hash: util::hash_hex(text.as_bytes()),
        });
    }
    (out, shown)
}

/// Every other page in scope by summary, so links and new slugs can be checked against
/// what exists. Cut to `budget`.
fn index_text(ctx: &Ctx, shown: &[Shown], budget: usize) -> Result<String> {
    let full: HashSet<String> = shown.iter().map(|s| s.slug.to_lowercase()).collect();
    let mut out =
        String::from("# Other pages in the wiki (summary only; you can't change these)\n\n");
    let mut used = tokens(&out);
    let mut left_out = 0;
    for p in db::pages_in_scope(&ctx.conn, ctx.scope())? {
        if full.contains(&p.slug.to_lowercase()) {
            continue;
        }
        let l = format!("- {}\n", super::line(&p));
        if used + tokens(&l) > budget {
            left_out += 1;
            continue;
        }
        used += tokens(&l);
        out.push_str(&l);
    }
    if left_out > 0 {
        out.push_str(&format!("- … and {left_out} more\n"));
    }
    Ok(out)
}

#[derive(Debug, Serialize)]
struct Outcome {
    slug: String,
    /// created | updated | deleted | unchanged | refused
    action: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    link_problems: Vec<String>,
}

fn refused(slug: &str, reason: impl Into<String>) -> Outcome {
    Outcome {
        slug: slug.to_string(),
        action: "refused",
        reason: Some(reason.into()),
        link_problems: Vec::new(),
    }
}

struct Accepted {
    slug: String,
    rel: String,
    text: String,
    created: bool,
}

/// Check the librarian's edits the way `write` checks a page, then write and commit the
/// ones that pass in one commit. Edits that fail are reported, not written.
fn apply(
    ctx: &mut Ctx,
    edits: reply::Edits,
    shown: &[Shown],
    model: &str,
    dry_run: bool,
    default_message: &str,
) -> Result<(Vec<Outcome>, Option<String>)> {
    let _lock = ctx.home.lock()?;
    ctx.sync()?;
    let shown: HashMap<String, &str> = shown
        .iter()
        .map(|s| (s.slug.to_lowercase(), s.hash.as_str()))
        .collect();
    let today = util::today();
    let mut out = Vec::new();
    let mut accepted: Vec<Accepted> = Vec::new();

    // Is `p` unchanged since the librarian was shown it in full?
    let check_shown = |p: &PageRow| -> Result<(), String> {
        let Some(hash) = shown.get(&p.slug.to_lowercase()) else {
            return Err("it wasn't shown in full, so it can't be changed".into());
        };
        let now = std::fs::read(ctx.home.wiki.join(&p.path)).unwrap_or_default();
        if util::hash_hex(&now) != *hash {
            return Err("it changed while the librarian was working; run again".into());
        }
        Ok(())
    };

    for prop in edits.pages {
        let slug = ctx.qualify(&prop.slug);
        if let Err(e) = page::check_slug(&slug) {
            out.push(refused(&prop.slug, format!("{e:#}")));
            continue;
        }
        if accepted.iter().any(|a| a.slug.eq_ignore_ascii_case(&slug)) {
            out.push(refused(&slug, "proposed twice; the first one is kept"));
            continue;
        }
        let existing = db::page_by_slug(&ctx.conn, &slug)?;
        let (rel, current) = match &existing {
            Some(p) => {
                if let Err(why) = check_shown(p) {
                    out.push(refused(&p.slug, why));
                    continue;
                }
                let cur = std::fs::read_to_string(ctx.home.wiki.join(&p.path))?;
                (p.path.clone(), Some(cur))
            }
            None => (path_for_new(ctx, &slug)?, None),
        };
        let slug = existing.as_ref().map_or(slug, |p| p.slug.clone());
        if existing.is_none() && ctx.home.wiki.join(&rel).exists() {
            out.push(refused(&slug, format!("{rel} already exists")));
            continue;
        }
        let mut pg = match Page::parse(&prop.content) {
            Ok(pg) => pg,
            Err(e) => {
                out.push(refused(&slug, format!("{e:#}")));
                continue;
            }
        };
        let old = current.as_deref().and_then(|c| Page::parse(c).ok());
        if !pg.has_fm {
            out.push(refused(
                &slug,
                "no frontmatter (a `---` block with title and summary)",
            ));
            continue;
        }
        let created = old
            .as_ref()
            .and_then(|o| o.str("created"))
            .map(str::to_string)
            .unwrap_or_else(|| today.clone());
        pg.set("created", created);
        let errs = pg.validate();
        if !errs.is_empty() {
            out.push(refused(&slug, errs.join("; ")));
            continue;
        }
        if let (Some(cur), Some(o)) = (&current, &old) {
            let mut same = pg.clone();
            match o.get("updated") {
                Some(v) => same.set("updated", v.clone()),
                None => {
                    same.fm.shift_remove("updated");
                }
            }
            if same.render() == *cur {
                out.push(Outcome {
                    slug,
                    action: "unchanged",
                    reason: None,
                    link_problems: Vec::new(),
                });
                continue;
            }
        }
        pg.set("updated", today.as_str());
        accepted.push(Accepted {
            slug,
            rel,
            text: pg.render(),
            created: existing.is_none(),
        });
    }

    // Deletes: only merged-away pages, and only if nothing is left pointing at them.
    let resolver = db::resolver(&ctx.conn)?;
    let mut deletes: Vec<PageRow> = Vec::new();
    for want in &edits.deletes {
        let slug = ctx.qualify(want);
        let Some(p) = db::page_by_slug(&ctx.conn, &slug)? else {
            out.push(refused(want, "no such page"));
            continue;
        };
        if let Err(why) = check_shown(&p) {
            out.push(refused(&p.slug, why));
            continue;
        }
        if !p.slug.contains('/') {
            out.push(refused(&p.slug, "project hubs can't be deleted"));
            continue;
        }
        if accepted
            .iter()
            .any(|a| a.slug.eq_ignore_ascii_case(&p.slug))
        {
            out.push(refused(&p.slug, "also proposed as a page"));
            continue;
        }
        if !db::refs_of(&ctx.conn, &p.slug)?.is_empty() {
            out.push(refused(&p.slug, "it has refs"));
            continue;
        }
        let rewritten: HashSet<String> = accepted.iter().map(|a| a.slug.to_lowercase()).collect();
        let still_linked: Vec<String> = db::backlinks(&ctx.conn, p.id)?
            .into_iter()
            .filter(|b| !rewritten.contains(&b.slug.to_lowercase()))
            .map(|b| b.slug)
            .collect();
        if !still_linked.is_empty() {
            out.push(refused(
                &p.slug,
                format!("still linked from {}", still_linked.join(", ")),
            ));
            continue;
        }
        let linked_by_new = accepted.iter().find(|a| {
            let body = Page::parse(&a.text).map(|pg| pg.body).unwrap_or_default();
            let project = page::locate(&a.rel).and_then(|l| l.project);
            links::extract(&body, &a.rel).iter().any(|l| {
                matches!(resolver.resolve(&l.target, project.as_deref()),
                    Resolution::Found(s) if s.eq_ignore_ascii_case(&p.slug))
            })
        });
        if let Some(a) = linked_by_new {
            out.push(refused(&p.slug, format!("the new {} links to it", a.slug)));
            continue;
        }
        deletes.push(p);
    }

    for a in &accepted {
        out.push(Outcome {
            slug: a.slug.clone(),
            action: if a.created { "created" } else { "updated" },
            reason: None,
            link_problems: Vec::new(),
        });
    }
    for d in &deletes {
        out.push(Outcome {
            slug: d.slug.clone(),
            action: "deleted",
            reason: None,
            link_problems: Vec::new(),
        });
    }
    if dry_run || (accepted.is_empty() && deletes.is_empty()) {
        return Ok((out, None));
    }

    for a in &accepted {
        util::atomic_write(&ctx.home.wiki.join(&a.rel), a.text.as_bytes())?;
    }
    for d in &deletes {
        let path = ctx.home.wiki.join(&d.path);
        std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))?;
    }
    let paths: Vec<&str> = accepted
        .iter()
        .map(|a| a.rel.as_str())
        .chain(deletes.iter().map(|d| d.path.as_str()))
        .collect();
    let why = edits
        .message
        .as_deref()
        .map(|m| m.lines().next().unwrap_or(m).trim().to_string())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| default_message.to_string());
    let mut msg = match (accepted.as_slice(), deletes.as_slice()) {
        ([a], []) => format!("{}: {why}", a.slug),
        _ => why,
    };
    if paths.len() > 1 {
        msg.push_str("\n\n");
        for o in out
            .iter()
            .filter(|o| o.action != "refused" && o.action != "unchanged")
        {
            msg.push_str(&format!("- {} {}\n", o.action, o.slug));
        }
    }
    let author = git::Author::agent(&format!("librarian ({model})"));
    let commit = ctx.commit_as(&paths, &msg, &author)?;
    ctx.sync()?;
    for o in out.iter_mut() {
        if o.action != "created" && o.action != "updated" {
            continue;
        }
        if let Some(row) = db::page_by_slug(&ctx.conn, &o.slug)? {
            ctx.event("write", Some(&row));
            o.link_problems = link_problems(ctx, &row)?;
        }
    }
    Ok((out, commit))
}

/// Where a new page proposed by the librarian goes. `<page>/<name>` under an existing
/// main page (not a hub) is a ref of it, as `new --parent` would make it.
fn path_for_new(ctx: &Ctx, slug: &str) -> Result<String> {
    if let Some((parent, base)) = slug.rsplit_once('/')
        && parent.contains('/')
        && let Some(p) = db::page_by_slug(&ctx.conn, parent)?
        && !p.is_ref
    {
        return Ok(page::ref_path(&p.path, base));
    }
    Ok(ctx.path_for(slug))
}

fn report(
    ctx: &Ctx,
    verb: &str,
    out: &[Outcome],
    commit: Option<&str>,
    model: &str,
    dry_run: bool,
) {
    if ctx.json {
        print_json(&json!({
            "action": verb, "dry_run": dry_run, "model": model, "changes": out, "commit": commit,
        }));
        return;
    }
    if out.is_empty() {
        println!("the librarian proposed no changes");
        return;
    }
    for o in out {
        let action = if dry_run && o.action != "refused" && o.action != "unchanged" {
            format!("would be {}", o.action)
        } else {
            o.action.to_string()
        };
        match &o.reason {
            Some(r) => println!("{action} {}: {r}", o.slug),
            None => println!("{action} {}", o.slug),
        }
        for p in &o.link_problems {
            warn(&format!("{}: {p}", o.slug));
        }
    }
    if let Some(c) = commit {
        println!(
            "commit {c} by librarian ({model}); review with `git -C {} show {c}`",
            ctx.home.wiki.display()
        );
    }
}

fn connect(st: &Settings) -> Result<Librarian> {
    let lib = st.connect()?;
    if let Some(n) = &lib.note {
        warn(n);
    }
    Ok(lib)
}

// ---- ai record ----

pub fn record(ctx: &mut Ctx, dry_run: bool) -> Result<ExitCode> {
    let mut notes = String::new();
    std::io::stdin()
        .read_to_string(&mut notes)
        .context("reading notes from stdin")?;
    if notes.trim().is_empty() {
        bail!("no notes on stdin: send what you learned, e.g. `reg-wiki ai record <<'EOF' … EOF`");
    }
    let st = Settings::load(&ctx.cfg);
    let q = Query {
        text: &notes,
        scope: ctx.scope(),
        tags: &[],
        limit: st.candidates,
    };
    let (hits, _) = search::query(&ctx.conn, &q)?;
    let mut candidates: Vec<PageRow> = hits.into_iter().map(|h| h.page).collect();
    // The hub, so new pages can be linked from it.
    if let Some(p) = ctx.current.as_deref()
        && let Some(hub) = db::page_by_slug(&ctx.conn, p)?
    {
        candidates.push(hub);
    }
    let budget = st.budget_tokens;
    let (files, shown) = files_text(ctx, &candidates, budget * 3 / 4);
    let index = index_text(ctx, &shown, budget / 4)?;
    let pages = format!("# Pages you may update (shown in full)\n\n{files}{index}");
    let prompt = prompts::record(&scope_label(ctx), &notes, &pages);

    let mut lib = connect(&st).context(
        "the librarian is needed to record notes; record them yourself with `reg-wiki write`",
    )?;
    let model = lib.model();
    let text = lib.ask(&prompt)?;
    drop(lib);
    let edits = reply::edits(&text);
    if edits.none || (edits.pages.is_empty() && edits.deletes.is_empty()) {
        report(ctx, "record", &[], None, &model, dry_run);
        return Ok(ExitCode::SUCCESS);
    }
    let edits = reply::Edits {
        deletes: Vec::new(),
        ..edits
    };
    let (out, commit) = apply(ctx, edits, &shown, &model, dry_run, "record notes")?;
    report(ctx, "record", &out, commit.as_deref(), &model, dry_run);
    Ok(ExitCode::SUCCESS)
}

// ---- ai tend ----

/// Pages whose titles and summaries share most of their words.
fn duplicates(ctx: &Ctx) -> Result<Vec<(PageRow, PageRow)>> {
    let pages: Vec<PageRow> = db::pages_in_scope(&ctx.conn, ctx.scope())?
        .into_iter()
        .filter(|p| p.slug.contains('/') && !p.is_ref)
        .collect();
    let words = |p: &PageRow| -> HashSet<String> {
        format!("{} {}", p.title, p.summary)
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_lowercase)
            .filter(|w| !search::is_stopword(w))
            .collect()
    };
    let sets: Vec<HashSet<String>> = pages.iter().map(words).collect();
    let mut out = Vec::new();
    for i in 0..pages.len() {
        for j in i + 1..pages.len() {
            let (a, b) = (&sets[i], &sets[j]);
            let inter = a.intersection(b).count();
            let union = a.union(b).count();
            if union >= 4 && inter * 2 >= union {
                out.push((pages[i].clone(), pages[j].clone()));
            }
        }
    }
    Ok(out)
}

pub fn tend(ctx: &mut Ctx, limit: usize, dry_run: bool) -> Result<ExitCode> {
    let st = Settings::load(&ctx.cfg);
    let issues = ctx.lint()?;
    let fixable = issues.iter().filter(|i| i.fixable).count();
    let mut problems: Vec<(String, String)> = issues
        .iter()
        .filter(|i| !i.fixable)
        .map(|i| (i.slug.clone(), format!("[{}] {}", i.kind, i.message)))
        .collect();
    for (a, b) in duplicates(ctx)? {
        problems.push((
            a.slug.clone(),
            format!("[duplicate?] may cover the same topic as {}", b.slug),
        ));
    }
    if problems.is_empty() {
        let hint = if fixable > 0 {
            format!("; {fixable} issue(s) can be fixed with `reg-wiki lint --fix`")
        } else {
            String::new()
        };
        if ctx.json {
            print_json(
                &json!({ "action": "tend", "changes": [], "commit": null, "fixable": fixable }),
            );
        } else {
            println!("nothing for the librarian to tend{hint}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // The pages with problems, most problems first, up to `limit` of them.
    let mut order: Vec<String> = Vec::new();
    let mut count: HashMap<String, usize> = HashMap::new();
    for (s, _) in &problems {
        if !count.contains_key(s) {
            order.push(s.clone());
        }
        *count.entry(s.clone()).or_default() += 1;
    }
    order.sort_by_key(|s| std::cmp::Reverse(count[s]));
    order.truncate(limit.max(1));
    let chosen: HashSet<&String> = order.iter().collect();
    let mut report_text = String::new();
    for (s, m) in &problems {
        if chosen.contains(s) {
            report_text.push_str(&format!("- {s}: {m}\n"));
        }
    }

    // In full: the pages with problems, then what fixing them needs: the other half of a
    // duplicate pair, the pages linking to it, and the hub an orphan could be linked from.
    let mut full: Vec<PageRow> = Vec::new();
    let mut more: Vec<PageRow> = Vec::new();
    for s in &order {
        let Some(p) = db::page_by_slug(&ctx.conn, s)? else {
            continue;
        };
        for (ps, m) in &problems {
            if ps != s {
                continue;
            }
            if let Some(other) = m.strip_prefix("[duplicate?] may cover the same topic as ")
                && let Some(o) = db::page_by_slug(&ctx.conn, other)?
            {
                more.extend(db::backlinks(&ctx.conn, o.id)?);
                more.push(o);
            }
            if m.starts_with("[orphan]")
                && let Some(proj) = &p.project
                && let Some(hub) = db::page_by_slug(&ctx.conn, proj)?
            {
                more.push(hub);
            }
        }
        full.push(p);
    }
    full.extend(more);
    let budget = st.budget_tokens;
    let (files, shown) = files_text(ctx, &full, budget * 3 / 4);
    let index = index_text(ctx, &shown, budget / 4)?;
    let pages = format!("# Pages you may change (shown in full)\n\n{files}{index}");
    let prompt = prompts::tend(&scope_label(ctx), &report_text, &pages);

    let mut lib = connect(&st).context(
        "the librarian is needed to tend the wiki; `reg-wiki lint` lists what to fix by hand",
    )?;
    let model = lib.model();
    let text = lib.ask(&prompt)?;
    drop(lib);
    let edits = reply::edits(&text);
    if edits.none || (edits.pages.is_empty() && edits.deletes.is_empty()) {
        report(ctx, "tend", &[], None, &model, dry_run);
        return Ok(ExitCode::SUCCESS);
    }
    let (out, commit) = apply(ctx, edits, &shown, &model, dry_run, "tend the wiki")?;
    report(ctx, "tend", &out, commit.as_deref(), &model, dry_run);
    if fixable > 0 && !ctx.json {
        println!("{fixable} more issue(s) can be fixed with `reg-wiki lint --fix`");
    }
    Ok(ExitCode::SUCCESS)
}
