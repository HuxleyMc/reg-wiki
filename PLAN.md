# reg-wiki: a living wiki for agents

`reg-wiki` is a Rust CLI for a single global wiki of linked Markdown pages. Agents write to it directly. People review the git history, ask for changes or edit pages by hand. Each page can be tied to one or more projects, so an agent working in project X sees X's pages first.

## Principles

1. **Progressive disclosure, as in skills.** An agent first sees titles and summaries. It opens a page when the summary says the page is relevant, and a page's refs when it needs the detail.
2. **Files are the source of truth and SQLite is the index.** Pages are plain `.md` files with `[[wikilinks]]`, and the wiki folder works as an Obsidian vault. SQLite holds the derived index (links, full-text search, project mapping) plus usage events that exist nowhere else. You can rebuild the index from the files at any time.
3. **Agents write without approval; git is the review.** Every write is committed with the author and reason recorded. People review with `reg-wiki log` / `reg-wiki diff` and undo with `reg-wiki revert`.
4. **Cheap for agents.** Output is compact by default and every command has `--json`. Hooks finish in under 50 ms.
5. **Search before you write.** Updating a page beats adding a duplicate.

## Storage

```
~/.reg/                         # $REG_HOME overrides
  config.toml                   # projects, hooks, librarian, limits
  reg.db                        # SQLite (WAL): index + events
  wiki/                         # git repo, Obsidian vault
    _shared/                    # pages not tied to one project
      rust-error-handling.md
    myapp/                      # one folder per project
      index.md                  # project hub
      billing/
        index.md                # page with supporting docs
        refs/stripe-webhooks.md
      auth.md
    reg/
      index.md
```

### Projects

- `reg-wiki project add [name]`, run inside a repo, registers the project. It is identified by its normalized git remote URL, which stays the same across clones and machines, with the absolute path as a fallback.
- Every command finds the current project from the cwd: go up to the git root, then match the remote or the path. `--project <name>` and `--all` override this.
- A page's folder gives its home project. Frontmatter `projects: [a, b]` adds more associations, so one `_shared` page can belong to several projects.
- `reg-wiki list`, `query` and the hooks default to the current project's pages plus `_shared`.

### Slugs and links

- A slug is the path relative to `wiki/` without `.md`, for example `myapp/billing` or `myapp/billing/stripe-webhooks` (refs drop the `refs/` part).
- `[[billing]]` resolves the way Obsidian does: first the linking page's project, then `_shared`, then a unique basename anywhere. Anything else is reported as ambiguous.
- Other forms: `[[slug#Heading]]`, `[[slug|alias]]`, `[[otherproject/slug]]`, and relative `[text](x.md)`.

### SQLite schema (first draft)

```sql
projects(id, name UNIQUE, remote, root_path, created_at)
pages(id, slug UNIQUE, path, project_id, is_ref, parent_id, title, summary,
      status, created, updated, words, chars, bytes, mtime, size, hash)
page_projects(page_id, project_id)        -- extra associations from frontmatter
tags(page_id, tag)
links(src_id, dst_slug, dst_id NULL, heading, kind)  -- dst_id NULL = broken
sources(page_id, glob)                    -- code globs, for staleness
pages_fts USING fts5(title, summary, tags, body)     -- bm25, weighted 10/5/3/1
events(id, ts, kind, page_id, project_id, agent, query, hits)
                                          -- read | write | query | ai_query | gap
```

Keeping the index fresh: every command reconciles first, comparing (mtime, size) and re-hashing on any mismatch. This is cheap because the wiki is small. `reg-wiki sync` forces a reconcile and `reg-wiki reindex` rebuilds from scratch. A file watcher isn't needed now, and `reg-wiki view --watch` could add one later.

Crate: `rusqlite` with `bundled`, which includes FTS5, so nothing is needed on the system.

### Concurrent writers

Several agents can write at once:
- Writes are atomic (write to a temp file, then rename) and committed to git under a lock file.
- `reg-wiki read` prints the page's hash. `reg-wiki write --base <hash>` refuses the write if the page changed since it was read, and returns the current content so the agent can merge.

## Page format

```markdown
---
title: Billing
summary: How subscriptions, invoices and Stripe webhooks fit together. Read before touching src/billing.
tags: [payments, stripe]
projects: [myapp]          # optional extra associations
created: 2026-11-08
updated: 2026-11-08        # set by reg
status: stable             # draft | stable | stale
sources: [src/billing/**]  # relative to the project root
---
```

- `summary` is required, at most 200 characters, and says *when* to read the page.
- Unknown frontmatter keys are kept unchanged when a file is rewritten.

## Commands

### Agent-facing (taken partly from codegraph)

