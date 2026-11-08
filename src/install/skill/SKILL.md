---
name: reg-wiki
description: Read and write the reg-wiki, a shared wiki of linked Markdown pages that records how projects work, through the `reg-wiki` CLI. Use it before working on a part of a codebase (to learn what is already known about it), and after learning something a later session would want (a decision and its reason, a gotcha, how parts fit together), to record it.
---

# reg-wiki

The wiki is the memory that outlives this session. Pages are Markdown files with frontmatter, linked with `[[wikilinks]]`, grouped by project, and every write is a git commit that people review. You don't need approval to write; you do need to write well, because the next agent will trust what you leave.

The current project is detected from the working directory (its git remote). Commands cover that project's pages plus shared ones (`_shared/…`). At session start you get a list of the project's pages, one `slug — summary` line each, and when a prompt matches a page, you're told about it.

## The loop

1. **Look before you work.** Before you change or explain a part of the code, check what the wiki knows:
   - `reg-wiki context "how do webhooks get retried"`: the best-matching pages' bodies plus summaries of what they link to, in one call, within a token budget. Usually the best first step.
   - `reg-wiki query stripe webhook`: ranked list of matches (slug, summary, snippet) when you want to choose.
   - `reg-wiki read <slug>`: one page in full. The footer gives its `hash` (needed to write it), its refs, links and backlinks, each with a summary. Follow those only when a summary says you need them.
2. **Do the work.** Trust the wiki, but if it disagrees with the code, the code wins, and the page needs fixing.
3. **Write back what you learned**, before you finish: anything that took real effort to find out, or that you'd want to know next time. Update the closest existing page rather than adding a new one.

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

`write` replaces the whole page with stdin. Send it without frontmatter to keep the page's current frontmatter. `--base` refuses the write if someone changed the page since you read it (exit code 3, current content on stdout): merge your change into it and retry with the new hash. `-m` says *why*; it's the commit message people review. Use a quoted heredoc (`<<'EOF'`) as above, so the shell leaves `$` and backticks alone and the command stays a single `reg-wiki` call.

To add a page, search first (`reg-wiki query`). Then `write` the new slug directly with full frontmatter (one step), or run `reg-wiki new <slug> --title … --summary … [--source 'src/billing/**']`, which commits a stub and warns about similar pages, and fill it in with `write`. If a similar page fits, update it instead. Set `sources` to the code the page describes: whoever edits those files later is reminded of the page.

## When to write

Write when you:
- made or discovered a **decision and its reason** ("we cap retries at 3 because Stripe resends anyway");
- hit a **gotcha** that cost time (an ordering constraint, a misleading name, a flaky test and its cause);
- worked out **how a part of the system fits together** that no page explains yet;
- found a page that is **wrong or stale**; fix it as soon as you notice.

Don't write: a diary of what you did this session, copies of code, anything obvious from reading the file, or secrets.

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
