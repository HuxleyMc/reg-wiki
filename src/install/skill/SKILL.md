---
name: reg-wiki
description: Read and write the reg-wiki, a shared wiki of linked Markdown pages that records how projects work, through the `reg-wiki` CLI. Use it when you need what earlier sessions learned (answering a question about how the code works, stuck on a problem that may have a recorded cause, picking up work across sessions or repositories), and when you learn something a later session would want (a decision and its reason, a gotcha, how parts fit together, notes on a feature in progress). Not a step to run on every task.
---

# reg-wiki

The wiki is the memory that outlives this session. Pages are Markdown files with frontmatter, linked with `[[wikilinks]]`, grouped by project, and every write is a git commit that people review. You don't need approval to write; you do need to write well, because the next agent will trust what you leave.

The current project is detected from the working directory (its git remote). Commands cover that project's pages plus shared ones (`_shared/…`). At session start you get a list of the project's pages, one `slug — summary` line each, and when a prompt matches a page, you're told about it.

## When to look

The wiki is there when you need it, not a checklist item: don't search it before every task. A small, self-contained change you can understand from the code doesn't need it. Look when:

- you're **answering a question** about how something works, why it is the way it is, or where something lives, and the answer isn't plain from the code in front of you;
- you **hit a problem** (a confusing failure, a surprising behaviour, a flaky test): a past session may have recorded the cause;
- you're **picking up earlier work**: a feature in progress, a follow-up, something a previous session left notes about;
- the work **spans several repositories** or depends on how another project behaves;
- a page in the session-start list or a "Relevant wiki pages" hint clearly covers what you're about to do.

How to look, cheapest first:
- `reg-wiki context "how do webhooks get retried"`: the best-matching pages' bodies plus summaries of what they link to, in one call, within a token budget.
- `reg-wiki query stripe webhook`: ranked list of matches (slug, summary, snippet) when you want to choose.
- `reg-wiki read <slug>`: one page in full. The footer gives its `hash` (needed to write it), its refs, links and backlinks, each with a summary. Follow those only when a summary says you need them.
- `reg-wiki ai query "how do webhooks get retried"`: if a librarian is set up, it reads the matching pages and answers in a few lines with its sources, which costs you less context than `context` when the answer is spread over many pages. Without one, you get `query` results.

Trust the wiki, but if it disagrees with the code, the code wins, and the page needs fixing.

## How to write

Prefer updating the closest existing page to adding a new one. What's worth recording is under [When to write](#when-to-write).

```sh
reg-wiki read myapp/billing                  # note the hash in the footer
reg-wiki write myapp/billing --base 3f9a1c2b7d4e -m "Explain why retries are capped at 3" <<'EOF'
---
title: Billing
summary: How subscriptions, invoices and Stripe webhooks fit together. Read before touching src/billing.
tags: [payments, stripe]
sources: [src/billing/**]
---

# Billing
…the whole page, edited…
EOF
```

If a librarian is set up, `reg-wiki ai record` with rough notes on stdin turns them into page edits for you (checked and committed like `write`); check what it reports and fix anything it got wrong.

`write` replaces the whole page with stdin. Send it without frontmatter to keep the page's current frontmatter. `--base` refuses the write if someone changed the page since you read it (exit code 3, current content on stdout): merge your change into it and retry with the new hash. `-m` says *why*; it's the commit message people review. Use a quoted heredoc (`<<'EOF'`) as above, so the shell leaves `$` and backticks alone and the command stays a single `reg-wiki` call.

To add a page, search first (`reg-wiki query`). Then `write` the new slug directly with full frontmatter (one step), or run `reg-wiki new <slug> --title … --summary … [--source 'src/billing/**']`, which commits a stub and warns about similar pages, and fill it in with `write`. If a similar page fits, update it instead. Set `sources` to the code the page describes: whoever edits those files later is reminded of the page.

## When to write

Write when you:
- made or discovered a **decision and its reason** ("we cap retries at 3 because Stripe resends anyway");
- hit a **gotcha** that cost time (an ordering constraint, a misleading name, a flaky test and its cause);
- worked out **how a part of the system fits together** that no page explains yet;
- are partway through **a larger feature** and want to keep track of what's decided, what's left and what to watch out for, so a later session (or another repository's) can pick it up;
- learned how **projects depend on each other** (an API contract, a shared config, an order to deploy in);
- found a page that is **wrong or stale**; fix it as soon as you notice.

Nothing to record is a fine outcome: most sessions don't need a write. Don't write a diary of what you did this session, copies of code, anything obvious from reading the file, or secrets. Notes on work in progress are fine when they'd help whoever continues it; mark them as such and trim them once the work lands.

When you're told an edited file is one of a page's `sources`, check whether the page still holds and update it if not.

## Slugs and links

- A slug is a page's path in the wiki without `.md`: `myapp/billing`, `_shared/rust-error-handling`. Inside a project, `billing` means `<current project>/billing`.
- Refs are supporting pages under a main page: `reg-wiki new stripe-webhooks --parent billing …` creates `myapp/billing/stripe-webhooks`. Keep main pages short and put the detail in refs.
- Link with `[[billing]]`, `[[billing#Retries]]`, `[[billing|the billing page]]`, or `[[otherproject/page]]`. `write` reports broken links; fix them.

## More

- [references/writing-pages.md](references/writing-pages.md): how to write a good page (read it before your first write).
- [references/commands.md](references/commands.md): every command and option.
- Every command takes `--json`. `--project <name>` and `--all` change the scope.
- If something seems off with the wiki, `reg-wiki status` and `reg-wiki lint` say what.
