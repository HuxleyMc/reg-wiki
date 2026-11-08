# Writing wiki pages

A page is read by an agent that has a task and little time. It sees the summary first and opens the page only if the summary says to. Write for that reader.

## Frontmatter

```yaml
---
title: Billing
summary: How subscriptions, invoices and Stripe webhooks fit together. Read before touching src/billing.
tags: [payments, stripe]
projects: [admin]          # optional: other projects this page belongs to
sources: [src/billing/**]  # optional: code this page describes, relative to the project root
status: stable             # draft | stable | stale
---
```

- `title` and `summary` are required. `created` and `updated` are set for you.
- **The summary says when to read the page**, in at most 200 characters: what it covers and the situation that calls for it. "Read before touching src/billing" beats "Notes about billing".
- `sources` ties the page to code. When one of those files is edited, the agent editing it is reminded about the page, and `reg-wiki lint` marks the page stale if the code changed after the page did. Globs follow git: `*` stays within a directory, `**` crosses directories, and a bare directory covers everything under it.
- `status: draft` for a first pass you're unsure of, `stable` once it's checked against the code, `stale` when you know it's out of date but can't fix it now.
- Unknown keys are kept as they are.

## The body

- **One topic per page.** If a page covers two things, split it and link the halves.
- **Lead with what matters most.** The first paragraph should answer "what is this and how does it work" in a few sentences.
- **Record the why.** The code shows *what*; the wiki is for reasons, trade-offs, constraints, history that explains odd code, and what was tried and rejected.
- **Keep main pages under about 150 lines.** Move detail (a protocol walkthrough, a long table, a troubleshooting list) into refs: `reg-wiki new <name> --parent <page> …`, then link to it from the main page.
- **Link generously.** Mention another page, link it: `[[auth]]`, `[[billing#Retries]]`, `[[_shared/rust-error-handling]]`. Links are how readers (and `context`) find related pages. Every page except a project's hub should be linked from somewhere; `lint` reports orphans.
- **Don't copy code.** Name files, functions and types (`src/billing/retry.rs`, `RetryPolicy::next`) so a reader can find them; a short snippet is fine when the shape is the point.
- **Be specific and checkable.** "Webhooks are retried 3 times with exponential backoff starting at 2 s (`RETRY_BASE`)" beats "webhooks are retried a few times".
- **Write it as current fact**, not as a story of your session. No "I found that…", no "today we…".
- Plain Markdown: headings, lists, short paragraphs, fenced code.

## Updating

- Prefer updating an existing page to adding a new one. `reg-wiki query` before `reg-wiki new`.
- Edit the page as a whole: keep what's still true, fix what's wrong, delete what's obsolete. Don't append "Update:" paragraphs.
- Always `read`, then `write --base <hash>`, so you don't overwrite someone else's change.
- Moving or renaming a page: `reg-wiki mv <old> <new>`, which rewrites every link to it.
- After a big change, `reg-wiki lint` checks links, orphans, summaries and page length; `reg-wiki lint --fix` fixes what it safely can.

## Project hubs

Each project has a hub page (its slug is the project name, e.g. `myapp`). It's the entry point: a short overview of the project and links to its main pages. When you add an important page, link it from the hub.
