use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Std;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

struct Env {
    _tmp: TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Env {
    /// A fresh REG_HOME plus a git repo with a remote, registered as project `myapp`.
    fn new() -> Env {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("home");
        let repo = tmp.path().join("myapp");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/myapp.git"],
        );
        let env = Env {
            _tmp: tmp,
            home,
            repo,
        };
        env.cmd().args(["project", "add"]).assert().success();
        env
    }

    fn cmd(&self) -> Command {
        self.cmd_in(&self.repo)
    }

    fn cmd_in(&self, dir: &Path) -> Command {
        let mut c = Command::cargo_bin("reg-wiki").unwrap();
        c.current_dir(dir)
            .env("REG_HOME", &self.home)
            .env("REG_AGENT", "test-agent")
            .env_remove("CLAUDECODE");
        c
    }

    fn wiki(&self) -> PathBuf {
        self.home.join("wiki")
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.cmd().args(args).arg("--json").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn new_page(&self, slug: &str, title: &str, summary: &str) {
        self.cmd()
            .args(["new", slug, "--title", title, "--summary", summary])
            .assert()
            .success();
    }

    fn write(&self, slug: &str, text: &str) -> assert_cmd::assert::Assert {
        self.cmd()
            .args(["write", slug, "-m", "test write"])
            .write_stdin(text)
            .assert()
    }

    fn log(&self) -> Vec<String> {
        let out = Std::new("git")
            .arg("-C")
            .arg(self.wiki())
            .args(["log", "--format=%an|%s"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Std::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

fn page(title: &str, summary: &str, body: &str) -> String {
    format!("---\ntitle: {title}\nsummary: {summary}\n---\n\n{body}\n")
}

#[test]
fn project_add_creates_a_hub_and_is_detected_from_subdirs() {
    let env = Env::new();
    assert!(env.wiki().join("myapp/index.md").exists());
    let sub = env.repo.join("src/deep");
    fs::create_dir_all(&sub).unwrap();
    env.cmd_in(&sub)
        .args(["project", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("* myapp — github.com/acme/myapp"));
    // Registering again is a no-op.
    env.cmd()
        .args(["project", "add"])
        .assert()
        .success()
        .stdout(predicate::str::contains("already registered"));
    assert_eq!(env.log(), vec!["test-agent|myapp: add project hub"]);
}

#[test]
fn new_read_write_list_round_trip() {
    let env = Env::new();
    env.new_page(
        "billing",
        "Billing",
        "How billing works. Read before touching src/billing.",
    );
    assert!(env.wiki().join("myapp/billing.md").exists());

    let read = env.json(&["read", "billing"]);
    assert_eq!(read["page"]["slug"], "myapp/billing");
    let hash = read["page"]["hash"].as_str().unwrap().to_string();

    let text = "---\ntitle: Billing\nsummary: How billing works.\ntags: [payments]\nowner: alice\nreview:\n  every: 30d\n---\n\n# Billing\n\nSee [[myapp]].\n";
    env.cmd()
        .args([
            "write",
            "billing",
            "--base",
            &hash,
            "-m",
            "describe billing",
        ])
        .write_stdin(text)
        .assert()
        .success()
        .stdout(predicate::str::starts_with("wrote myapp/billing"));

    let on_disk = fs::read_to_string(env.wiki().join("myapp/billing.md")).unwrap();
    assert!(
        on_disk.contains("owner: alice\nreview:\n  every: 30d\n"),
        "unknown keys kept:\n{on_disk}"
    );
    assert!(
        on_disk.contains("created: "),
        "created kept from the original page"
    );
    assert!(on_disk.contains("updated: "));

    env.cmd().args(["list"]).assert().success().stdout(
        predicate::str::contains("myapp/billing — How billing works.").and(
            predicate::str::contains("myapp — Hub for the myapp project"),
        ),
    );
    env.cmd()
        .args(["list", "--tag", "payments"])
        .assert()
        .success()
        .stdout("myapp/billing — How billing works.\n");

    let hub = env.json(&["read", "myapp"]);
    assert_eq!(hub["backlinks"][0]["slug"], "myapp/billing");

    let log = env.log();
    assert_eq!(log[0], "test-agent|myapp/billing: describe billing");
    assert_eq!(log[1], "test-agent|myapp/billing: create page");
}

#[test]
fn stale_base_is_refused_with_current_content() {
    let env = Env::new();
    env.new_page("auth", "Auth", "Sessions and tokens.");
    let old = env.json(&["read", "auth"])["page"]["hash"]
        .as_str()
        .unwrap()
        .to_string();
    env.write("auth", &page("Auth", "Sessions and tokens.", "First edit."))
        .success();

    env.cmd()
        .args(["write", "auth", "--base", &old, "-m", "second"])
        .write_stdin(page("Auth", "Sessions and tokens.", "Second edit."))
        .assert()
        .code(3)
        .stdout(predicate::str::contains("First edit."))
        .stderr(predicate::str::contains("conflict"));
    let on_disk = fs::read_to_string(env.wiki().join("myapp/auth.md")).unwrap();
    assert!(on_disk.contains("First edit.") && !on_disk.contains("Second edit."));
}

#[test]
fn invalid_pages_are_refused() {
    let env = Env::new();
    env.write("x", "no frontmatter\n")
        .failure()
        .stderr(predicate::str::contains("frontmatter"));
    let long = "a".repeat(201);
    env.write("x", &page("X", &long, ""))
        .failure()
        .stderr(predicate::str::contains("201 characters"));
    env.write("x", "---\ntitle: X\nsummary: s\nstatus: done\n---\n")
        .failure()
        .stderr(predicate::str::contains("status"));
    assert!(!env.wiki().join("myapp/x.md").exists());
}

#[test]
fn body_only_write_keeps_frontmatter_and_noop_is_not_committed() {
    let env = Env::new();
    env.new_page("cache", "Cache", "Redis cache layout.");
    env.write("cache", "\n# Cache\n\nKeys expire after an hour.\n")
        .success();
    let text = fs::read_to_string(env.wiki().join("myapp/cache.md")).unwrap();
    assert!(text.contains("summary: Redis cache layout.") && text.contains("expire after an hour"));
    let commits = env.log().len();
    env.write("cache", "\n# Cache\n\nKeys expire after an hour.\n")
        .success()
        .stdout(predicate::str::starts_with("unchanged"));
    assert_eq!(env.log().len(), commits);
}

#[test]
fn refs_live_under_their_parent() {
    let env = Env::new();
    env.new_page("billing", "Billing", "Billing overview.");
    env.cmd()
        .args([
            "new",
            "stripe",
            "--parent",
            "billing",
            "--title",
            "Stripe",
            "--summary",
            "Stripe webhook details.",
        ])
        .assert()
        .success();
    assert!(env.wiki().join("myapp/billing/refs/stripe.md").exists());
    let r = env.json(&["read", "billing"]);
    assert_eq!(r["refs"][0]["slug"], "myapp/billing/stripe");
    let s = env.json(&["read", "stripe"]);
    assert_eq!(s["page"]["is_ref"], true);
    assert_eq!(s["parent"]["slug"], "myapp/billing");
    // Refs are hidden from `list` unless asked for.
    env.cmd()
        .arg("list")
        .assert()
        .stdout(predicate::str::contains("stripe").not());
    env.cmd()
        .args(["list", "--refs"])
        .assert()
        .stdout(predicate::str::contains("myapp/billing/stripe"));
}

#[test]
fn links_resolve_report_broken_and_ambiguous() {
    let env = Env::new();
    env.new_page("_shared/rust", "Rust", "Rust conventions.");
    // Other projects' folders; a first segment that is no project is a folder in this one.
    for p in ["a", "b"] {
        fs::create_dir_all(env.wiki().join(p)).unwrap();
    }
    env.new_page("a/dup", "Dup A", "One.");
    env.new_page("b/dup", "Dup B", "Two.");
    env.write(
        "page",
        &page(
            "Page",
            "Links.",
            "[[rust]] [[dup]] [[missing]] [x](../_shared/rust.md) `[[in-code]]`",
        ),
    )
    .success()
    .stderr(
        predicate::str::contains("broken link [[missing]]")
            .and(predicate::str::contains(
                "ambiguous link [[dup]]: a/dup, b/dup",
            ))
            .and(predicate::str::contains("in-code").not()),
    );
    let r = env.json(&["read", "page"]);
    assert_eq!(r["links"].as_array().unwrap().len(), 1);
    assert_eq!(r["links"][0]["slug"], "_shared/rust");

    // Creating the missing page fixes the link on the other page.
    env.new_page("missing", "Missing", "Now here.");
    let r = env.json(&["read", "page"]);
    assert_eq!(r["links"].as_array().unwrap().len(), 2);
}

#[test]
fn hand_edits_are_picked_up() {
    let env = Env::new();
    fs::create_dir_all(env.wiki().join("_shared")).unwrap();
    fs::write(
        env.wiki().join("_shared/by-hand.md"),
        page("By hand", "Written in Obsidian.", ""),
    )
    .unwrap();
    env.cmd()
        .arg("list")
        .assert()
        .stdout(predicate::str::contains(
            "_shared/by-hand — Written in Obsidian.",
        ));

    fs::write(
        env.wiki().join("_shared/by-hand.md"),
        page("By hand", "Edited again.", ""),
    )
    .unwrap();
    env.cmd()
        .arg("list")
        .assert()
        .stdout(predicate::str::contains("_shared/by-hand — Edited again."));

    fs::remove_file(env.wiki().join("_shared/by-hand.md")).unwrap();
    env.cmd()
        .arg("list")
        .assert()
        .stdout(predicate::str::contains("by-hand").not());

    // Broken frontmatter is a warning, not a failure.
    fs::write(
        env.wiki().join("_shared/bad.md"),
        "---\ntitle: [unclosed\n---\n",
    )
    .unwrap();
    env.cmd()
        .arg("sync")
        .assert()
        .success()
        .stderr(predicate::str::contains("_shared/bad.md"));
}

#[test]
fn scope_follows_the_current_project() {
    let env = Env::new();
    env.new_page("mine", "Mine", "In myapp.");
    env.new_page("_shared/everyone", "Everyone", "Shared.");
    fs::create_dir_all(env.wiki().join("other")).unwrap();
    env.write("other/theirs", &page("Theirs", "In other.", ""))
        .success();
    env.write(
        "_shared/assoc",
        "---\ntitle: Assoc\nsummary: Shared but tied to myapp.\nprojects: [myapp]\n---\n",
    )
    .success();

    let slugs = |v: Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|p| p["slug"].as_str().unwrap().to_string())
            .collect()
    };
    let here = slugs(env.json(&["list"]));
    assert!(here.contains(&"myapp/mine".into()) && here.contains(&"_shared/everyone".into()));
    assert!(!here.contains(&"other/theirs".into()));
    assert!(slugs(env.json(&["list", "--all"])).contains(&"other/theirs".into()));
    assert!(slugs(env.json(&["list", "--project", "other"])).contains(&"other/theirs".into()));
    env.cmd()
        .args(["list", "--project", "nope"])
        .assert()
        .failure();
}

#[test]
fn unknown_first_segment_is_a_folder_in_the_current_project() {
    let env = Env::new();
    env.new_page("api/errors", "API errors", "Error codes.");
    assert!(env.wiki().join("myapp/api/errors.md").exists());
}

#[test]
fn concurrent_writers_all_commit() {
    let env = Env::new();
    let children: Vec<_> = (0..8)
        .map(|i| {
            let mut c = Std::new(assert_cmd::cargo::cargo_bin("reg-wiki"));
            c.current_dir(&env.repo)
                .env("REG_HOME", &env.home)
                .env("REG_AGENT", format!("agent-{i}"))
                .args(["write", &format!("page-{i}"), "-m", "parallel"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null());
            let mut child = c.spawn().unwrap();
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(page("P", "Parallel page.", "").as_bytes())
                .unwrap();
            child
        })
        .collect();
    for mut c in children {
        assert!(c.wait().unwrap().success());
    }
    let log = env.log();
    assert_eq!(log.len(), 9, "{log:?}");
    let pages = env.json(&["list"]);
    assert_eq!(pages.as_array().unwrap().len(), 9);
}

#[test]
fn reindex_rebuilds_the_same_index() {
    let env = Env::new();
    env.new_page("one", "One", "First.");
    let before = env.json(&["list", "--all"]);
    fs::remove_file(env.home.join("reg.db")).unwrap();
    assert_eq!(env.json(&["list", "--all"]), before);
    env.cmd()
        .arg("reindex")
        .assert()
        .success()
        .stdout(predicate::str::contains("reindexed: 2 pages"));
    assert_eq!(env.json(&["list", "--all"]), before);
}