| Command | Does |
|---|---|
| `reg-wiki list [--tag] [--project\|--all]` | One line per page: `slug — summary`. |
| `reg-wiki query <search> [--limit --tag --project --json]` | FTS5 BM25 ranking. Returns slug, title, summary and a snippet. |
| `reg-wiki context <question> [--budget 4k]` | Returns, in one call, the top matching pages' bodies plus the summaries of their linked pages, cut to fit a token budget (the counterpart to codegraph's `explore`). `reg-wiki ai query` is the version with an AI-written answer. |
| `reg-wiki read <slug>` | The page plus a footer: hash, refs, outgoing links and backlinks, each shown by summary. |
| `reg-wiki new <slug> --title --summary [--parent]` | Creates a page from a template. Warns about close matches. |
| `reg-wiki write <slug> [--base hash] -m "why"` (stdin) | Validates, sets `updated`, commits, and reports broken links. |
| `reg-wiki links <slug>` / `reg-wiki backlinks <slug>` | |
| `reg-wiki mv <old> <new>` | Renames a page and rewrites every link to it. |
| `reg-wiki lint [--fix]` | Broken or ambiguous links, orphans, bad summaries, main pages that are too long, and stale pages (a `sources` file changed after `updated`). |
| `reg-wiki ai query <question>` | `reg-wiki` ranks the candidate pages and the librarian writes an answer from them (see below). |
| `reg-wiki ai record` (stdin) | Rough notes go in; the librarian proposes page edits; `reg-wiki` checks them and writes them. |
| `reg-wiki ai tend` | Maintenance pass driven by `lint`: merge duplicates, split long pages, fix links. |

### User-facing

| Command | Does |
|---|---|
| `reg-wiki install [--target claude,codex,...] [--yes]` | Interactive setup: wires agents, sets up the librarian (on/off, which agent, which model). |
| `reg-wiki uninstall [--target]` | Removes everything `install` added. |
| `reg-wiki project add\|list\|rm` | Manages projects. |
| `reg-wiki status` | Index freshness, pending changes, lint counts. |
| `reg-wiki stats [--project\|--all]` | Pages, refs, words, characters, bytes, links, orphans, broken and stale pages, most-linked and most-read pages, recent writes by agent, **queries with no results** (gaps in the wiki), and growth over time. |
| `reg-wiki view [--project]` | Writes a self-contained HTML force graph and opens it in the browser. Nodes are coloured by project and sized by link count. Clicking a node shows the rendered page and its backlinks. Includes search and filters for project, tag and refs. |
| `reg-wiki log` / `reg-wiki diff` / `reg-wiki revert <commit>` | Review agent changes through git. |

## Agent integration (RTK and codegraph models)

`reg-wiki install --target claude` writes:

1. **Skill** at `~/.claude/skills/reg-wiki/`:
   - `SKILL.md`: when to read the wiki and when to write to it, and the core loop: `context`/`query` → `read` → work → `write`.
   - `references/writing-pages.md`: one topic per page; the summary says when to read it; main page under about 150 lines with detail in refs; link generously; record the *why*; no copies of code.
   - `references/commands.md`: the full command reference.
2. **Hooks**, each calling `reg-wiki hook <event>`:
   - `SessionStart`: the current project's index, capped at about 1.5k tokens with hubs first.
   - `UserPromptSubmit`: `Relevant wiki pages: …` from a fast FTS lookup. Prints nothing when there's no match.
   - `PostToolUse` on Edit/Write: when the edited file matches a page's `sources`, reminds the agent to update that page.
   - `Stop` (opt-in): if the session changed code and wrote nothing to the wiki, suggests `reg-wiki ai record`.
3. **Permissions:** adds `Bash(reg-wiki:*)` to the allow list (skipped with `--no-permissions`).
4. **Marker-fenced section** in `CLAUDE.md` / `AGENTS.md` / `GEMINI.md`, as codegraph does. `uninstall` removes it cleanly.
5. `--print-config <target>` prints the snippets so they can be set up by hand.

Other targets (codex, cursor, gemini, opencode) follow the same pattern, each with its own file locations. An MCP server (`reg-wiki serve --mcp`) is possible later; the CLI with a skill covers the need for now.

## Librarian agent (ACP)

An optional agent that reads and summarises the wiki on behalf of the main session. **`reg-wiki` handles every file read, every write and the ranking; the librarian only receives text and returns text.** It never touches the filesystem, so its permissions are simple: `reg-wiki` rejects every `session/request_permission`.

### `reg-wiki ai query "how does X work"`

```
main session ──► reg-wiki ai query "<q>"
                       1. detect project from cwd
                       2. rank: FTS5 bm25 + one hop of link expansion → top N candidates
                       3. build prompt: question, project, ranked candidates
                          (slug, title, summary, score, full body; refs as summaries)
                          cut to fit the budget, best-ranked first
                       4. ACP session → librarian model reads, answers
                       5. parse reply, check cited slugs exist, log event
main session ◄── answer + sources (slug — summary) + gaps
```

