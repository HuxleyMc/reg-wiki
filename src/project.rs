//! Finding the current project from the working directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::home::ProjectCfg;

/// The nearest ancestor (inclusive) that contains `.git`.
pub fn git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| d.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The repo's remote URL: `origin` if present, otherwise the first one listed.
pub fn git_remote(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let remotes: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| {
            (
                k.trim_start_matches("remote.").trim_end_matches(".url"),
                v.trim(),
            )
        })
        .collect();
    remotes
        .iter()
        .find(|(name, _)| *name == "origin")
        .or(remotes.first())
        .map(|(_, url)| url.to_string())
}

/// Reduce the many spellings of a remote to `host/owner/repo`.
///
/// `git@github.com:Acme/App.git`, `https://user@github.com/Acme/App` and
/// `ssh://git@github.com:22/Acme/App.git/` all become `github.com/Acme/App`.
pub fn normalize_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        (authority, path)
    } else if let Some((authority, path)) = url.split_once(':').filter(|(a, _)| !a.contains('/')) {
        // scp-like syntax: [user@]host:path
        (authority, path)
    } else {
        // A local path used as a remote.
        return url.to_string();
    };

    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host
        .split_once(':')
        .map_or(host, |(h, _)| h)
        .to_ascii_lowercase();
    let path = path.trim_matches('/');
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

/// Match `cwd` to a registered project: by remote first, then by root path.
pub fn detect(projects: &[ProjectCfg], cwd: &Path) -> Option<ProjectCfg> {
    let root = git_root(cwd);
    if let Some(remote) = root
        .as_deref()
        .and_then(git_remote)
        .map(|r| normalize_remote(&r))
        && let Some(p) = projects
            .iter()
            .find(|p| p.remote.as_deref() == Some(remote.as_str()))
    {
        return Some(p.clone());
    }
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    projects
        .iter()
        .filter_map(|p| {
            let r = PathBuf::from(p.root.as_deref()?);
            cwd.starts_with(&r).then(|| (r.components().count(), p))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, p)| p.clone())
}

#[cfg(test)]
mod tests {
    use super::normalize_remote as n;

    #[test]
    fn remotes_normalize_to_the_same_form() {
        let want = "github.com/Acme/App";
        for url in [
            "git@github.com:Acme/App.git",
            "https://github.com/Acme/App.git",
            "https://user:tok@GitHub.com/Acme/App/",
            "ssh://git@github.com:22/Acme/App.git",
            "git://github.com/Acme/App",
        ] {
            assert_eq!(n(url), want, "{url}");
        }
    }

    #[test]
    fn local_paths_are_kept() {
        assert_eq!(n("/srv/git/app.git"), "/srv/git/app");
    }
}
