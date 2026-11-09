//! `reg-wiki update`: replace this binary with a release from GitHub.
//!
//! Downloads go through `curl` (or `wget`) and `tar`, like install.sh, so the
//! binary carries no HTTP or TLS stack. The archive is checked against its
//! published SHA-256 before the new binary is renamed over the old one, so a
//! hook running at the same moment sees either the old file or the new one.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::Cli;
use crate::cmd::print_json;

const RELEASES: &str = "https://github.com/HuxleyMc/reg-wiki/releases";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(clap::Args, Debug)]
pub struct UpdateArgs {
    /// Only report whether a newer release exists (exit code 0 either way).
    #[arg(long)]
    pub check: bool,
    /// Install this release instead of the latest, e.g. v0.2.0 (may be older).
    #[arg(long, value_name = "TAG")]
    pub version: Option<String>,
    /// Reinstall even if this version is already installed.
    #[arg(long)]
    pub force: bool,
}

pub fn run(cli: &Cli, a: &UpdateArgs) -> Result<ExitCode> {
    let base = std::env::var("REG_WIKI_RELEASES_URL").unwrap_or_else(|_| RELEASES.into());
    let base = base.trim_end_matches('/');
    let fetcher = Fetcher::find()?;

    let (tag, pinned) = match &a.version {
        Some(v) => (normalize_tag(v), true),
        None => (fetcher.latest_tag(base)?, false),
    };
    let target =
        Version::parse(&tag[1..]).with_context(|| format!("not a release version: {tag}"))?;
    let current = Version::parse(CURRENT).expect("crate version is valid");
    // Without --version, never move backwards (e.g. a pre-release build).
    let wanted = a.force
        || if pinned {
            target != current
        } else {
            target > current
        };

    if a.check || !wanted {
        if cli.json {
            print_json(&json!({
                "current": format!("v{CURRENT}"),
                "latest": tag,
                "update_available": target > current,
                "updated": false,
            }));
        } else if target > current {
            println!("update available: v{CURRENT} -> {tag} (run `reg-wiki update`)");
        } else if a.check || !pinned {
            println!("up to date (v{CURRENT})");
        } else {
            println!("already on {tag}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let exe = std::env::current_exe()
        .and_then(fs::canonicalize)
        .context("can't find the running reg-wiki binary")?;
    let dir = exe.parent().context("binary has no parent directory")?;
    let asset = format!("reg-wiki-{}.tar.gz", target_triple()?);
    let url = format!("{base}/download/{tag}/{asset}");

    let tmp = tempfile::tempdir()?;
    let archive = tmp.path().join(&asset);
    let sums = tmp.path().join(format!("{asset}.sha256"));
    if !cli.json {
        eprintln!("downloading {asset} ({tag})");
    }
    fetcher
        .get(&url, &archive)
        .with_context(|| format!("download failed: {url}"))?;
    fetcher
        .get(&format!("{url}.sha256"), &sums)
        .context("checksum download failed")?;
    verify(&archive, &sums)?;

    let status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(tmp.path())
        .status()
        .context("can't run tar")?;
    if !status.success() {
        bail!("couldn't unpack {asset}");
    }
    let new_bin = tmp.path().join("reg-wiki");
    if !new_bin.is_file() {
        bail!("{asset} has no reg-wiki binary");
    }
    let new_version = installed_version(&new_bin)?;

    replace(&new_bin, &exe, dir)?;

    if cli.json {
        print_json(&json!({
            "current": format!("v{CURRENT}"),
            "latest": tag,
            "update_available": target > current,
            "updated": true,
            "installed": new_version,
            "path": exe,
        }));
    } else {
        println!("updated v{CURRENT} -> {tag} ({})", exe.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Copy `new` next to `exe` and rename it over `exe`, so the swap is atomic.
fn replace(new: &Path, exe: &Path, dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let staged = dir.join(format!(".reg-wiki.update-{}", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        fs::copy(new, &staged)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))?;
        fs::rename(&staged, exe)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&staged);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            bail!(
                "can't write to {}: permission denied. Re-run with the rights to change it \
                 (e.g. sudo reg-wiki update), or reinstall somewhere you own with install.sh --dir",
                dir.display()
            );
        }
        return Err(e).with_context(|| format!("can't replace {}", exe.display()));
    }
    Ok(())
}

fn verify(archive: &Path, sums: &Path) -> Result<()> {
    let text = fs::read_to_string(sums)?;
    let expected = text
        .split_whitespace()
        .next()
        .context("empty checksum file")?
        .to_ascii_lowercase();
    let actual = crate::util::hash_hex(&fs::read(archive)?);
    if actual != expected {
        bail!(
            "checksum mismatch for {}; nothing was changed",
            archive.display()
        );
    }
    Ok(())
}

fn installed_version(bin: &Path) -> Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .context("the downloaded binary doesn't run on this machine")?;
    if !out.status.success() {
        bail!("the downloaded binary doesn't run on this machine");
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.split_whitespace().nth(1).unwrap_or("").to_string())
}

fn normalize_tag(v: &str) -> String {
    let v = v.trim();
    if v.starts_with('v') {
        v.to_string()
    } else {
        format!("v{v}")
    }
}

/// The release asset name for the machine this binary was built for.
fn target_triple() -> Result<&'static str> {
    Ok(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        (os, arch) => bail!("no prebuilt release for {os}/{arch}; build from source"),
    })
}

