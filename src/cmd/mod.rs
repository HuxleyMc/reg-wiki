//! Subcommands and the context they share.

mod ai;
mod links;
mod lint;
mod list;
mod mv;
mod new;
mod project;
mod query;
mod read;
mod schedule;
mod stats;
mod status;
mod sync;
mod view;
mod write;

use std::cell::OnceCell;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, bail};
use rusqlite::Connection;

use crate::db::{self, PageRow, reconcile};
use crate::home::{Config, Home, SHARED};
use crate::links::Resolution;
use crate::{AiCmd, Cli, Command, ProjectCmd, git, util};

pub fn run(cli: Cli) -> Result<ExitCode> {
    // These run before the index is opened: hooks must never fail the agent's session,
    // and install/uninstall only touch the agent's config.
    match &cli.cmd {
        Command::Hook { event } => return Ok(crate::hooks::run(&cli, event)),
        Command::Install(a) => return crate::install::install(&cli, a),
        Command::Uninstall(a) => return crate::install::uninstall(&cli, a),
        Command::Serve { .. } => return crate::serve::run(&cli),
        _ => {}
    }
    let cwd = std::env::current_dir()?;
    let mut ctx = Ctx::open(&cli, cwd)?;
    match cli.cmd {
        Command::List { tag, refs } => list::run(&ctx, &tag, refs),
        Command::Read { slug } => read::run(&ctx, &slug),
        Command::New {
            slug,
            title,
            summary,
            parent,
            tags,
            sources,
            message,
        } => new::run(
            &mut ctx,
            new::Args {
                slug,
                title,
                summary,
                parent,
                tags,
                sources,
                message,
            },
        ),
        Command::Write {
            slug,
            base,
            message,
        } => write::run(&mut ctx, &slug, base.as_deref(), &message),
        Command::Ai { cmd } => match cmd {
            AiCmd::Query { question, limit } => ai::query(&ctx, &question.join(" "), limit),
            AiCmd::Record { dry_run } => ai::record(&mut ctx, dry_run),
            AiCmd::Tend { limit, dry_run } => ai::tend(&mut ctx, limit, dry_run),
            AiCmd::Models => ai::models(&ctx),
            AiCmd::Schedule { when, at } => schedule::run(&ctx, when.as_deref(), at.as_deref()),
        },
        Command::Project { cmd } => match cmd {
            ProjectCmd::Add { name } => project::add(&mut ctx, name.as_deref()),
            ProjectCmd::List => project::list(&ctx),
            ProjectCmd::Rm { name } => project::rm(&mut ctx, &name),
        },
        Command::Query { search, limit, tag } => query::query(&ctx, &search.join(" "), limit, &tag),
        Command::Context {
            question,
            budget,
            candidates,
        } => query::context(&ctx, &question.join(" "), budget.as_deref(), candidates),
        Command::Links { slug } => links::links(&ctx, &slug),
        Command::Backlinks { slug } => links::backlinks(&ctx, &slug),
        Command::Mv { old, new, message } => mv::run(&mut ctx, &old, &new, message.as_deref()),
        Command::Lint { fix } => lint::run(&mut ctx, fix),
        Command::Status => status::run(&ctx),
        Command::Stats => stats::run(&ctx),
        Command::View { out, no_open } => view::run(&ctx, out, no_open),
        Command::Sync => sync::sync(&ctx),
        Command::Reindex => sync::reindex(&mut ctx),
        Command::Hook { .. }
        | Command::Install(_)
        | Command::Uninstall(_)
        | Command::Serve { .. } => unreachable!(),
    }
}

pub struct Ctx {
    pub home: Home,
    pub cfg: Config,
    pub conn: Connection,
    pub json: bool,
    pub cwd: PathBuf,
    /// The project in effect: `--project`, or the one detected from the cwd.
    pub current: Option<String>,
    pub all: bool,
    pub stats: reconcile::Stats,
    agent_flag: Option<String>,
    agent: OnceCell<String>,
}

