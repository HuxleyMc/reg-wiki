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
            .env_remove("CLAUDECODE")
            .env_remove("REG_SESSION")
            .env_remove("REG_LIBRARIAN")
            .env_remove("CLAUDE_ENV_FILE")
            .env_remove("CLAUDE_CONFIG_DIR");
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

    /// JSON from a command that may exit 1 (e.g. `lint` with errors).
    fn json_any(&self, args: &[&str]) -> Value {
        let out = self.cmd().args(args).arg("--json").output().unwrap();
        assert!(
            out.status.code().is_some_and(|c| c <= 1),
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

// ---- Milestone 2: find and maintain ----

fn fm(title: &str, summary: &str, extra: &str, body: &str) -> String {
    format!("---\ntitle: {title}\nsummary: {summary}\n{extra}---\n\n{body}\n")
}

fn slugs_of(v: &Value, key: &str) -> Vec<String> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["slug"].as_str().unwrap().to_string())
        .collect()
}

/// billing (with a ref) and auth, linking to each other in several styles.
fn seeded() -> Env {
    let env = Env::new();
    env.write("billing", &fm("Billing", "How subscriptions, invoices and Stripe webhooks fit together.", "",
        "# Billing\n\nInvoices go out monthly. See [[stripe]], [[auth]] and the [hub](index.md)."))
        .success();
    env.cmd()
        .args([
            "new",
            "stripe",
            "--parent",
            "billing",
            "--title",
            "Stripe webhooks",
            "--summary",
            "Webhook retries, idempotency keys and signature checks.",
        ])
        .assert()
        .success();
    env.write(
        "auth",
        &fm(
            "Auth",
            "Sessions, tokens and how login works.",
            "",
            "Uses [[billing]] for plan checks, [[billing/stripe]] for events, and [b](billing.md).",
        ),
    )
    .success();
    env
}

#[test]
fn query_ranks_matches_and_falls_back_to_any_word() {
    let env = seeded();
    let v = env.json(&["query", "webhook", "retries"]);
    assert_eq!(v["matched_all_words"], true);
    assert_eq!(slugs_of(&v, "hits")[0], "myapp/billing/stripe");
    assert!(v["hits"][0]["snippet"].as_str().unwrap().contains('«'));

    // Stemming: "invoice" finds "Invoices"; stopwords are ignored.
    let v = env.json(&["query", "how", "do", "invoice"]);
    assert_eq!(slugs_of(&v, "hits"), vec!["myapp/billing"]);

    // No page has every word: fall back, preferring pages that match more of them.
    let v = env.json(&["query", "stripe", "webhooks", "login", "zebra"]);
    assert_eq!(v["matched_all_words"], false);
    assert_eq!(slugs_of(&v, "hits")[0], "myapp/billing/stripe");

    env.cmd()
        .args(["query", "zebra"])
        .assert()
        .success()
        .stdout("")
        .stderr(predicate::str::contains("no pages match"));
    let stats = env.json(&["stats"]);
    assert_eq!(stats["queries_without_results"][0]["name"], "zebra");
}

#[test]
fn query_respects_scope_and_tags() {
    let env = seeded();
    fs::create_dir_all(env.wiki().join("other")).unwrap();
    env.write(
        "other/billing-notes",
        &fm(
            "Other billing",
            "Invoices elsewhere.",
            "tags: [money]\n",
            "",
        ),
    )
    .success();
    assert!(
        !slugs_of(&env.json(&["query", "invoices"]), "hits")
            .contains(&"other/billing-notes".into())
    );
    assert!(
        slugs_of(&env.json(&["query", "invoices", "--all"]), "hits")
            .contains(&"other/billing-notes".into())
    );
    assert_eq!(
        slugs_of(
            &env.json(&["query", "invoices", "--all", "--tag", "money"]),
            "hits"
        ),
        vec!["other/billing-notes"]
    );
}

#[test]
fn context_fits_the_budget() {
    let env = seeded();
    let v = env.json(&["context", "stripe", "webhooks", "--budget", "2k"]);
    let c = &v["context"];
    assert!(c["used"].as_u64().unwrap() <= 2000);
    assert_eq!(c["pages"][0]["slug"], "myapp/billing/stripe");
    // Linked pages are offered by summary.
    let rel: Vec<&str> = c["related"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["slug"].as_str().unwrap())
        .collect();
    assert!(
        rel.iter()
            .all(|s| !slugs_of(c, "pages").iter().any(|p| p == s)),
        "no duplicates"
    );

    // A long first page is cut to fit rather than dropped.
    let long = "word ".repeat(4000);
    env.write(
        "huge",
        &fm("Huge zebra", "A very long page about zebras.", "", &long),
    )
    .success();
    let v = env.json(&["context", "zebra", "--budget", "500"]);
    let c = &v["context"];
    assert_eq!(c["pages"][0]["slug"], "myapp/huge");
    assert_eq!(c["pages"][0]["truncated"], true);
    assert!(c["used"].as_u64().unwrap() <= 500, "{}", c["used"]);
    env.cmd()
        .args(["context", "zebra", "--budget", "500"])
        .assert()
        .stdout(predicate::str::contains("truncated"));
}

