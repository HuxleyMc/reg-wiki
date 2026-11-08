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
    // Paths that no longer exist and were never committed (e.g. a hand-made page that
    // was moved) would make git reject the whole pathspec.
    let tracked = tracked(wiki, paths)?;
    let paths: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|p| {
            wiki.join(p).exists()
                || tracked
                    .iter()
                    .any(|t| t == p || t.starts_with(&format!("{p}/")))
        })
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    let paths = &paths[..];
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

fn tracked(repo: &Path, paths: &[&str]) -> Result<Vec<String>> {
    let mut c = git(repo);
    c.args(["ls-files", "-z", "--"]).args(paths);
    let out = run(c)?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect())
}

/// The date (YYYY-MM-DD) files matching `globs` last changed in the repo at `root`:
/// `today` if any are modified or untracked, else their last commit date.
pub fn last_change(root: &Path, globs: &[String], today: &str) -> Result<Option<String>> {
    let specs: Vec<String> = globs.iter().map(|g| format!(":(glob){g}")).collect();
    let mut st = git(root);
    st.args(["status", "--porcelain", "--"]).args(&specs);
    if !run(st)?.stdout.is_empty() {
        return Ok(Some(today.to_string()));
    }
    let mut log = git(root);
    log.args(["log", "-1", "--format=%cs", "--"]).args(&specs);
    let out = run(log)?;
    let date = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!date.is_empty()).then_some(date))
}

/// Uncommitted changes in the wiki, as `git status --porcelain` lines.
pub fn pending(wiki: &Path) -> Result<Vec<String>> {
    let mut c = git(wiki);
    c.args(["status", "--porcelain", "--untracked-files=all"]);
    let out = run(c)?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.contains(".reg-tmp-"))
        .map(str::to_string)
        .collect())
}

/// Commit authors since `since` (a git date such as `30.days`), limited to `paths`.
pub fn authors_since(wiki: &Path, since: &str, paths: &[&str]) -> Result<Vec<String>> {
    let mut c = git(wiki);
    c.args(["log", &format!("--since={since}"), "--format=%an", "--"])
        .args(paths);
    let out = match c.output() {
        Ok(o) if o.status.success() => o,
        _ => return Ok(Vec::new()), // no commits yet
    };
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}
