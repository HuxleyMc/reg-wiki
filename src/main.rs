mod cmd;
mod db;
mod git;
mod home;
mod links;
mod lint;
mod page;
mod project;
mod search;
mod util;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// A living wiki for agents: linked Markdown pages, indexed in SQLite, reviewed through git.
#[derive(Parser, Debug)]
#[command(name = "reg-wiki", version)]
pub struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Use this project instead of the one detected from the current directory.
    #[arg(long, global = true, conflicts_with = "all")]
    pub project: Option<String>,

    /// Cover every project, not just the current one.
    #[arg(long, global = true)]
    pub all: bool,

    /// Name recorded as the author of writes and in usage events.
    #[arg(long, global = true, env = "REG_AGENT")]
    pub agent: Option<String>,

    #[command(subcommand)]
    pub cmd: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// One line per page: `slug — summary`.
    List {
        /// Only pages with this tag (repeatable; all must match).
        #[arg(long)]
        tag: Vec<String>,
        /// Include refs (supporting pages) as well as main pages.
        #[arg(long)]
        refs: bool,
    },
    /// Print a page plus a footer with its hash, refs, links and backlinks.
    Read { slug: String },
    /// Create a page from the template.
    New {
        slug: String,
        #[arg(long)]
        title: String,
        /// When to read the page, in at most 200 characters.
        #[arg(long)]
        summary: String,
        /// Create the page as a ref (supporting doc) of this page.
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Commit message (why the page exists).
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// Replace a page with the Markdown on stdin, then commit it.
    Write {
        slug: String,
        /// Hash printed by `read`; the write is refused if the page changed since.
        #[arg(long)]
        base: Option<String>,
        /// Why the page changed; becomes the commit message.
        #[arg(short = 'm', long)]
        message: String,
    },
    /// Full-text search: slug, summary and a snippet for each match, best first.
    Query {
        #[arg(required = true, num_args = 1..)]
        search: Vec<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Only pages with this tag (repeatable; all must match).
        #[arg(long)]
        tag: Vec<String>,
    },
    /// The best-matching pages' bodies plus summaries of what they link to, in one
    /// call, cut to a token budget.
    Context {
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
        /// Token budget, e.g. 4k or 1500 (default: `[limits] context_budget`, else 4k).
        #[arg(long)]
        budget: Option<String>,
        /// How many search hits to consider.
        #[arg(long, default_value_t = 8)]
        candidates: usize,
    },
    /// Pages a page links to.
    Links { slug: String },
    /// Pages that link to a page.
    Backlinks { slug: String },
    /// Rename a page (with its refs and subpages) and rewrite every link to it.
    Mv {
        old: String,
        new: String,
        /// Why; added to the commit message.
        #[arg(short = 'm', long)]
        message: Option<String>,
    },
    /// Check for broken or ambiguous links, orphans, bad summaries, long and stale pages.
    Lint {
        /// Fix what can be fixed safely, in one commit.
        #[arg(long)]
        fix: bool,
    },
    /// Index freshness, uncommitted changes and lint counts.
    Status,
    /// Size, links, health, reading and writing activity, and gaps in the wiki.
    Stats,
    /// Manage projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Bring the index up to date with the files.
    Sync,
    /// Rebuild the index from scratch (usage events are kept).
    Reindex,
}

#[derive(Subcommand, Debug)]
pub enum ProjectCmd {
    /// Register the git repo containing the current directory as a project.
    Add { name: Option<String> },
    /// List registered projects.
    List,
    /// Unregister a project. Its pages are kept.
    Rm { name: String },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cmd::run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("reg-wiki: {e:#}");
            ExitCode::from(1)
        }
    }
}