#[test]
fn links_and_backlinks() {
    let env = seeded();
    env.cmd()
        .args(["links", "billing"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("myapp/billing/stripe — ")
                .and(predicate::str::contains("myapp/auth — "))
                .and(predicate::str::contains("myapp — ")),
        );
    let back = env.json(&["backlinks", "billing"]);
    assert_eq!(slugs_of(&back, "backlinks"), vec!["myapp/auth"]);
}

#[test]
fn mv_moves_the_subtree_and_rewrites_links() {
    let env = seeded();
    let commits = env.log().len();
    env.cmd()
        .args(["mv", "billing", "payments/billing", "-m", "group payments"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "moved myapp/billing → myapp/payments/billing",
        ));
    let w = env.wiki();
    assert!(!w.join("myapp/billing.md").exists() && !w.join("myapp/billing").exists());
    assert!(w.join("myapp/payments/billing.md").exists());
    assert!(w.join("myapp/payments/billing/refs/stripe.md").exists());

    // Wikilinks that still resolve are left alone; the .md links are re-pathed.
    let auth = fs::read_to_string(w.join("myapp/auth.md")).unwrap();
    assert!(
        auth.contains("[[billing]]") && auth.contains("[[billing/stripe]]"),
        "{auth}"
    );
    assert!(auth.contains("[b](payments/billing.md)"), "{auth}");
    let billing = fs::read_to_string(w.join("myapp/payments/billing.md")).unwrap();
    assert!(billing.contains("[hub](../index.md)"), "{billing}");

    assert_eq!(
        env.log().len(),
        commits + 1,
        "one commit for the whole move"
    );
    assert!(env.log()[0].contains("myapp/billing → myapp/payments/billing: group payments"));
    let lint = env.json_any(&["lint"]);
    assert_eq!(lint["errors"], 0, "{lint}");
}

#[test]
fn mv_across_projects_qualifies_links() {
    let env = seeded();
    fs::create_dir_all(env.wiki().join("other")).unwrap();
    env.cmd()
        .args(["mv", "auth", "other/auth"])
        .assert()
        .success();
    // Unique basenames resolve anywhere, so [[auth]] and [[billing]] keep working as
    // written; the relative .md link has to change.
    let auth = fs::read_to_string(env.wiki().join("other/auth.md")).unwrap();
    assert!(auth.contains("[b](../myapp/billing.md)"), "{auth}");
    let links = env.json(&["links", "other/auth"]);
    let targets: Vec<&str> = links["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["page"]["slug"].as_str().unwrap())
        .collect();
    assert_eq!(
        targets,
        vec!["myapp/billing", "myapp/billing/stripe", "myapp/billing"]
    );
    assert_eq!(env.json_any(&["lint", "--all"])["errors"], 0);

    // Where the old form would now point at a different page, it is qualified.
    fs::create_dir_all(env.wiki().join("third")).unwrap();
    env.write(
        "other/auth",
        &fm(
            "Auth",
            "Sessions, tokens and how login works.",
            "",
            "[[billing]]",
        ),
    )
    .success();
    env.write(
        "other/billing",
        &fm("Other billing", "The other project's billing.", "", ""),
    )
    .success();
    // other/auth's [[billing]] now means other/billing; moving myapp/billing must not
    // change that, but myapp pages that meant myapp/billing must keep meaning it.
    env.cmd()
        .args(["mv", "myapp/billing", "third/billing"])
        .assert()
        .success();
    let auth = fs::read_to_string(env.wiki().join("other/auth.md")).unwrap();
    assert!(auth.contains("[[billing]]"), "{auth}");
    let stripe_back = env.json(&["backlinks", "third/billing/stripe"]);
    assert!(slugs_of(&stripe_back, "backlinks").contains(&"third/billing".to_string()));
}

#[test]
fn mv_handles_refs_and_uncommitted_pages_and_refuses_bad_targets() {
    let env = seeded();
    env.cmd()
        .args(["mv", "billing", "auth"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
    env.cmd()
        .args(["mv", "billing", "billing/inner"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("inside itself"));
    env.cmd().args(["mv", "myapp", "x/y"]).assert().failure();

    // A ref moved under another main page stays a ref.
    env.cmd()
        .args(["mv", "stripe", "auth/stripe"])
        .assert()
        .success();
    assert!(env.wiki().join("myapp/auth/refs/stripe.md").exists());
    let billing = fs::read_to_string(env.wiki().join("myapp/billing.md")).unwrap();
    assert!(
        billing.contains("[[stripe]]"),
        "basename still unique: {billing}"
    );
    assert!(
        !env.wiki().join("myapp/billing").exists(),
        "empty folders are removed"
    );

    // A page made by hand and never committed can be moved too.
    fs::write(
        env.wiki().join("myapp/scratch.md"),
        fm("Scratch", "Notes not yet committed.", "", ""),
    )
    .unwrap();
    env.cmd()
        .args(["mv", "scratch", "notes"])
        .assert()
        .success();
    assert!(env.wiki().join("myapp/notes.md").exists());
}

#[test]
fn lint_reports_and_fixes() {
    let env = seeded();
    fs::write(
        env.home.join("config.toml"),
        "[limits]\nmain_page_lines = 3\n",
    )
    .unwrap();
    env.cmd().args(["project", "add"]).assert().success(); // re-register after replacing config
    env.write(
        "loose",
        &fm(
            "Loose",
            "Short.",
            "",
            "[[Stripe]] [[Auth Page]] [[nope]]\na\nb\nc\nd",
        ),
    )
    .success();

    let v = env.json_any(&["lint"]);
    let kinds: Vec<(String, String)> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["slug"].as_str().unwrap().to_string(),
                i["kind"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let has = |s: &str, k: &str| kinds.contains(&(s.to_string(), k.to_string()));
    assert!(has("myapp/loose", "broken-link"));
    assert!(has("myapp/loose", "summary"));
    assert!(has("myapp/loose", "long"));
    assert!(has("myapp/loose", "orphan"));
    assert!(!has("myapp", "orphan"), "hubs are not orphans");
    assert!(
        !has("myapp/billing/stripe", "orphan"),
        "refs are reached through their parent"
    );
    env.cmd().arg("lint").assert().code(1);

    // "Auth Page" is not loosely "auth"; only exact normalisations are fixed.
    env.write(
        "loose",
        &fm(
            "Loose",
            "A page with a loosely written link.",
            "",
            "[[Billing]] [[billing_stripe]] [[Stripe Webhooks Gone]]",
        ),
    )
    .success();
    fs::write(
        env.wiki().join("_shared/rust-errors.md"),
        fm(
            "Rust errors",
            "anyhow vs thiserror conventions.",
            "",
            "[[loose]]",
        ),
    )
    .unwrap();
    env.write(
        "billing/stripe",
        &fm(
            "Stripe webhooks",
            "Webhook retries, idempotency keys and signature checks.",
            "",
            "",
        ),
    )
    .success();
    env.write(
        "billing-stripe",
        &fm(
            "Billing stripe",
            "Notes on billing stripe setup.",
            "",
            "[[loose]]",
        ),
    )
    .success();
    env.cmd().args(["lint", "--fix"]).assert().code(1).stdout(
        predicate::str::contains("[[billing_stripe]] → [[billing-stripe]]").and(
            predicate::str::contains("broken link [[Stripe Webhooks Gone]]"),
        ),
    );
    let loose = fs::read_to_string(env.wiki().join("myapp/loose.md")).unwrap();
    assert!(loose.contains("[[Billing]] [[billing-stripe]]"), "{loose}");
    let rust = fs::read_to_string(env.wiki().join("_shared/rust-errors.md")).unwrap();
    assert!(
        rust.contains("created: ") && rust.contains("updated: "),
        "dates filled in: {rust}"
    );
    assert!(
        env.log()[0].contains("lint --fix: 2 fixes"),
        "{:?}",
        env.log()
    );
}

#[test]
fn lint_finds_stale_pages_from_sources() {
    let env = seeded();
    fs::create_dir_all(env.repo.join("src/billing")).unwrap();
    fs::write(env.repo.join("src/billing/mod.rs"), "fn main() {}\n").unwrap();
    git(&env.repo, &["add", "."]);
    let st = Std::new("git")
        .arg("-C")
        .arg(&env.repo)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "code",
        ])
        .env("GIT_COMMITTER_DATE", "2099-01-02T00:00:00")
        .status()
        .unwrap();
    assert!(st.success());
    env.write(
        "billing",
        &fm(
            "Billing",
            "How subscriptions, invoices and Stripe webhooks fit together.",
            "sources: [src/billing/**]\nstatus: stable\n",
            "[[stripe]] [[auth]]",
        ),
    )
    .success();

    let v = env.json_any(&["lint"]);
    let stale: Vec<&Value> = v["issues"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == "stale")
        .collect();
    assert_eq!(stale.len(), 1, "{v}");
    assert!(stale[0]["message"].as_str().unwrap().contains("2099-01-02"));

    env.cmd()
        .args(["lint", "--fix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("marked stale"));
    let text = fs::read_to_string(env.wiki().join("myapp/billing.md")).unwrap();
    assert!(text.contains("status: stale"), "{text}");
    assert_eq!(env.json(&["stats"])["stale"], 1);
}

#[test]
fn status_shows_pending_hand_edits_and_lint() {
    let env = seeded();
    env.cmd().arg("status").assert().success().stdout(
        predicate::str::contains("project  myapp (github.com/acme/myapp)")
            .and(predicate::str::contains("nothing uncommitted"))
            .and(predicate::str::contains("lint     no problems")),
    );
    fs::write(
        env.wiki().join("myapp/auth.md"),
        fm(
            "Auth",
            "Sessions, tokens and how login works.",
            "",
            "[[gone]]",
        ),
    )
    .unwrap();
    let v = env.json(&["status"]);
    assert_eq!(v["pending"].as_array().unwrap().len(), 1);
    assert_eq!(v["lint"]["errors"], 1);
    assert_eq!(v["reconciled"]["updated"], 1);
}

#[test]
fn stats_cover_size_links_reads_and_writes() {
    let env = seeded();
    env.cmd().args(["read", "billing"]).assert().success();
    env.cmd().args(["read", "billing"]).assert().success();
    let v = env.json(&["stats"]);
    assert_eq!(v["pages"], 4);
    assert_eq!(v["refs"], 1);
    assert_eq!(v["broken_links"], 0);
    assert_eq!(v["most_read"][0]["name"], "myapp/billing");
    assert_eq!(v["most_read"][0]["count"], 2);
    assert_eq!(v["writes_30d"][0]["name"], "test-agent");
    assert!(v["growth"][0]["total"].as_i64().unwrap() >= 1);
    env.cmd()
        .arg("stats")
        .assert()
        .success()
        .stdout(predicate::str::contains("myapp + _shared"));
}

#[test]
fn old_index_is_migrated() {
    let env = seeded();
    {
        let db = rusqlite::Connection::open(env.home.join("reg.db")).unwrap();
        db.execute_batch(
            "DROP TABLE pages_fts; CREATE VIRTUAL TABLE pages_fts USING fts5(title, summary, tags, body);
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }
    // Rebuilt with stemming: "invoice" matches "Invoices".
    assert_eq!(
        slugs_of(&env.json(&["query", "invoice"]), "hits"),
        vec!["myapp/billing"]
    );
}

// ---- Milestone 3: agents ----

impl Env {
    /// Run `reg-wiki hook <event>` with `input` on stdin. Hooks always exit 0; returns
    /// the parsed JSON output, or `None` if the hook printed nothing.
    fn hook(&self, event: &str, input: Value) -> Option<Value> {
        self.hook_with(event, input, &[])
    }

    fn hook_with(&self, event: &str, input: Value, envs: &[(&str, &str)]) -> Option<Value> {
        let mut c = self.cmd();
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c
            .args(["hook", event])
            .write_stdin(input.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "hook {event} exited {:?}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        if text.trim().is_empty() {
            None
        } else {
            Some(serde_json::from_str(&text).unwrap())
        }
    }

    fn input(&self, extra: Value) -> Value {
        let mut v = serde_json::json!({ "session_id": "sess-1", "cwd": self.repo });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    }
}

/// The `slug — summary` lines of the session start index.
fn listed(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.contains(" — ") && !l.starts_with("reg-wiki:"))
        .collect()
}

fn context_of(v: &Option<Value>) -> String {
    v.as_ref().expect("hook printed nothing")["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn session_start_lists_the_project_hub_first_within_the_cap() {
    let env = seeded();
    let out = env.hook(
        "session-start",
        env.input(serde_json::json!({ "hook_event_name": "SessionStart", "source": "startup" })),
    );
    assert_eq!(
        out.as_ref().unwrap()["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    let text = context_of(&out);
    let lines = listed(&text);
    assert!(lines[0].starts_with("myapp — "), "{text}");
    assert!(text.contains("myapp/billing — How subscriptions"), "{text}");
    // Refs are left out; they're reached through their page.
    assert!(!text.contains("myapp/billing/stripe"), "{text}");

    // Capped: with many pages, the list is cut and says how many are left.
    for i in 0..80 {
        env.write(
            &format!("page-{i}"),
            &page(
                &format!("Page {i}"),
                "A page that exists to fill the session start list up.",
                "x",
            ),
        )
        .success();
    }
    env.cmd()
        .args(["write", "_shared/conventions", "-m", "t"])
        .write_stdin(fm(
            "Conventions",
            "Team conventions for every project.",
            "tags: [hub]\n",
            "x",
        ))
        .assert()
        .success();
    let text = context_of(&env.hook("session-start", env.input(serde_json::json!({}))));
    assert!(text.chars().count() / 4 <= 1600, "{} chars", text.len());
    assert!(text.contains("more: `reg-wiki list`"), "{text}");
    let lines = listed(&text);
    // Hubs first: the project's, then shared hubs.
    assert!(lines[0].starts_with("myapp — "), "{text}");
    assert!(lines[1].starts_with("_shared/conventions — "), "{text}");
    // Then the most linked-to pages.
    assert!(
        lines[2].starts_with("myapp/billing — ") || lines[2].starts_with("myapp/auth — "),
        "{text}"
    );

    // `[limits] session_start_tokens` changes the cap.
    fs::write(
        env.home.join("config.toml"),
        format!(
            "{}\n[limits]\nsession_start_tokens = 200\n",
            fs::read_to_string(env.home.join("config.toml")).unwrap()
        ),
    )
    .unwrap();
    let text = context_of(&env.hook("session-start", env.input(serde_json::json!({}))));
    assert!(text.len() < 1000, "{text}");
}

#[test]
fn session_start_exports_the_session_id_for_later_writes() {
    let env = seeded();
    let envfile = env.home.join("claude-env");
    env.hook_with(
        "session-start",
        env.input(serde_json::json!({})),
        &[("CLAUDE_ENV_FILE", envfile.to_str().unwrap())],
    );
    assert_eq!(
        fs::read_to_string(&envfile).unwrap(),
        "export REG_SESSION=sess-1\n"
    );
    let state: Value =
        serde_json::from_str(&fs::read_to_string(env.home.join("sessions/sess-1.json")).unwrap())
            .unwrap();
    assert_eq!(state["tagged"], true);
    assert_eq!(state["wrote"], false);

    // A write from inside the session marks it.
    env.cmd()
        .env("REG_SESSION", "sess-1")
        .args(["write", "auth", "-m", "t"])
        .write_stdin("# Auth\n\nChanged.\n")
        .assert()
        .success();
    let state: Value =
        serde_json::from_str(&fs::read_to_string(env.home.join("sessions/sess-1.json")).unwrap())
            .unwrap();
    assert_eq!(state["wrote"], true);
}

#[test]
fn session_start_outside_a_project_shows_shared_pages_or_nothing() {
    let env = Env::new();
    let elsewhere = env.home.parent().unwrap().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let input = serde_json::json!({ "session_id": "s", "cwd": elsewhere });
    assert!(env.hook("session-start", input.clone()).is_none());
    env.cmd()
        .args(["write", "_shared/git-tips", "-m", "t"])
        .write_stdin(page(
            "Git tips",
            "Read when a rebase or merge goes wrong.",
            "x",
        ))
        .assert()
        .success();
    let text = context_of(&env.hook("session-start", input));
    assert!(text.contains("isn't a registered wiki project"), "{text}");
    assert!(text.contains("_shared/git-tips — "), "{text}");
}

#[test]
fn prompt_hook_suggests_matching_pages_once_and_is_quiet_otherwise() {
    let env = seeded();
    let ask = |prompt: &str, sid: &str| {
        env.hook(
            "user-prompt-submit",
            serde_json::json!({ "session_id": sid, "cwd": env.repo, "prompt": prompt }),
        )
    };
    let text = context_of(&ask("Why are stripe webhooks retried so often?", "a"));
    assert!(text.starts_with("Relevant wiki pages"), "{text}");
    assert!(text.contains("myapp/billing/stripe — "), "{text}");
    // Not again in the same session…
    assert!(ask("Why are stripe webhooks retried so often?", "a").is_none());
    // …but in another one.
    assert!(ask("Why are stripe webhooks retried so often?", "b").is_some());
    // Unrelated prompts, slash commands and empty prompts print nothing.
    assert!(ask("rename the variable in main.rs to something clearer", "c").is_none());
    assert!(ask("/compact", "c").is_none());
    // One shared generic word ("src", in billing's summary) is not relevance.
    env.write(
        "billing",
        &fm(
            "Billing",
            "Read before touching src/billing, for invoices and plans.",
            "",
            "Returns whole invoices. [[auth]]",
        ),
    )
    .success();
    assert!(ask("Add a unit test for util::short in src/util.rs (a hash shorter than SHORT_HASH is returned whole)", "c").is_none());
    assert!(ask("", "c").is_none());
    // A one-word prompt matching a title is enough.
    assert!(context_of(&ask("login", "d")).contains("myapp/auth"));
    // Hubs are never suggested.
    assert!(
        ask("myapp project overview hub", "e")
            .is_none_or(|v| !context_of(&Some(v)).contains("myapp —"))
    );
}

#[test]
fn post_tool_use_reminds_about_pages_whose_sources_were_edited() {
    let env = seeded();
    env.write(
        "billing",
        &fm(
            "Billing",
            "How subscriptions, invoices and Stripe webhooks fit together.",
            "sources: [src/billing/**, Cargo.toml]\n",
            "[[stripe]] [[auth]]",
        ),
    )
    .success();
    fs::create_dir_all(env.repo.join("src/billing")).unwrap();
    fs::write(env.repo.join("src/billing/retry.rs"), "x").unwrap();
    fs::write(env.repo.join("src/main.rs"), "x").unwrap();
    let edit = |tool: &str, file: &Path, sid: &str| {
        env.hook(
            "post-tool-use",
            serde_json::json!({
                "session_id": sid, "cwd": env.repo, "hook_event_name": "PostToolUse",
                "tool_name": tool, "tool_input": { "file_path": file },
                "tool_response": { "success": true },
            }),
        )
    };
    let out = edit("Edit", &env.repo.join("src/billing/retry.rs"), "a");
    assert_eq!(
        out.as_ref().unwrap()["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    let text = context_of(&out);
    assert!(
        text.contains("src/billing/retry.rs is documented by"),
        "{text}"
    );
    assert!(text.contains("myapp/billing — "), "{text}");
    assert!(text.contains("reg-wiki write"), "{text}");
    // Once per page per session.
    assert!(edit("Write", &env.repo.join("Cargo.toml"), "a").is_none());
    assert!(edit("Write", &env.repo.join("Cargo.toml"), "b").is_some());
    // Files no page covers, other tools, and files outside any project: nothing.
    assert!(edit("Edit", &env.repo.join("src/main.rs"), "c").is_none());
    assert!(edit("Read", &env.repo.join("src/billing/retry.rs"), "c").is_none());
    let outside = env.home.parent().unwrap().join("loose.rs");
    fs::write(&outside, "x").unwrap();
    assert!(edit("Edit", &outside, "c").is_none());
    // Relative paths resolve against the hook's cwd.
    assert!(edit("Edit", Path::new("src/billing/retry.rs"), "d").is_some());

    // Editing a wiki file by hand: told once that it isn't committed.
    let wiki_file = env.wiki().join("myapp/auth.md");
    let text = context_of(&edit("Edit", &wiki_file, "e"));
    assert!(
        text.contains("edited the wiki file myapp/auth.md directly"),
        "{text}"
    );
    assert!(edit("Edit", &wiki_file, "e").is_none());
}

#[test]
fn stop_hook_asks_once_when_code_changed_and_the_wiki_did_not() {
    let env = seeded();
    fs::write(env.repo.join("main.rs"), "x").unwrap();
    let stop = |sid: &str, active: bool| {
        env.hook(
            "stop",
            serde_json::json!({ "session_id": sid, "cwd": env.repo, "stop_hook_active": active }),
        )
    };
    let edit = |sid: &str| {
        env.hook(
            "post-tool-use",
            serde_json::json!({ "session_id": sid, "cwd": env.repo, "tool_name": "Edit",
                                "tool_input": { "file_path": env.repo.join("main.rs") } }),
        )
    };
    // Nothing edited: nothing to say.
    env.hook(
        "session-start",
        env.input(serde_json::json!({ "session_id": "a" })),
    );
    assert!(stop("a", false).is_none());

    edit("a");
    let out = stop("a", false).expect("stop should ask");
    assert_eq!(out["decision"], "block");
    let reason = out["reason"].as_str().unwrap();
    assert!(reason.contains("changed 1 file(s) (main.rs)"), "{reason}");
    assert!(reason.contains("reg-wiki write"), "{reason}");
    // Only once.
    assert!(stop("a", false).is_none());

    // Already continuing because of a stop hook: never block again.
    env.hook(
        "session-start",
        env.input(serde_json::json!({ "session_id": "b" })),
    );
    edit("b");
    assert!(stop("b", true).is_none());

    // A session that wrote to the wiki isn't asked. Without REG_SESSION, any write since
    // the session started counts.
    env.hook(
        "session-start",
        env.input(serde_json::json!({ "session_id": "c" })),
    );
    edit("c");
    env.cmd()
        .args(["write", "auth", "-m", "t"])
        .write_stdin("# Auth\n\nNew.\n")
        .assert()
        .success();
    assert!(stop("c", false).is_none());

    // With REG_SESSION, only this session's writes count.
    let envfile = env.home.join("claude-env");
    let ef = [("CLAUDE_ENV_FILE", envfile.to_str().unwrap())];
    env.hook_with(
        "session-start",
        env.input(serde_json::json!({ "session_id": "d" })),
        &ef,
    );
    env.hook_with(
        "session-start",
        env.input(serde_json::json!({ "session_id": "e" })),
        &ef,
    );
    edit("d");
    edit("e");
    env.cmd()
        .env("REG_SESSION", "d")
        .args(["write", "auth", "-m", "t"])
        .write_stdin("# Auth\n\nNewer.\n")
        .assert()
        .success();
    assert!(stop("d", false).is_none());
    assert!(stop("e", false).is_some());
}

#[test]
fn hooks_never_fail_and_do_nothing_for_the_librarian() {
    let env = seeded();
    let input = env.input(serde_json::json!({ "prompt": "stripe webhooks" }));
    assert!(env.hook("user-prompt-submit", input.clone()).is_some());
    assert!(
        env.hook_with(
            "user-prompt-submit",
            input.clone(),
            &[("REG_LIBRARIAN", "1")]
        )
        .is_none()
    );
    // Garbage input, unknown events and a broken REG_HOME: exit 0, nothing on stdout.
    for (event, stdin) in [
        ("session-start", "not json"),
        ("pre-tool-use", "{}"),
        ("stop", ""),
    ] {
        env.cmd()
            .args(["hook", event])
            .write_stdin(stdin)
            .assert()
            .success()
            .stdout("");
    }
    let file = env.home.parent().unwrap().join("a-file");
    fs::write(&file, "x").unwrap();
    env.cmd()
        .env("REG_HOME", &file)
        .args(["hook", "session-start"])
        .write_stdin(input.to_string())
        .assert()
        .success()
        .stdout("");
}

// ---- install / uninstall ----

struct Claude {
    env: Env,
    home: PathBuf,
}

impl Claude {
    fn new() -> Claude {
        let env = Env::new();
        let home = env.home.parent().unwrap().join("userhome");
        fs::create_dir_all(home.join(".claude")).unwrap();
        Claude { env, home }
    }

    fn cmd(&self) -> Command {
        let mut c = self.env.cmd();
        c.env("HOME", &self.home);
        c
    }

    fn run(&self, args: &[&str]) -> assert_cmd::assert::Assert {
        self.cmd().args(args).assert()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(".claude").join(rel)
    }

    fn settings(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(self.path("settings.json")).unwrap()).unwrap()
    }

    /// Every file under ~/.claude with its content, for before/after comparisons.
    fn snapshot(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack = vec![self.home.join(".claude")];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    out.push((p.display().to_string(), "<dir>".into()));
                    stack.push(p);
                } else {
                    out.push((p.display().to_string(), fs::read_to_string(&p).unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}

const USER_SETTINGS: &str = r#"{
  "model": "opus",
  "permissions": {
    "allow": [
      "Bash(npm run test *)"
    ],
    "deny": []
  },
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "prettier --write"
          }
        ]
      }
    ]
  },
  "zzz_unknown": {
    "kept": true
  }
}
"#;

#[test]
fn install_merges_into_existing_settings_and_uninstall_restores_them() {
    let c = Claude::new();
    fs::write(c.path("settings.json"), USER_SETTINGS).unwrap();
    fs::write(c.path("CLAUDE.md"), "# My rules\n\nBe brief.\n").unwrap();
    let before = c.snapshot();

    c.run(&["install", "--yes"])
        .success()
        .stdout(predicate::str::contains("settings.json"));

    // Skill files.
    let skill = fs::read_to_string(c.path("skills/reg-wiki/SKILL.md")).unwrap();
    assert!(
        skill.starts_with("---\nname: reg-wiki\ndescription: "),
        "{skill}"
    );
    for f in ["references/writing-pages.md", "references/commands.md"] {
        assert!(c.path("skills/reg-wiki").join(f).is_file(), "{f}");
    }

    // Settings: user keys untouched and in order, our hooks added next to theirs.
    let s = c.settings();
    let keys: Vec<&String> = s.as_object().unwrap().keys().collect();
    assert_eq!(keys, ["model", "permissions", "hooks", "zzz_unknown"]);
    assert_eq!(s["zzz_unknown"]["kept"], true);
    assert_eq!(
        s["permissions"]["allow"],
        serde_json::json!(["Bash(npm run test *)", "Bash(reg-wiki:*)"])
    );
    let post = s["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 2);
    assert_eq!(post[0]["hooks"][0]["command"], "prettier --write");
    assert_eq!(post[1]["matcher"], "Edit|Write|MultiEdit|NotebookEdit");
    let cmd = post[1]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.ends_with("reg-wiki hook post-tool-use"), "{cmd}");
    assert_eq!(
        s["hooks"]["SessionStart"][0]["matcher"],
        "startup|clear|compact"
    );
    assert!(s["hooks"]["UserPromptSubmit"][0].get("matcher").is_none());
    // Stop is opt-in.
    assert!(s["hooks"].get("Stop").is_none());

    let md = fs::read_to_string(c.path("CLAUDE.md")).unwrap();
    assert!(
        md.starts_with("# My rules\n\nBe brief.\n\n<!-- reg-wiki:start -->"),
        "{md}"
    );
    assert!(md.ends_with("<!-- reg-wiki:end -->\n"), "{md}");

    // Idempotent: a second install changes nothing.
    let installed = c.snapshot();
    c.run(&["install", "--yes"])
        .success()
        .stdout(predicate::str::contains("already installed"));
    assert_eq!(c.snapshot(), installed);

    // Uninstall puts every file back exactly as it was.
    c.run(&["uninstall", "--yes"]).success();
    assert_eq!(c.snapshot(), before);
    c.run(&["uninstall", "--yes"])
        .success()
        .stdout(predicate::str::contains("not installed"));
}

#[test]
fn install_from_scratch_and_uninstall_leaves_nothing() {
    let c = Claude::new();
    let before = c.snapshot();
    let v = {
        let out = c
            .cmd()
            .args(["install", "--yes", "--stop-hook", "--json"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    assert_eq!(v["action"], "install");
    assert!(v["changes"].as_array().unwrap().len() >= 5, "{v}");
    let s = c.settings();
    assert!(
        s["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .ends_with("reg-wiki hook stop")
    );
    assert_eq!(
        s["permissions"]["allow"],
        serde_json::json!(["Bash(reg-wiki:*)"])
    );

    // Re-running without --stop-hook keeps the Stop hook.
    c.run(&["install", "--yes"])
        .success()
        .stdout(predicate::str::contains("already installed"));
    assert!(c.settings()["hooks"].get("Stop").is_some());

    c.run(&["uninstall", "--yes"]).success();
    assert_eq!(c.snapshot(), before);
}

#[test]
fn install_respects_existing_permissions_and_no_permissions() {
    let c = Claude::new();
    fs::write(
        c.path("settings.json"),
        r#"{"permissions":{"allow":["Bash(reg-wiki *)"]}}"#,
    )
    .unwrap();
    c.run(&["install", "--yes"]).success();
    // Already allowed in the newer spelling: not added twice…
    assert_eq!(
        c.settings()["permissions"]["allow"],
        serde_json::json!(["Bash(reg-wiki *)"])
    );
    // …and not removed by uninstall, since install didn't add it.
    c.run(&["uninstall", "--yes"]).success();
    assert_eq!(
        c.settings(),
        serde_json::json!({"permissions":{"allow":["Bash(reg-wiki *)"]}})
    );

    let c = Claude::new();
    c.run(&["install", "--yes", "--no-permissions"]).success();
    assert!(c.settings().get("permissions").is_none());
    // Adding it later and taking it away again.
    c.run(&["install", "--yes"]).success();
    assert_eq!(
        c.settings()["permissions"]["allow"],
        serde_json::json!(["Bash(reg-wiki:*)"])
    );
    c.run(&["install", "--yes", "--no-permissions"]).success();
    assert!(c.settings().get("permissions").is_none());
}

#[test]
fn install_refuses_bad_settings_and_asks_before_writing() {
    let c = Claude::new();
    fs::write(c.path("settings.json"), "{ not json").unwrap();
    c.run(&["install", "--yes"])
        .failure()
        .stderr(predicate::str::contains("isn't valid JSON"));
    assert_eq!(
        fs::read_to_string(c.path("settings.json")).unwrap(),
        "{ not json"
    );

    let c = Claude::new();
    // No --yes and no terminal to ask on: refuse rather than guess.
    c.cmd()
        .arg("install")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--yes"));
    assert!(!c.path("settings.json").exists());
    c.run(&["install", "--yes", "--target", "codex"])
        .failure()
        .stderr(predicate::str::contains("unknown target"));
}

#[test]
fn install_honours_claude_config_dir_and_prints_config() {
    let c = Claude::new();
    let dir = c.home.join("elsewhere");
    c.cmd()
        .env("CLAUDE_CONFIG_DIR", &dir)
        .args(["install", "--yes"])
        .assert()
        .success();
    assert!(dir.join("skills/reg-wiki/SKILL.md").is_file());
    assert!(dir.join("settings.json").is_file());
    assert!(!c.path("settings.json").exists());

    let out = c
        .cmd()
        .args(["install", "--print-config", "claude"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    for want in [
        "\"UserPromptSubmit\"",
        "Bash(reg-wiki:*)",
        "<!-- reg-wiki:start -->",
        "name: reg-wiki",
    ] {
        assert!(text.contains(want), "{want} missing from:\n{text}");
    }
    // Printing writes nothing.
    assert!(!c.path("settings.json").exists());
}

#[test]
fn installed_hook_commands_run() {
    // The commands install writes are the ones the hook dispatcher accepts.
    let c = Claude::new();
    c.run(&["install", "--yes", "--stop-hook"]).success();
    let s = c.settings();
    for (event, groups) in s["hooks"].as_object().unwrap() {
        let cmd = groups[0]["hooks"][0]["command"].as_str().unwrap();
        let arg = cmd.rsplit(' ').next().unwrap();
        let out = c
            .cmd()
            .args(["hook", arg])
            .write_stdin(
                c.env
                    .input(serde_json::json!({ "hook_event_name": event }))
                    .to_string(),
            )
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("unknown hook event"),
            "{event}: {cmd}"
        );
    }
}

#[test]
fn closed_stdout_is_not_a_panic() {
    let env = seeded();
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("reg-wiki"))
        .current_dir(&env.repo)
        .env("REG_HOME", &env.home)
        .args(["read", "billing"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}

#[test]
fn new_takes_sources_and_does_not_flag_the_hub_as_similar() {
    let env = Env::new();
    env.cmd()
        .args([
            "new",
            "project-detection",
            "--title",
            "Project detection",
            "--summary",
            "Read before changing how the cwd maps to a project.",
            "--source",
            "src/project.rs",
            "--source",
            "src/home.rs",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains("similar").not());
    let text = fs::read_to_string(env.wiki().join("myapp/project-detection.md")).unwrap();
    assert!(
        text.contains("sources: [src/project.rs, src/home.rs]\n"),
        "{text}"
    );
}

/// The JSON embedded in a `view` page.
fn view_data(html: &str) -> Value {
    let start = html
        .find(r#"<script id="data" type="application/json">"#)
        .unwrap();
    let rest = &html[start..];
    let rest = &rest[rest.find('>').unwrap() + 1..];
    serde_json::from_str(&rest[..rest.find("</script>").unwrap()]).unwrap()
}

#[test]
fn view_writes_a_self_contained_graph_of_the_scope() {
    let env = seeded();
    env.write(
        "_shared/rust",
        &fm(
            "Rust",
            "Error handling across projects.",
            "",
            "Close </script> early? <b>no</b>",
        ),
    )
    .success();

    // Default location, with --no-open so the test doesn't start a browser.
    let v = env.json(&["view", "--no-open"]);
    let path = PathBuf::from(v["path"].as_str().unwrap());
    assert_eq!(path, env.home.join("view.html"));
    assert_eq!(v["opened"], false);
    let html = fs::read_to_string(&path).unwrap();
    assert!(
        !html.contains("{{"),
        "template placeholders left in the page"
    );
    assert!(
        !html.contains("<script src"),
        "the page must not load scripts"
    );

    let g = view_data(&html);
    let nodes = g["nodes"].as_array().unwrap();
    let slugs: Vec<&str> = nodes.iter().map(|n| n["slug"].as_str().unwrap()).collect();
    assert!(
        slugs.contains(&"myapp/billing") && slugs.contains(&"_shared/rust"),
        "{slugs:?}"
    );
    let idx = |s: &str| slugs.iter().position(|x| *x == s).unwrap();
    let billing = &nodes[idx("myapp/billing")];
    assert_eq!(billing["project"], "myapp");
    assert!(
        billing["html"]
            .as_str()
            .unwrap()
            .contains(r##"href="#p=myapp/auth""##)
    );
    let stripe = &nodes[idx("myapp/billing/stripe")];
    assert_eq!(stripe["is_ref"], true);
    assert_eq!(stripe["parent"], "myapp/billing");
    assert!(nodes[idx("myapp")]["hub"].as_bool().unwrap());
    let rust = nodes[idx("_shared/rust")]["html"].as_str().unwrap();
    assert!(
        rust.contains("&lt;/script&gt;") && !rust.contains("<b>"),
        "{rust}"
    );

    // One edge per linked pair; the ref's edge to its parent is marked.
    let edges = g["edges"].as_array().unwrap();
    let has = |a: &str, b: &str, kind: &str| {
        let (a, b) = (idx(a), idx(b));
        edges.iter().any(|e| {
            let (s, t) = (
                e["s"].as_u64().unwrap() as usize,
                e["t"].as_u64().unwrap() as usize,
            );
            ((s, t) == (a, b) || (s, t) == (b, a)) && e["kind"] == kind
        })
    };
    assert!(has("myapp/billing", "myapp/auth", "link"));
    assert!(has("myapp/billing", "myapp/billing/stripe", "ref"));
    assert_eq!(
        edges
            .iter()
            .filter(|e| {
                let p = [
                    e["s"].as_u64().unwrap() as usize,
                    e["t"].as_u64().unwrap() as usize,
                ];
                p.contains(&idx("myapp/billing")) && p.contains(&idx("myapp/auth"))
            })
            .count(),
        1
    );

    // Another project's pages only show up with --all.
    let other = env.repo.parent().unwrap().join("other");
    fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "-q"]);
    env.cmd_in(&other)
        .args(["project", "add"])
        .assert()
        .success();
    let mine = env.cmd().args(["view", "--out", "-"]).output().unwrap();
    assert!(
        !view_data(&String::from_utf8_lossy(&mine.stdout))["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["project"] == "other")
    );
    let out = env.repo.join("all.html");
    env.cmd()
        .args(["view", "--all", "--no-open", "-o", "all.html"])
        .assert()
        .success()
        .stdout(predicate::str::contains("all.html"));
    let all = view_data(&fs::read_to_string(out).unwrap());
    assert!(
        all["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["slug"] == "other")
    );
    assert_eq!(all["scope"], Value::Null);
}

// ---- Milestone 5: the librarian ----

/// A seeded wiki whose librarian is `tests/fake_acp.py`, answering with `replies` in turn.
struct Lib {
    env: Env,
    log: PathBuf,
    replies: PathBuf,
}

impl Lib {
    fn new(replies: &[&str]) -> Lib {
        Lib::with(seeded(), replies, "")
    }

    fn with(env: Env, replies: &[&str], extra_config: &str) -> Lib {
        let dir = env.home.parent().unwrap().to_path_buf();
        let log = dir.join("acp.log");
        let replies_path = dir.join("replies.txt");
        fs::write(&replies_path, replies.join("\n=====\n")).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fake_acp.py");
        let cfg = env.home.join("config.toml");
        let mut text = fs::read_to_string(&cfg).unwrap();
        text.push_str(&format!(
            "\n[librarian]\nagent = \"fake\"\nmodel = \"haiku\"\ntimeout_secs = 5\n{extra_config}\n\
             [agents.fake]\ncommand = \"python3\"\nargs = [{:?}]\n",
            script.display().to_string()
        ));
        fs::write(&cfg, text).unwrap();
        Lib {
            env,
            log,
            replies: replies_path,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = self.env.cmd();
        c.env("FAKE_ACP_LOG", &self.log)
            .env("FAKE_ACP_REPLIES", &self.replies)
            .env("CLAUDECODE", "1");
        c
    }

    fn json(&self, args: &[&str], stdin: &str) -> Value {
        let out = self
            .cmd()
            .args(args)
            .arg("--json")
            .write_stdin(stdin)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// Messages the fake agent received.
    fn received(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn prompts(&self) -> Vec<String> {
        self.received()
            .iter()
            .filter(|m| m["method"] == "session/prompt")
            .map(|m| {
                m["params"]["prompt"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }
}

#[test]
fn ai_query_answers_from_ranked_pages_and_checks_sources() {
    let lib = Lib::new(&[
        "<answer>\nInvoices go out monthly.\n</answer>\n<sources>\n- myapp/billing — billing\nmyapp/nope\n</sources>\n<gaps>\n- how refunds work\n</gaps>\n<need>\n</need>",
    ]);
    let v = lib.json(&["ai", "query", "when", "do", "invoices", "go", "out"], "");
    assert_eq!(v["answer"], "Invoices go out monthly.");
    assert_eq!(slugs_of(&v, "sources"), ["myapp/billing"]);
    assert_eq!(v["unknown_sources"][0], "myapp/nope");
    assert_eq!(v["gaps"][0], "how refunds work");
    assert_eq!(
        v["model"], "claude-haiku-5-5",
        "`haiku` matched the offered model"
    );
    assert_eq!(v["rounds"], 1);

    let msgs = lib.received();
    // Protocol: no fs or terminal, an empty temp dir as cwd, the recursion guard set,
    // and the configured model chosen through configOptions.
    let init = msgs.iter().find(|m| m["method"] == "initialize").unwrap();
    assert_eq!(init["params"]["clientCapabilities"]["terminal"], false);
    assert_eq!(
        init["params"]["clientCapabilities"]["fs"]["readTextFile"],
        false
    );
    let env = msgs.iter().find(|m| m.get("env").is_some()).unwrap();
    assert_eq!(env["env"]["REG_LIBRARIAN"], "1");
    assert!(env["env"]["CLAUDECODE"].is_null());
    let new = msgs.iter().find(|m| m["method"] == "session/new").unwrap();
    let cwd = new["params"]["cwd"].as_str().unwrap();
    assert!(
        cwd.contains("reg-librarian-") && !Path::new(cwd).exists(),
        "removed afterwards"
    );
    let set = msgs
        .iter()
        .find(|m| m["method"] == "session/set_config_option")
        .unwrap();
    assert_eq!(set["params"]["configId"], "model");
    assert_eq!(set["params"]["value"], "claude-haiku-5-5");

    // The prompt has the question and the best page in full, refs by summary.
    let p = &lib.prompts()[0];
    assert!(p.contains("Question: when do invoices go out"), "{p}");
    assert!(p.contains("## Billing (myapp/billing)") && p.contains("Invoices go out monthly."));
    assert!(
        p.contains("- myapp/billing/stripe — Webhook retries"),
        "{p}"
    );

    // Gaps are recorded and show up in stats.
    let s = lib.env.json(&["stats"]);
    assert_eq!(s["librarian_gaps"][0]["name"], "how refunds work");

    // Text output.
    let lib = Lib::with(Env::new(), &[], "");
    lib.env
        .write(
            "billing",
            &fm(
                "Billing",
                "How invoices work and when they are sent.",
                "",
                "Monthly.",
            ),
        )
        .success();
    fs::write(
        &lib.replies,
        "<answer>Monthly.</answer><sources>myapp/billing</sources><gaps></gaps><need></need>",
    )
    .unwrap();
    lib.cmd()
        .args(["ai", "query", "invoices"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Monthly.\n\nSources:\n- myapp/billing — How invoices work",
        ))
        .stdout(predicate::str::contains("Not in the wiki").not());
}

#[test]
fn ai_query_sends_needed_pages_in_one_follow_up() {
    let lib = Lib::new(&[
        "<answer>partial</answer><sources>myapp/billing</sources><gaps></gaps><need>\nmyapp/billing/stripe\nmyapp/billing\n</need>",
        "<answer>Webhooks are retried.</answer><sources>myapp/billing/stripe</sources><gaps></gaps><need>myapp/auth</need>",
    ]);
    let v = lib.json(&["ai", "query", "invoices"], "");
    assert_eq!(v["answer"], "Webhooks are retried.");
    assert_eq!(v["rounds"], 2);
    let prompts = lib.prompts();
    assert_eq!(
        prompts.len(),
        2,
        "no third round even though it asked again"
    );
    assert!(prompts[1].contains("## Stripe webhooks (myapp/billing/stripe)"));
    assert!(
        !prompts[1].contains("## Billing (myapp/billing)"),
        "already shown in full"
    );
}

#[test]
fn ai_query_falls_back_to_search() {
    // Off: no [librarian] section.
    let env = seeded();
    env.cmd()
        .args(["ai", "query", "invoices"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "myapp/billing — How subscriptions",
        ))
        .stderr(predicate::str::contains(
            "no librarian answer (the librarian is off",
        ));

    // A reply it can't parse.
    let lib = Lib::new(&["I think it's monthly?"]);
    let v = lib.json(&["ai", "query", "invoices"], "");
    assert!(v["answer"].is_null());
    assert!(v["fallback"].as_str().unwrap().contains("no <answer>"));
    assert_eq!(slugs_of(&v, "hits")[0], "myapp/billing");

    // The agent dies, or hangs past the timeout (which is cancelled).
    for mode in ["exit", "hang"] {
        let lib = Lib::with(seeded(), &[], "");
        let cfg = lib.env.home.join("config.toml");
        let text = fs::read_to_string(&cfg)
            .unwrap()
            .replace("timeout_secs = 5", "timeout_secs = 2");
        fs::write(&cfg, text).unwrap();
        let start = std::time::Instant::now();
        let out = lib
            .cmd()
            .env("FAKE_ACP_MODE", mode)
            .args(["ai", "query", "invoices"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(start.elapsed().as_secs() < 10, "{mode} took too long");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("no librarian answer"), "{mode}: {err}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("myapp/billing"));
        if mode == "hang" {
            assert!(err.contains("timed out"), "{err}");
            assert!(
                lib.received()
                    .iter()
                    .any(|m| m["method"] == "session/cancel")
            );
        }
    }

    // A model the agent doesn't offer: a warning, and the agent's default is used.
    let lib = Lib::with(seeded(), &["<answer>ok</answer>"], "");
    let cfg = lib.env.home.join("config.toml");
    let text = fs::read_to_string(&cfg)
        .unwrap()
        .replace("model = \"haiku\"", "model = \"gpt-9\"");
    fs::write(&cfg, text).unwrap();
    let out = lib
        .cmd()
        .args(["ai", "query", "invoices", "--json"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("doesn't offer model `gpt-9`"));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["answer"], "ok");
    assert_eq!(v["model"], "default");
}

#[test]
fn librarian_permission_requests_are_rejected() {
    let lib = Lib::new(&["<answer>ok</answer>"]);
    let out = lib
        .cmd()
        .env("FAKE_ACP_MODE", "permission")
        .args(["ai", "query", "invoices", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let msgs = lib.received();
    let answers: Vec<&Value> = msgs
        .iter()
        .filter(|m| m.get("method").is_none() && m.get("id").is_some())
        .collect();
    assert_eq!(answers[0]["result"]["outcome"]["outcome"], "selected");
    assert_eq!(answers[0]["result"]["outcome"]["optionId"], "no");
    assert_eq!(
        answers[1]["error"]["code"], -32601,
        "fs requests are refused"
    );
}

#[test]
fn ai_models_lists_what_the_agent_offers() {
    let lib = Lib::new(&[]);
    let v = lib.json(&["ai", "models"], "");
    let ids: Vec<&str> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["default", "claude-haiku-5-5", "claude-sonnet-5-5"]);
    assert_eq!(v["current"], "claude-haiku-5-5");
    lib.cmd()
        .args(["ai", "models"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "* claude-haiku-5-5  (Haiku) — Fastest",
        ));
}

#[test]
fn ai_record_writes_checked_proposals_as_the_librarian() {
    let reply = "<page slug=\"myapp/billing\">\n```markdown\n---\ntitle: Billing\nsummary: How subscriptions, invoices and Stripe webhooks fit together.\n---\n\n# Billing\n\nInvoices go out monthly, on the 1st (UTC). See [[stripe]] and [[auth]].\n```\n</page>\n\
<page slug=\"myapp/refunds\">\n---\ntitle: Refunds\nsummary: How refunds are issued and why they take five days. Read before touching src/refunds.\nsources: [src/refunds/**]\n---\n\n# Refunds\n\nIssued through [[billing]].\n</page>\n\
<page slug=\"billing/proration\">\n---\ntitle: Proration\nsummary: How plan changes are prorated mid-cycle.\n---\nProrated daily.\n</page>\n\
<page slug=\"myapp/auth\">\n---\ntitle: Auth\nsummary: x\n---\nnot shown in full, so refused\n</page>\n\
<page slug=\"myapp/bad\">\n---\ntitle: Bad\n---\nno summary\n</page>\n\
<message>Record the invoice date and refunds</message>";
    let lib = Lib::new(&[reply, reply]);
    let notes = "Invoices are sent on the 1st, UTC. Refunds take five days because of the bank.";

    // Dry run: nothing written.
    let v = lib.json(&["ai", "record", "--dry-run"], notes);
    assert_eq!(v["dry_run"], true);
    assert!(v["commit"].is_null());
    assert!(!lib.env.wiki().join("myapp/refunds.md").exists());

    let before = lib.env.log().len();
    let v = lib.json(&["ai", "record"], notes);
    let action = |slug: &str| {
        v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["slug"] == slug)
            .map(|c| c["action"].as_str().unwrap().to_string())
    };
    assert_eq!(action("myapp/billing").as_deref(), Some("updated"));
    assert_eq!(action("myapp/refunds").as_deref(), Some("created"));
    assert_eq!(
        action("myapp/billing/proration").as_deref(),
        Some("created")
    );
    assert_eq!(action("myapp/auth").as_deref(), Some("refused"));
    assert_eq!(action("myapp/bad").as_deref(), Some("refused"));

    // The proposals went through the same checks and bookkeeping as `write`.
    let billing = fs::read_to_string(lib.env.wiki().join("myapp/billing.md")).unwrap();
    assert!(
        billing.contains("on the 1st (UTC)")
            && billing.contains("created: ")
            && billing.contains("updated: ")
    );
    assert!(!billing.contains("```"), "fence removed");
    assert!(
        lib.env
            .wiki()
            .join("myapp/billing/refs/proration.md")
            .exists(),
        "a ref of billing"
    );

    // One commit, authored by the librarian, listing what changed.
    let log = lib.env.log();
    assert_eq!(log.len(), before + 1);
    assert_eq!(
        log[0],
        "librarian (claude-haiku-5-5)|Record the invoice date and refunds"
    );

    // The prompt had the notes, the matching pages in full and the rest by summary.
    let p = &lib.prompts()[1];
    assert!(p.contains(notes));
    assert!(
        p.contains("<file slug=\"myapp/billing\">\n---\ntitle: Billing"),
        "{p}"
    );
    assert!(
        p.contains("summary says when to read the page"),
        "writing rules included"
    );
}

#[test]
fn ai_tend_fixes_lint_problems_and_merges_duplicates() {
    let env = Env::new();
    env.write(
        "billing",
        &fm(
            "Billing",
            "How invoices are created and sent each month.",
            "",
            "See [[invoicing]] and [[missing-page]].",
        ),
    )
    .success();
    env.write(
        "invoicing",
        &fm(
            "Invoicing",
            "How invoices are created and sent every month.",
            "",
            "Monthly invoices. Linked from [[billing]].",
        ),
    )
    .success();
    env.write(
        "lonely",
        &fm(
            "Lonely",
            "A page that nothing links to, about caching rules.",
            "",
            "Cache for 5 minutes.",
        ),
    )
    .success();
    let fixed_billing = "---\ntitle: Billing\nsummary: How invoices are created and sent each month.\n---\n\nMonthly invoices. Caching: see [[lonely]].\n";
    let reply = format!(
        "<page slug=\"myapp/billing\">\n{fixed_billing}</page>\n<delete slug=\"myapp/invoicing\"/>\n<delete slug=\"myapp/lonely\"/>\n<message>Merge invoicing into billing</message>"
    );
    let lib = Lib::with(env, &[&reply], "");
    let v = lib.json(&["ai", "tend"], "");
    let action = |slug: &str| {
        v["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["slug"] == slug)
            .map(|c| {
                (
                    c["action"].as_str().unwrap().to_string(),
                    c["reason"].as_str().unwrap_or("").to_string(),
                )
            })
    };
    assert_eq!(action("myapp/billing").unwrap().0, "updated");
    assert_eq!(action("myapp/invoicing").unwrap().0, "deleted");
    let (a, why) = action("myapp/lonely").unwrap();
    assert_eq!(a, "refused");
    assert!(why.contains("links to it"), "{why}");
    assert!(!lib.env.wiki().join("myapp/invoicing.md").exists());
    assert!(lib.env.wiki().join("myapp/lonely.md").exists());

    let p = &lib.prompts()[0];
    assert!(p.contains("[broken]") || p.contains("missing-page"), "{p}");
    assert!(p.contains("[duplicate?]"), "{p}");
    assert!(p.contains("<file slug=\"myapp/invoicing\">"), "{p}");
    assert!(
        p.contains("<file slug=\"myapp\">"),
        "the hub, to link the orphan from: {p}"
    );

    // A clean wiki needs no librarian.
    let env = Env::new();
    env.cmd()
        .args(["ai", "tend"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "nothing for the librarian to tend",
        ));
}

#[test]
fn install_sets_up_the_librarian_without_starting_it() {
    let c = Claude::new();
    c.run(&["install", "--yes"]).success();
    let cfg = fs::read_to_string(c.env.home.join("config.toml")).unwrap();
    assert!(
        cfg.contains(
            "[librarian]\nagent = \"claude\"\nenabled = true\nmodel = \"claude-haiku-5-5\""
        ),
        "{cfg}"
    );
    assert!(cfg.contains("[agents.claude]\ncommand = \"npx\"\nargs = [\"-y\", \"@agentclientprotocol/claude-agent-acp\"]"), "{cfg}");
    assert!(cfg.contains("[[projects]]"), "projects kept");

    // Again: nothing changes. A chosen model and turning it off are kept.
    c.run(&["install", "--yes"])
        .success()
        .stdout(predicate::str::contains("nothing to change"));
    c.run(&["install", "--yes", "--librarian-model", "claude-sonnet-5-5"])
        .success();
    let cfg = fs::read_to_string(c.env.home.join("config.toml")).unwrap();
    assert!(cfg.contains("model = \"claude-sonnet-5-5\""));
    c.run(&["install", "--yes", "--no-librarian"]).success();
    let cfg = fs::read_to_string(c.env.home.join("config.toml")).unwrap();
    assert!(cfg.contains("enabled = false") && cfg.contains("claude-sonnet-5-5"));
}