- The prompt asks for a fixed reply in simple tagged sections (`<answer>`, `<sources>`, `<gaps>`, `<need>`), which a small model follows more reliably than JSON. Because `reg-wiki` has already ranked the pages and cut them to fit, the model only has to read and summarise.
- **One follow-up round:** if `need` names a ref or linked page that was only shown as a summary, `reg-wiki` sends its full body in a second `session/prompt` on the same session. No more than one extra round.
- If `gaps` isn't empty, it is logged as a gap event (shown in `reg-wiki stats`), and the output suggests `reg-wiki ai record` once the main session works out the answer.
- **Fallback:** if the librarian is turned off, times out or returns something `reg-wiki` can't parse, `reg-wiki` prints the plain `reg-wiki query` results with a one-line note. The main session always gets an answer of some kind.

### `reg-wiki ai record` (stdin notes) and `reg-wiki ai tend`

These use the same arrangement. `reg-wiki` finds the related pages and sends them with the notes or the lint report. The librarian returns **proposed page contents** (full Markdown for each slug it wants to create or change, plus a commit message). `reg-wiki` checks them the same way as `reg-wiki write` (frontmatter, summary length, links), writes them, and commits as `librarian (<model>)`. Proposals that fail the checks are reported, not written.

### ACP details

- `reg-wiki` is the ACP client, built on the `agent-client-protocol` crate (v1.0). It starts the agent as a subprocess and talks JSON-RPC over stdio.
- **Capabilities:** `reg-wiki` advertises no `fs` or `terminal` support, and the session has `cwd` set to an empty temporary directory.
- **Model choice:** after `session/new`, `reg-wiki` reads the `configOptions` entry with category `model` and calls `session/set_config_option`. `reg-wiki install` lists the models the agent actually offers.
- **Reply:** `reg-wiki` collects `agent_message_chunk` updates until `stopReason: end_turn`. A timeout sends `session/cancel` and then falls back to `reg-wiki query`.
- **Recursion guard:** the agent runs with `REG_LIBRARIAN=1`, so the `reg-wiki` hooks do nothing inside it.
- **Latency:** v1 starts the agent fresh for each call, which takes a few seconds. A warm `reg-wiki daemon` that keeps one session open can come later, if needed.

```toml
[librarian]
enabled = true
agent = "claude"              # key into [agents.*]
model = "claude-haiku-5-5"    # smallest, cheapest model by default; checked against configOptions
budget_tokens = 12000         # kept small so a small model stays accurate
candidates = 8
timeout_secs = 90

[agents.claude]
command = "npx"
args = ["-y", "@zed-industries/claude-agent-acp"]  # check the exact package name when building
```

## Architecture

```
src/
  main.rs          # clap
  home.rs          # REG_HOME, config.toml
  project.rs       # cwd → project (git remote normalisation)
  page.rs          # frontmatter round-trip, slugs
  links.rs         # pulldown-cmark link extraction, resolution
  db/              # rusqlite: schema, migrations, reconcile, fts
  git.rs           # commit / log / diff / revert (shell out to git, or gix)
  search.rs        # query + context budgeting
  lint.rs  stats.rs
  view/            # html template, css and a small hand-written force layout (include_str!)
  hooks/           # one module per event, reads hook JSON from stdin
  install/         # per-target installers, embedded skill files
  librarian/       # ACP client, permission policy, prompts
  cmd/             # one file per subcommand
```

Crates: `clap`, `serde`, `serde_json`, `serde_yaml_ng`, `toml`, `rusqlite` (bundled), `pulldown-cmark`, `walkdir`, `globset`, `jiff`, `open`, `anyhow`, `fs4` (file locking), `agent-client-protocol`, `tokio` (only for the librarian). Tests use `assert_cmd`, `insta` and `tempfile`, with `REG_HOME` pointed at a temporary directory.

## Milestones

1. **Core:** home dir, config, projects, page round-trip, links, SQLite reconcile, `new`/`read`/`write`/`list`, git commits.
2. **Find and maintain:** `query`, `context`, `links`, `mv`, `lint`, `status`, `stats`.
3. **Agents:** skill, hooks, `install`/`uninstall` for Claude Code, then try it on real projects.
4. **View:** HTML graph.
5. **Librarian:** ACP client, `ai query` with fallback, then `ai record` and `ai tend`, and model selection during install.
6. **More:** codex, cursor and gemini targets, `serve --mcp`, scheduled `tend`.

## Out of scope for v1

- Syncing the wiki between machines. `~/.reg/wiki` stays a local git repo; you can add a remote yourself.
