mod cmd;
mod db;
mod git;
mod home;
mod hooks;
mod install;
mod librarian;
mod links;
mod lint;
mod page;
mod project;
mod search;
mod serve;
mod update;
mod util;
mod view;

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
        /// Code the page describes, as a glob relative to the project root, e.g.
        /// `src/billing/**` (repeatable). Edits to these files remind agents of the page.
        #[arg(long = "source", value_name = "GLOB")]
        sources: Vec<String>,
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
    /// Write an HTML graph of the wiki and open it in the browser.
    ///
    /// Nodes are coloured by project and sized by links; click one to read the page
    /// with its refs and backlinks. Covers the current project plus `_shared`, or
    /// every page with `--all`.
    View {
        /// Where to write the page (default: view.html in REG_HOME); `-` for stdout.
        #[arg(long, short = 'o', value_name = "PATH")]
        out: Option<std::path::PathBuf>,
        /// Write the file but don't open a browser.
        #[arg(long)]
        no_open: bool,
    },
    /// Ask the librarian, an agent that reads the wiki for you (see `[librarian]` in
    /// config.toml; set up by `reg-wiki install`).
    Ai {
        #[command(subcommand)]
        cmd: AiCmd,
    },
    /// Manage projects.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Bring the index up to date with the files.
    Sync,
    /// Rebuild the index from scratch (usage events are kept).
    Reindex,
    /// Run an agent hook (called by Claude Code or Gemini CLI; reads the hook's JSON on
    /// stdin).
    ///
    /// Events: session-start, user-prompt-submit, post-tool-use, stop, and Gemini CLI's
    /// before-agent and after-tool. A hook never
    /// fails: errors go to stderr and the exit code is 0. Does nothing when
    /// REG_LIBRARIAN=1.
    Hook { event: String },
    /// Serve the wiki to MCP clients over stdio (`--mcp`), for agents that reach tools
    /// through MCP: Codex, Cursor, Gemini CLI. `reg-wiki install` sets this up.
    Serve {
        /// Speak the Model Context Protocol on stdin/stdout.
        #[arg(long, required = true)]
        mcp: bool,
    },
    /// Set up agents to use the wiki: Claude Code gets a skill, hooks, permissions and a
    /// CLAUDE.md section; Codex, Cursor and Gemini CLI get the MCP server and what else
    /// each supports.
    Install(InstallArgs),
    /// Remove everything `install` added.
    Uninstall(UninstallArgs),
    /// Update reg-wiki to the latest release (or `--version`), checking its SHA-256.
    Update(update::UpdateArgs),
}

#[derive(clap::Args, Debug)]
pub struct InstallArgs {
    /// Agents to set up, comma-separated: claude, codex, cursor, gemini.
    #[arg(long, default_value = "claude")]
    pub target: String,
    /// Don't ask for confirmation.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Don't add `Bash(reg-wiki:*)` to Claude Code's allow list, or trust the MCP server
    /// in Gemini CLI.
    #[arg(long)]
    pub no_permissions: bool,
    /// Also add the Stop hook, which asks the agent once whether anything is worth
    /// recording when a session changed code but wrote nothing to the wiki.
    #[arg(long)]
    pub stop_hook: bool,
    /// Don't set up the librarian (or turn it off if it was).
    #[arg(long, conflicts_with_all = ["librarian_agent", "librarian_model"])]
    pub no_librarian: bool,
    /// Agent the librarian runs on: a key into `[agents.*]` in config.toml (default: claude).
    #[arg(long, value_name = "NAME")]
    pub librarian_agent: Option<String>,
    /// Model for the librarian (default: claude-haiku-5-5). `reg-wiki ai models` lists them.
    #[arg(long, value_name = "MODEL")]
    pub librarian_model: Option<String>,
    /// Print the files and settings for this target instead of writing them.
    #[arg(long, value_name = "TARGET")]
    pub print_config: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct UninstallArgs {
    /// Agents to remove the setup from, comma-separated (default: every agent).
    #[arg(long)]
    pub target: Option<String>,
    /// Don't ask for confirmation.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Subcommand, Debug)]
pub enum AiCmd {
    /// Answer a question from the best-matching pages, citing them. Falls back to plain
    /// `query` results when the librarian is off or fails.
    Query {
        #[arg(required = true, num_args = 1..)]
        question: Vec<String>,
        /// How many search results to show if it falls back to plain search.
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Turn rough notes (stdin) into page edits; reg-wiki checks them and commits the
    /// ones that pass as `librarian (<model>)`.
    Record {
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Fix what `lint` finds that needs judgment: broken links, orphans, weak
    /// summaries, long pages, likely duplicates.
    Tend {
        /// At most this many pages with problems per pass.
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// List the models the librarian's agent offers.
    Models,
    /// Run `ai tend` on a schedule (a crontab entry), over every project. Without an
    /// argument, shows the current schedule.
    Schedule {
        /// daily, weekly (Sundays) or off.
        #[arg(value_parser = ["daily", "weekly", "off"])]
        when: Option<String>,
        /// Local time to run at, HH:MM (default 03:17).
        #[arg(long, value_name = "HH:MM")]
        at: Option<String>,
    },
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
    // Die quietly on a closed pipe (`reg-wiki list | head`) instead of panicking.
    // SAFETY: called before any other thread exists; SIG_DFL is always a valid handler.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    match cmd::run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("reg-wiki: {e:#}");
            ExitCode::from(1)
        }
    }
}