enum Fetcher {
    Curl,
    Wget,
}

impl Fetcher {
    fn find() -> Result<Self> {
        let works = |cmd: &str| {
            Command::new(cmd)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if works("curl") {
            Ok(Fetcher::Curl)
        } else if works("wget") {
            Ok(Fetcher::Wget)
        } else {
            bail!("reg-wiki update needs curl or wget")
        }
    }

    fn get(&self, url: &str, dest: &Path) -> Result<()> {
        let mut cmd = match self {
            Fetcher::Curl => {
                let mut c = Command::new("curl");
                c.args(["-fsSL", "--retry", "3", "--connect-timeout", "15", "-o"]);
                c.arg(dest).arg(url);
                c
            }
            Fetcher::Wget => {
                let mut c = Command::new("wget");
                c.args(["-q", "--timeout=30", "-O"]).arg(dest).arg(url);
                c
            }
        };
        let out = cmd.stderr(Stdio::piped()).output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            bail!("{}", err.trim());
        }
        Ok(())
    }

    /// The newest release's tag, read from where /releases/latest redirects to.
    fn latest_tag(&self, base: &str) -> Result<String> {
        let url = format!("{base}/latest");
        let out = match self {
            Fetcher::Curl => Command::new("curl")
                .args(["-fsSLI", "--connect-timeout", "15", "-o", "/dev/null"])
                .args(["-w", "%{url_effective}", &url])
                .output()?,
            Fetcher::Wget => Command::new("wget")
                .args(["-q", "-S", "--spider", "--max-redirect=5", &url])
                .output()?,
        };
        let text = match self {
            Fetcher::Curl => String::from_utf8_lossy(&out.stdout).into_owned(),
            Fetcher::Wget => String::from_utf8_lossy(&out.stderr)
                .lines()
                .filter_map(|l| {
                    l.trim()
                        .strip_prefix("Location:")
                        .or(l.trim().strip_prefix("location:"))
                })
                .next_back()
                .unwrap_or("")
                .trim()
                .to_string(),
        };
        let tag = text.trim().rsplit('/').next().unwrap_or("");
        if (matches!(self, Fetcher::Curl) && !out.status.success()) || !tag.starts_with('v') {
            bail!("couldn't find the latest release at {base} (is the repo reachable from here?)");
        }
        Ok(tag.to_string())
    }
}

/// `major.minor.patch[-pre]`; a pre-release sorts before its release.
#[derive(Debug, PartialEq, Eq)]
struct Version {
    nums: [u64; 3],
    pre: Option<String>,
}

impl Version {
    fn parse(s: &str) -> Option<Self> {
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) => (c, Some(p.to_string())),
            None => (s, None),
        };
        let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
        let nums = [it.next()??, it.next()??, it.next()??];
        if it.next().is_some() {
            return None;
        }
        Some(Version { nums, pre })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering::*;
        self.nums
            .cmp(&other.nums)
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Equal,
                (None, Some(_)) => Greater,
                (Some(_), None) => Less,
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("0.2.0") > v("0.1.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.2.0") > v("0.2.0-rc.1"));
        assert!(v("0.2.0-rc.2") > v("0.2.0-rc.1"));
        assert_eq!(v("0.1.0"), v("0.1.0"));
        assert!(Version::parse("0.1").is_none());
        assert!(Version::parse("0.1.0.1").is_none());
        assert!(Version::parse("x.1.0").is_none());
    }

    #[test]
    fn tags_get_a_v() {
        assert_eq!(normalize_tag("0.2.0"), "v0.2.0");
        assert_eq!(normalize_tag("v0.2.0"), "v0.2.0");
    }
}
