//! `reg-wiki install` / `uninstall`: wire an agent up to the wiki. Only Claude Code for
//! now; other targets would get their own module with the same shape.

mod claude;

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

const TARGETS: &[&str] = &["claude"];

fn check_target(t: &str) -> Result<()> {
    if !TARGETS.contains(&t) {
        bail!(
            "unknown target `{t}`; supported: {} (codex, cursor and gemini are planned)",
            TARGETS.join(", ")
        );
    }
    Ok(())
}

pub fn install(cli: &Cli, a: &InstallArgs) -> Result<ExitCode> {
    if let Some(t) = &a.print_config {
        check_target(t)?;
        claude::print_config(cli.json, a)?;
        return Ok(ExitCode::SUCCESS);
    }
    check_target(&a.target)?;
    let plan = claude::plan_install(a)?;
    apply(cli.json, a.yes, "install", plan)
}

pub fn uninstall(cli: &Cli, a: &UninstallArgs) -> Result<ExitCode> {
    check_target(&a.target)?;
    let plan = claude::plan_uninstall()?;
    apply(cli.json, a.yes, "uninstall", plan)
}

/// One file to write (`Some`) or delete (`None`), and why.
pub struct Change {
    pub path: PathBuf,
    pub content: Option<String>,
    pub what: String,
}

pub struct Plan {
    pub changes: Vec<Change>,
    /// Directories to remove afterwards if they're empty, deepest first.
    pub prune_dirs: Vec<PathBuf>,
    pub notes: Vec<String>,
    pub hook_command: Option<String>,
}

fn apply(json: bool, yes: bool, verb: &str, plan: Plan) -> Result<ExitCode> {
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
            println!("reg-wiki: {state} for Claude Code; nothing to change");
            for n in &plan.notes {
                println!("note: {n}");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }

    if !yes {
        if !std::io::stdin().is_terminal() {
            bail!(
                "{verb} would change files in ~/.claude; run it with --yes to go ahead without asking"
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
