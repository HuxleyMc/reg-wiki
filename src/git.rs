//! The wiki's git repo. Every write is a commit, so the history is the review.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

pub struct Author {
    pub name: String,
    pub email: String,
}

impl Author {
    pub fn agent(name: &str) -> Author {
        let id: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        Author {
            name: name.to_string(),
            email: format!("{id}@reg-wiki"),
        }
    }
}

fn git(wiki: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(wiki);
    // Commits must work without any user git config.
    c.env("GIT_COMMITTER_NAME", "reg-wiki")
        .env("GIT_COMMITTER_EMAIL", "reg-wiki@localhost");
    c
}

fn run(mut cmd: Command) -> Result<Output> {
    let out = cmd.output().context("running git (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out)
}

pub fn ensure_repo(wiki: &Path) -> Result<()> {
    if wiki.join(".git").exists() {
        return Ok(());
    }
    let mut c = git(wiki);
    c.args(["init", "-q", "-b", "main"]);
    run(c).map(|_| ())
}

/// Stage and commit exactly `paths` (relative to the wiki). Returns the short commit
/// hash, or `None` if there was nothing to commit.
pub fn commit(
    wiki: &Path,
    paths: &[&str],
    message: &str,
    author: &Author,
) -> Result<Option<String>> {
    let mut add = git(wiki);
    add.args(["add", "-A", "--"]).args(paths);
    run(add)?;

    let mut diff = git(wiki);
    diff.args(["diff", "--cached", "--quiet", "--"]).args(paths);
    if diff.status().context("running git")?.success() {
        return Ok(None);
    }

    let mut c = git(wiki);
    c.args(["-c", "commit.gpgsign=false", "commit", "-q", "--no-verify"])
        .arg(format!("--author={} <{}>", author.name, author.email))
        .args(["-m", message, "--"])
        .args(paths);
    run(c)?;

    let mut rev = git(wiki);
    rev.args(["rev-parse", "--short", "HEAD"]);
    let out = run(rev)?;
    Ok(Some(
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    ))
}

/// The user's git identity, if configured.
pub fn user_name() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}