impl Ctx {
    /// Open the index and find the current project from `cwd`.
    pub fn open(cli: &Cli, cwd: PathBuf) -> Result<Ctx> {
        let home = Home::locate()?;
        home.ensure()?;
        let cfg = Config::load(&home.config)?;
        let mut conn = db::open(&home.db)?;
        let stats = reconcile::reconcile(&mut conn, &home.wiki, &cfg.projects())?;
        warn_all(&stats.warnings);
        let current = match &cli.project {
            Some(p) => Some(p.clone()),
            None => crate::project::detect(&cfg.projects(), &cwd).map(|p| p.name),
        };
        let ctx = Ctx {
            home,
            cfg,
            conn,
            json: cli.json,
            cwd,
            current,
            all: cli.all,
            stats,
            agent_flag: cli.agent.clone(),
            agent: OnceCell::new(),
        };
        if let Some(p) = &cli.project
            && !ctx.is_project(p)
        {
            bail!("unknown project `{p}`; see `reg-wiki project list`");
        }
        Ok(ctx)
    }

    /// The project to filter by, or `None` for every page.
    pub fn scope(&self) -> Option<&str> {
        if self.all {
            None
        } else {
            self.current.as_deref()
        }
    }

    pub fn is_project(&self, name: &str) -> bool {
        self.cfg.projects().iter().any(|p| p.name == name) || self.home.wiki.join(name).is_dir()
    }

    pub fn agent(&self) -> &str {
        self.agent.get_or_init(|| {
            self.agent_flag
                .clone()
                .or_else(|| std::env::var_os("CLAUDECODE").map(|_| "claude".to_string()))
                .or_else(|| std::env::var_os("GEMINI_CLI").map(|_| "gemini".to_string()))
                .or_else(git::user_name)
                .unwrap_or_else(|| "user".to_string())
        })
    }

    pub fn author(&self) -> git::Author {
        git::Author::agent(self.agent())
    }

    /// Re-run the reconcile, e.g. after taking the write lock or writing a file.
    pub fn sync(&mut self) -> Result<()> {
        let stats = reconcile::reconcile(&mut self.conn, &self.home.wiki, &self.cfg.projects())?;
        warn_all(&stats.warnings);
        self.stats = stats;
        Ok(())
    }

    /// Turn user input into a full slug: `billing` → `<current project>/billing`.
    /// Input that already starts with a project or `_shared` is kept.
    pub fn qualify(&self, input: &str) -> String {
        let s = input.trim().trim_matches('/');
        let s = s.strip_suffix(".md").unwrap_or(s);
        let first = s.split('/').next().unwrap_or(s);
        if first == SHARED || self.is_project(first) {
            return s.to_string();
        }
        match &self.current {
            Some(p) => format!("{p}/{s}"),
            None => format!("{SHARED}/{s}"),
        }
    }

    /// File path (relative to the wiki) for a new main page.
    pub fn path_for(&self, slug: &str) -> String {
        if !slug.contains('/') || self.home.wiki.join(slug).is_dir() {
            format!("{slug}/index.md")
        } else {
            format!("{slug}.md")
        }
    }

    /// Find an existing page the way a `[[link]]` from the current project would.
    pub fn find(&self, input: &str) -> Result<Option<PageRow>> {
        let input = input.trim().trim_matches('/');
        let input = input.strip_suffix(".md").unwrap_or(input);
        match db::resolver(&self.conn)?.resolve(input, self.current.as_deref()) {
            Resolution::Found(s) => db::page_by_slug(&self.conn, &s),
            Resolution::Broken => Ok(None),
            Resolution::Ambiguous(c) => bail!("`{input}` is ambiguous: {}", c.join(", ")),
        }
    }

    pub fn resolve(&self, input: &str) -> Result<PageRow> {
        match self.find(input)? {
            Some(p) => Ok(p),
            None => bail!("no page `{input}`; see `reg-wiki list`"),
        }
    }

