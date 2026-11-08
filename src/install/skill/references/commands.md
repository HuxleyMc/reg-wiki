# reg-wiki commands

Every command reconciles the index with the files first, so hand edits are always picked up.

Options every command takes:

| Option | Does |
|---|---|
| `--json` | Machine-readable output. |
| `--project <name>` | Use this project instead of the one detected from the working directory. |
| `--all` | Cover every project, not just the current one (plus shared pages). |
| `--agent <name>` | Author recorded on writes and in usage events (env `REG_AGENT`; defaults to `claude` inside Claude Code). |

Slugs can be given in full (`myapp/billing`, `_shared/x`) or short (`billing`), which resolves like a `[[link]]` from the current project: the project's own page first, then `_shared`, then a unique basename anywhere.

## Finding

| Command | Does |
|---|---|
| `reg-wiki context <question…> [--budget 4k] [--candidates 8]` | The best-matching pages' bodies plus summaries of the pages they link to, cut to fit a token budget (`4k`, `1.5k`, `4000`). The first match is truncated rather than dropped. |
| `reg-wiki query <words…> [--limit 10] [--tag T]` | Full-text search (title, summary, tags, body; stemmed, words match as prefixes). One `slug — summary` line per hit, plus a snippet. Every word must match; if no page matches them all, pages matching some are shown, with a note. |
| `reg-wiki list [--tag T] [--refs]` | Every page in scope, one `slug — summary` line each. Refs only with `--refs`. |
| `reg-wiki read <slug>` | The page as stored, then a footer: slug, `hash`, parent, refs, links (with problems) and backlinks, each by summary. |
| `reg-wiki links <slug>` / `reg-wiki backlinks <slug>` | Pages a page links to / pages linking to it. |

`--tag` can be repeated; a page needs every tag.

## Writing

| Command | Does |
|---|---|
| `reg-wiki write <slug> -m "why" [--base <hash>]` | Replaces the page with the Markdown on stdin, validates it, sets `updated`, commits with `-m` as the message and reports broken links. Without frontmatter on stdin, the page keeps its current frontmatter. A slug that doesn't exist yet is created (needs frontmatter with `title` and `summary`). Writing identical content is a no-op. |
| `reg-wiki new <slug> --title T --summary S [--parent P] [--tag T…] [--source GLOB…] [-m "why"]` | Creates a draft page with that frontmatter (`--source` fills `sources`) and a heading, commits it, and prints its hash for the following `write`. With `--parent`, creates a ref under that page. Warns about pages with similar titles. Fails if the page exists. |
| `reg-wiki mv <old> <new> [-m "why"]` | Renames a page, with its refs and subpages, and rewrites every link to it, in one commit. Works across projects. |

`write --base <hash>` (at least 4 characters of the hash from `read`) refuses the write if the page changed since: it exits with code **3**, prints the current content on stdout and the new hash on stderr. Merge and retry with the new hash.

Writing pages, with a quoted heredoc so the shell leaves the text alone:

```sh
reg-wiki write billing --base 3f9a1c2b -m "Document retry cap" <<'EOF'
…page…
EOF
```

Validation refuses a write without `title` or `summary`, with a summary over 200 characters, with a `status` other than `draft`, `stable` or `stale`, or with `tags`, `projects` or `sources` that aren't lists of strings.

## Maintaining

| Command | Does |
|---|---|
| `reg-wiki lint [--fix]` | Broken and ambiguous links, invalid frontmatter, orphans (pages nothing links to; hubs excepted), summaries too short to say when to read the page, missing dates, main pages over the line limit (`[limits] main_page_lines`), and stale pages (a `sources` file changed after the page's `updated`). Exits 1 if there are errors. `--fix` rewrites links that only differ in spelling (`[[Stripe Webhooks]]` → `[[stripe-webhooks]]`), fills in missing dates and marks stale pages `status: stale`, in one commit. |
| `reg-wiki status` | Where the wiki is, the current project, index freshness, uncommitted (hand-edited) files and lint counts. |
| `reg-wiki stats` | Pages, refs, words, links, orphans, broken and stale pages, most linked and most read pages, recent writers, and searches that found nothing (gaps worth filling). |
| `reg-wiki view [--out PATH] [--no-open]` | Writes a self-contained HTML graph (default `view.html` in `$REG_HOME`) and opens it in the browser: nodes coloured by project and sized by links, refs as rings; click one to read the page with its refs, links and backlinks. Search box, project, tag and refs filters. `--out -` prints the HTML. For people; agents rarely need it. |
| `reg-wiki sync` | Bring the index up to date now (every command does this anyway). |
| `reg-wiki reindex` | Rebuild the index from the files. Usage events are kept. |

## Librarian

An optional agent (set up by `reg-wiki install`, configured under `[librarian]` in `config.toml`) that reads pages for you. reg-wiki ranks and reads the pages and checks and writes whatever the librarian proposes; the librarian only gets text and returns text.

| Command | Does |
|---|---|
| `reg-wiki ai query <question…>` | Ranks the pages, sends the best ones (in full, within `budget_tokens`) and the summaries of what they link to, and prints the librarian's answer, its sources (checked to exist) and the parts the wiki doesn't cover. If the librarian is off, fails or times out, prints plain `query` results with a one-line note instead, so you always get something. |
| `reg-wiki ai record [--dry-run]` | Rough notes on stdin; the librarian turns them into page edits. Each edit is checked like `write` (frontmatter, summary, links) and those that pass are committed together as `librarian (<model>)`; the rest are reported. It can only change pages it was shown in full. |
| `reg-wiki ai tend [--limit 5] [--dry-run]` | Sends what `lint` finds that needs judgment (broken links, orphans, weak summaries, long pages, likely duplicates) with the pages involved, and applies the fixes the same way. May delete a page it merged into another, only if nothing still links to it. |
| `reg-wiki ai models` | The models the librarian's agent offers (`*` marks the one in use). |

```sh
reg-wiki ai record <<'EOF'
Invoices go out on the 1st, UTC; the cron is in src/billing/schedule.rs.
Refunds take 5 days because the bank batches them.
EOF
```

## Projects

| Command | Does |
|---|---|
| `reg-wiki project add [name]` | Run inside a repo: registers it (by its normalized git remote, or its path) and creates its hub page. The name defaults to the repo's. |
| `reg-wiki project list` | Registered projects. |
| `reg-wiki project rm <name>` | Unregisters a project; its pages are kept. |

## Setup (for people)

| Command | Does |
|---|---|
| `reg-wiki install [--target claude] [--yes] [--no-permissions] [--stop-hook] [--librarian-model M] [--librarian-agent A] [--no-librarian]` | Writes this skill, adds the hooks and `Bash(reg-wiki:*)` to `~/.claude/settings.json`, adds a section to `~/.claude/CLAUDE.md`, and sets up the librarian in `config.toml` (asked interactively, with the models the agent offers; `--yes` uses the defaults without starting it). Safe to run again. `--print-config claude` prints the Claude Code part instead. |
| `reg-wiki uninstall` | Removes exactly what install added. |
| `reg-wiki hook <event>` | Run by Claude Code's hooks; not for direct use. |

## Files

- `~/.reg/` (or `$REG_HOME`): `config.toml`, the index `reg.db`, and `wiki/`, a git repo of Markdown files that also works as an Obsidian vault.
- Pages live at `wiki/<project>/<page>.md` or `wiki/<project>/<page>/index.md`; refs at `wiki/<project>/<page>/refs/<ref>.md`; shared pages under `wiki/_shared/`.
