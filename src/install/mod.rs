//! `reg-wiki install` / `uninstall`: wire agents up to the wiki. Claude Code gets the
//! skill, hooks and a CLAUDE.md section and uses the CLI; Codex, Cursor and Gemini CLI
//! reach the wiki through the MCP server (`reg-wiki serve --mcp`), plus whatever
//! instructions file and hooks each one has.

mod claude;
mod codex;
mod cursor;
mod gemini;
mod librarian;
mod shared;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};

use crate::{Cli, InstallArgs, UninstallArgs, util};

/// The skill, embedded so the binary is all there is to install.
pub const SKILL_FILES: &[(&str, &str)] = &[
    ("SKILL.md", include_str!("skill/SKILL.md")),
    (
        "references/writing-pages.md",
        include_str!("skill/references/writing-pages.md"),
    ),
    (
        "references/commands.md",
        include_str!("skill/references/commands.md"),
    ),
];

pub const TARGETS: &[(&str, &str)] = &[
    ("claude", "Claude Code"),
    ("codex", "Codex"),
    ("cursor", "Cursor"),
    ("gemini", "Gemini CLI"),
];

/// `claude,codex` → the targets, checked, without repeats.
fn targets(spec: &str) -> Result<Vec<&'static str>> {
    let mut out: Vec<&'static str> = Vec::new();
    for t in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let t = t.to_lowercase();
        let Some((name, _)) = TARGETS
            .iter()
            .find(|(n, _)| *n == t || (t == "gemini-cli" && *n == "gemini"))
        else {
            bail!(
                "unknown target `{t}`; supported: {}",
                TARGETS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        if !out.contains(name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        bail!("no target given");
    }
    Ok(out)
}

fn label(target: &str) -> &'static str {
    TARGETS
        .iter()
        .find(|(n, _)| *n == target)
        .map_or("the agent", |(_, l)| l)
}

fn labels(ts: &[&str]) -> String {
    ts.iter().map(|t| label(t)).collect::<Vec<_>>().join(", ")
}

pub fn install(cli: &Cli, a: &InstallArgs) -> Result<ExitCode> {
    if let Some(t) = &a.print_config {
        let ts = targets(t)?;
        if ts.len() != 1 {
            bail!("--print-config takes one target");
        }
        match ts[0] {
            "claude" => claude::print_config(cli.json, a)?,
            "codex" => codex::print_config(cli.json)?,
            "cursor" => cursor::print_config(cli.json)?,
            _ => gemini::print_config(cli.json, a)?,
        }
        return Ok(ExitCode::SUCCESS);
    }
    let ts = targets(&a.target)?;
    let mut plan = Plan::default();
    for t in &ts {
        plan.merge(match *t {
            "claude" => claude::plan_install(a)?,
            "codex" => codex::plan_install()?,
            "cursor" => cursor::plan_install()?,
            _ => gemini::plan_install(a)?,
        });
    }
    let interactive = !a.yes && std::io::stdin().is_terminal();
    let mut notes = Vec::new();
    if let Some(c) = librarian::plan(a, interactive, &mut notes)? {
        plan.changes.push(c);
    }
    plan.notes.extend(notes);
    apply(cli.json, a.yes, "install", &labels(&ts), plan)
}

pub fn uninstall(cli: &Cli, a: &UninstallArgs) -> Result<ExitCode> {
    // Without --target, every agent: each plan is empty where nothing was installed.
    let ts = match &a.target {
        Some(t) => targets(t)?,
        None => TARGETS.iter().map(|(n, _)| *n).collect(),
    };
    let mut plan = Plan::default();
    for t in &ts {
        plan.merge(match *t {
            "claude" => claude::plan_uninstall()?,
            "codex" => codex::plan_uninstall()?,
            "cursor" => cursor::plan_uninstall()?,
            _ => gemini::plan_uninstall()?,
        });
    }
    let who = if a.target.is_some() {
        labels(&ts)
    } else {
        "any agent".to_string()
    };
    apply(cli.json, a.yes, "uninstall", &who, plan)
}

/// One file to write (`Some`) or delete (`None`), and why.
pub struct Change {
    pub path: PathBuf,
    pub content: Option<String>,
    pub what: String,
}

#[derive(Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    /// Directories to remove afterwards if they're empty, deepest first.
    pub prune_dirs: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub hook_command: Option<String>,
}

impl Plan {
    fn merge(&mut self, other: Plan) {
        self.changes.extend(other.changes);
        self.prune_dirs.extend(other.prune_dirs);
        for n in other.notes {
            if !self.notes.contains(&n) {
                self.notes.push(n);
            }
        }
        if self.hook_command.is_none() {
            self.hook_command = other.hook_command;
        }
    }
}

fn apply(json: bool, yes: bool, verb: &str, who: &str, plan: Plan) -> Result<ExitCode> {
    if plan.changes.is_empty() {
        if json {
            crate::cmd::print_json(&serde_json::json!({
                "action": verb, "changes": [], "notes": plan.notes,
                "hook_command": plan.hook_command,
            }));
        } else {
            let state = if verb == "install" {
                "already installed"
            } else {
                "not installed"
            };
            println!("reg-wiki: {state} for {who}; nothing to change");
            for n in &plan.notes {
                println!("note: {n}");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!(
                "{verb} would change {who}'s settings and ~/.reg; run it with --yes to go ahead without asking"
            );
        }
        eprintln!("reg-wiki {verb} will:");
        for c in &plan.changes {
            eprintln!("  {} — {}", tilde(&c.path), c.what);
        }
        eprint!("Proceed? [Y/n] ");
        std::io::stderr().flush().ok();
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        let a = answer.trim().to_lowercase();
        if !(a.is_empty() || a == "y" || a == "yes") {
            eprintln!("nothing changed");
            return Ok(ExitCode::from(1));
        }
    }

    for c in &plan.changes {
        match &c.content {
            Some(text) => util::atomic_write(&c.path, text.as_bytes())?,
            None => match std::fs::remove_file(&c.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| format!("removing {}", c.path.display()));
                }
            },
        }
    }
    for d in &plan.prune_dirs {
        // Fails (and is left alone) unless the directory is empty.
        let _ = std::fs::remove_dir(d);
    }

    if json {
        let changes: Vec<_> = plan
            .changes
            .iter()
            .map(|c| serde_json::json!({ "path": c.path, "what": c.what, "deleted": c.content.is_none() }))
            .collect();
        crate::cmd::print_json(&serde_json::json!({
            "action": verb, "changes": changes, "notes": plan.notes,
            "hook_command": plan.hook_command,
        }));
    } else {
        for c in &plan.changes {
            println!("{} — {}", tilde(&c.path), c.what);
        }
        for n in &plan.notes {
            println!("note: {n}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `~/…` for paths under $HOME, for messages.
pub fn tilde(p: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Ok(rest) = p.strip_prefix(&home)
    {
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}