    /// Write a page atomically and commit it. Returns the commit, if anything changed.
    pub fn store(
        &self,
        rel: &str,
        text: &str,
        slug: &str,
        message: &str,
    ) -> Result<Option<String>> {
        util::atomic_write(&self.home.wiki.join(rel), text.as_bytes())?;
        let commit = git::commit(
            &self.home.wiki,
            &[rel],
            &format!("{slug}: {message}"),
            &self.author(),
        )?;
        self.mark_session_write();
        Ok(commit)
    }

    /// Tell the agent session we run in (if any) that it wrote to the wiki, so the
    /// Stop hook doesn't ask it to record what it learned.
    fn mark_session_write(&self) {
        if let Some(id) = std::env::var_os(crate::hooks::SESSION_ENV) {
            crate::hooks::state::mark_wrote(&self.home, &id.to_string_lossy());
        }
    }

    pub fn event(&self, kind: &str, page: Option<&PageRow>) {
        self.log(kind, page, None, None);
    }

    /// A search, with how many pages it found; zero-hit queries show up as gaps in `stats`.
    pub fn query_event(&self, query: &str, hits: usize) {
        self.log("query", None, Some(query), Some(hits as i64));
    }

    fn log(&self, kind: &str, page: Option<&PageRow>, query: Option<&str>, hits: Option<i64>) {
        let e = db::Event {
            kind,
            page,
            project: self.current.as_deref(),
            agent: self.agent(),
            query,
            hits,
        };
        if let Err(e) = db::log_event(&self.conn, e) {
            warn(&format!("could not record usage event: {e:#}"));
        }
    }
}

impl Ctx {
    /// Lint every page visible in the current scope.
    pub fn lint(&self) -> Result<Vec<crate::lint::Issue>> {
        let projects = self.cfg.projects();
        let today = util::today();
        let lines = self
            .cfg
            .int("limits", "main_page_lines")
            .map_or(crate::lint::MAIN_PAGE_LINES, |n| n.max(1) as usize);
        let opts = crate::lint::Opts {
            scope: self.scope(),
            main_page_lines: lines,
            projects: &projects,
            today: &today,
        };
        crate::lint::check(&self.conn, &self.home.wiki, &opts)
    }

    /// Commit several wiki paths at once (moves, fixes).
    pub fn commit(&self, paths: &[&str], message: &str) -> Result<Option<String>> {
        self.commit_as(paths, message, &self.author())
    }

    pub fn commit_as(
        &self,
        paths: &[&str],
        message: &str,
        author: &git::Author,
    ) -> Result<Option<String>> {
        let commit = git::commit(&self.home.wiki, paths, message, author)?;
        self.mark_session_write();
        Ok(commit)
    }
}

pub fn warn(msg: &str) {
    eprintln!("reg-wiki: warning: {msg}");
}

fn warn_all(msgs: &[String]) {
    for m in msgs {
        warn(m);
    }
}

pub fn print_json(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// `slug — summary`, the one-line form used everywhere pages are listed.
pub fn line(p: &PageRow) -> String {
    let summary = if p.summary.is_empty() {
        "(no summary)"
    } else {
        &p.summary
    };
    format!("{} — {summary}", p.slug)
}

/// Link problems on a page, as human-readable strings.
pub fn link_problems(ctx: &Ctx, page: &PageRow) -> Result<Vec<String>> {
    let resolver = db::resolver(&ctx.conn)?;
    let mut out = Vec::new();
    for l in db::outgoing(&ctx.conn, page.id)? {
        match l.resolution.as_str() {
            "broken" => out.push(format!("broken link [[{}]]", l.target)),
            "ambiguous" => {
                let cands = match resolver.resolve(&l.target, page.project.as_deref()) {
                    Resolution::Ambiguous(c) => c.join(", "),
                    _ => String::new(),
                };
                out.push(format!("ambiguous link [[{}]]: {cands}", l.target));
            }
            _ => {}
        }
    }
    out.dedup();
    Ok(out)
}
