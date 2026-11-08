//! What the librarian is told. Kept plain and explicit for small models: one job per
//! prompt, the reply format spelled out, and everything it needs inline.

use crate::page::MAX_SUMMARY;

const INTRO: &str = "You are the librarian of a wiki of linked Markdown pages about software \
projects. Everything you need is in this message: don't use tools, don't read files and don't \
run commands.";

pub const QUERY_FORMAT: &str = "Reply in exactly this format, with nothing outside the tags:

<answer>
The answer in plain Markdown: short, specific and faithful to the pages (name the files, \
functions and settings they mention). If the pages don't cover something, say so plainly \
instead of guessing.
</answer>
<sources>
The slug of each page the answer relies on, one per line.
</sources>
<gaps>
One line for each part of the question the pages don't answer. Empty if none.
</gaps>
<need>
Slugs of pages shown only by summary whose full text you need to answer well, one per \
line. Empty if none. You will get them in a second message.
</need>";

pub fn query(question: &str, scope: &str, pages: &str) -> String {
    format!(
        "{INTRO}\n\nAnother agent, working in {scope}, asked the question below. Answer it \
         from the wiki pages that follow, which have been ranked by relevance, best first.\n\n\
         {QUERY_FORMAT}\n\nQuestion: {question}\n\n{pages}"
    )
}

pub fn query_follow_up(pages: &str) -> String {
    format!(
        "Here are the pages you asked for. Answer the question again in the same format, \
         using everything you have now, and leave <need> empty.\n\n{pages}"
    )
}

/// The page-writing guidance from the skill, without the parts about commands.
fn writing_rules() -> String {
    let full = include_str!("../install/skill/references/writing-pages.md");
    let end = full.find("\n## Updating").unwrap_or(full.len());
    full[..end]
        .lines()
        // The librarian can't run commands; refs are made by naming them, see below.
        .filter(|l| !l.contains("reg-wiki new"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn edit_format(allow_delete: bool) -> String {
    let delete = if allow_delete {
        "\n<delete slug=\"myapp/old-page\"/>\n(only to remove a page whose content you merged \
         into another; only pages shown in full, with no refs, and only if you also rewrite \
         every page shown as linking to it)"
    } else {
        ""
    };
    format!(
        "Reply in exactly this format, with nothing outside the tags:

<page slug=\"myapp/billing\">
---
title: Billing
summary: How subscriptions and invoices fit together. Read before touching src/billing.
sources: [src/billing/**]
---

# Billing
…the whole page…
</page>
(one <page> block for each page you create or replace){delete}
<message>One line saying why, for the commit message people review.</message>

Rules:
- Each <page> holds the complete file: frontmatter between `---` lines, then the body. \
`title` and `summary` are required; the summary is at most {MAX_SUMMARY} characters and says \
when to read the page. Leave out `created` and `updated`; they're set for you.
- To change an existing page, send all of it: keep what is still true, fix what is wrong. \
You may only change pages shown in full below.
- New slugs are lowercase words joined by hyphens, under a project folder \
(`myapp/retry-policy`) or `_shared/` for knowledge not tied to one project. A ref (a \
supporting page for detail) of `myapp/billing` has the slug `myapp/billing/<name>`.
- Link to other pages with [[slug]] and only to slugs that exist or that you create.
- If nothing needs to change, reply with just <none/>."
    )
}

pub fn record(scope: &str, notes: &str, pages: &str) -> String {
    format!(
        "{INTRO}\n\nAn agent working in {scope} sent the notes below about what it learned. \
         Turn what is worth keeping into wiki edits: update the page that fits best when \
         there is one, and create a page only when none does. Leave out what a later \
         session wouldn't need (a diary of the session, things obvious from the code).\n\n\
         # How to write pages\n\n{}\n\n# Reply\n\n{}\n\n# Notes\n\n{}\n\n{pages}",
        writing_rules(),
        edit_format(false),
        notes.trim()
    )
}

pub fn tend(scope: &str, report: &str, pages: &str) -> String {
    format!(
        "{INTRO}\n\nThe wiki for {scope} has the problems listed below. Fix what you can: \
         repair or remove broken links, link orphan pages from a page that should mention \
         them, rewrite summaries that don't say when to read the page, split pages that are \
         too long into a shorter main page plus refs, update stale pages only where the page \
         itself shows what is out of date (otherwise leave them), and merge pages that cover \
         the same topic. Change as little as needed.\n\n\
         # How to write pages\n\n{}\n\n# Reply\n\n{}\n\n# Problems\n\n{}\n\n{pages}",
        writing_rules(),
        edit_format(true),
        report.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_leave_out_commands() {
        let r = writing_rules();
        assert!(r.contains("summary says when to read the page"));
        assert!(!r.contains("## Updating"));
        assert!(!r.contains("reg-wiki new"));
    }
}
