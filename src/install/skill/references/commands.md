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
| `reg-wiki new <slug> --title T --summary S [--parent P] [--tag T…] [-m "why"]` | Creates a draft page with that frontmatter and a heading, commits it, and prints its hash for the following `write`. With `--parent`, creates a ref under that page. Warns about pages with similar titles. Fails if the page exists. |
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
| `reg-wiki sync` | Bring the index up to date now (every command does this anyway). |
| `reg-wiki reindex` | Rebuild the index from the files. Usage events are kept. |

## Projects

| Command | Does |
|---|---|
| `reg-wiki project add [name]` | Run inside a repo: registers it (by its normalized git remote, or its path) and creates its hub page. The name defaults to the repo's. |
| `reg-wiki project list` | Registered projects. |
| `reg-wiki project rm <name>` | Unregisters a project; its pages are kept. |

## Setup (for people)

| Command | Does |
|---|---|
| `reg-wiki install [--target claude] [--yes] [--no-permissions] [--stop-hook]` | Writes this skill, adds the hooks and `Bash(reg-wiki:*)` to `~/.claude/settings.json`, and adds a section to `~/.claude/CLAUDE.md`. Safe to run again. `--print-config claude` prints it all instead. |
| `reg-wiki uninstall` | Removes exactly what install added. |
| `reg-wiki hook <event>` | Run by Claude Code's hooks; not for direct use. |

## Files

- `~/.reg/` (or `$REG_HOME`): `config.toml`, the index `reg.db`, and `wiki/`, a git repo of Markdown files that also works as an Obsidian vault.
- Pages live at `wiki/<project>/<page>.md` or `wiki/<project>/<page>/index.md`; refs at `wiki/<project>/<page>/refs/<ref>.md`; shared pages under `wiki/_shared/`.
