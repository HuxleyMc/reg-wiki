//! `reg-wiki serve --mcp`: the wiki as an MCP server (stdio, newline-delimited JSON-RPC),
//! for agents that reach tools through MCP rather than a shell: Codex, Cursor and Gemini
//! CLI, or any other MCP client.
//!
//! Each tool call runs `reg-wiki` itself as a subprocess with the matching command and
//! returns its text output, so the server and the CLI can't drift apart and a bad call
//! can't take the server down. Commands take milliseconds, so this costs nothing that
//! matters.
//!
//! The current project comes from the client's first root (`roots/list`) when it offers
//! one, else from the directory the server was started in; every tool also takes
//! `project` and `all`. Inside the librarian (`REG_LIBRARIAN=1`) it offers no tools: the
//! librarian only reads the text reg-wiki sends it.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

use crate::Cli;

/// Newest first; we answer with the client's version when we know it.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const ROOTS_REQUEST: &str = "reg-wiki-roots";

pub const INSTRUCTIONS: &str = "\
reg-wiki is a wiki of linked Markdown pages that records how projects work: decisions and \
their reasons, gotchas, how parts fit together. It is the memory that outlives this session; \
people review every write through git.

When to use it: not on every task. A small change you can understand from the code doesn't \
need it. Look when you need what earlier sessions learned: answering a question about how \
or why something works, stuck on a problem that may have a recorded cause, picking up \
earlier work, or working across repositories.

1. To look: wiki_context with a question (cheapest), or wiki_query to pick from ranked \
matches, then wiki_read for one page in full. Follow refs and links only when their summary \
says you need them. If the wiki disagrees with the code, the code wins and the page needs \
fixing.
2. When you learn something a later session would want (a decision and its reason, a \
gotcha, how parts fit together, notes on a larger feature in progress), record it: update the closest \
page (wiki_read it, then wiki_write the whole page with base set to the hash from the read \
and message saying why), or wiki_write a new slug with full frontmatter (title, summary, \
tags, sources). Search before you add a page; updating beats a duplicate.

Writing pages: one topic per page. The summary (at most 200 characters) says when to read the \
page. Keep the main page under about 150 lines and put detail in refs (wiki_new with parent). \
Link with [[slug]]. Record the why, not a diary of the session and not copies of code. Set \
sources to globs of the code the page describes, relative to the project root.

Slugs are paths like myapp/billing or _shared/topic; inside a project, billing means \
<project>/billing.";

pub fn run(cli: &Cli) -> Result<ExitCode> {
    let exe = std::env::current_exe().context("finding the reg-wiki executable")?;
    let mut server = Server {
        exe,
        cwd: std::env::current_dir()?,
        agent: cli.agent.clone(),
        project: cli.project.clone(),
        all: cli.all,
        disabled: std::env::var("REG_LIBRARIAN").is_ok_and(|v| v == "1"),
        roots_capable: false,
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("reading from the client")?;
        if line.trim().is_empty() {
            continue;
        }
        let replies = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(batch)) => batch.iter().filter_map(|m| server.handle(m)).collect(),
            Ok(msg) => server.handle(&msg).into_iter().collect(),
            Err(e) => vec![error(Value::Null, -32700, &format!("parse error: {e}"))],
        };
        for r in replies {
            writeln!(out, "{r}")?;
        }
        out.flush()?;
    }
    Ok(ExitCode::SUCCESS)
}

struct Server {
    exe: PathBuf,
    cwd: PathBuf,
    agent: Option<String>,
    project: Option<String>,
    all: bool,
    disabled: bool,
    roots_capable: bool,
}

fn reply(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

impl Server {
    fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to a request of ours.
            if id.as_ref().and_then(Value::as_str) == Some(ROOTS_REQUEST) {
                self.take_roots(msg);
            }
            return None;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else {
            // Notifications.
            if method == "notifications/initialized" || method == "notifications/roots/list_changed"
            {
                return self.roots_capable.then(roots_request);
            }
            return None;
        };
        Some(match method {
            "initialize" => reply(id, self.initialize(&params)),
            "ping" => reply(id, json!({})),
            "tools/list" => reply(
                id,
                json!({ "tools": if self.disabled { json!([]) } else { tools() } }),
            ),
            "tools/call" => match self.call(&params) {
                Ok(r) => reply(id, r),
                Err(e) => error(id, -32602, &format!("{e:#}")),
            },
            "resources/list" => reply(id, json!({ "resources": [] })),
            "prompts/list" => reply(id, json!({ "prompts": [] })),
            _ => error(id, -32601, &format!("method not found: {method}")),
        })
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str);
        let version = asked
            .filter(|v| PROTOCOL_VERSIONS.contains(v))
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        self.roots_capable = params.pointer("/capabilities/roots").is_some();
        if self.agent.is_none() {
            self.agent = params
                .pointer("/clientInfo/name")
                .and_then(Value::as_str)
                .map(agent_name);
        }
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "reg-wiki", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        })
    }

    /// The first `file://` root becomes the directory the project is detected from.
    fn take_roots(&mut self, msg: &Value) {
        let Some(roots) = msg.pointer("/result/roots").and_then(Value::as_array) else {
            return;
        };
        let dir = roots
            .iter()
            .filter_map(|r| r.get("uri").and_then(Value::as_str))
            .filter_map(file_uri_path)
            .find(|p| p.is_dir());
        if let Some(d) = dir {
            self.cwd = d;
        }
    }

    fn call(&self, params: &Value) -> Result<Value> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .context("tools/call needs a tool name")?;
        let empty = Map::new();
        let args = params
            .get("arguments")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        if self.disabled {
            anyhow::bail!("reg-wiki offers no tools inside the librarian");
        }
        let Some((argv, stdin)) = command_for(name, args)? else {
            anyhow::bail!("unknown tool `{name}`");
        };
        Ok(self.exec(&argv, stdin.as_deref(), args))
    }

    fn exec(&self, argv: &[String], stdin: Option<&str>, args: &Map<String, Value>) -> Value {
        let mut c = Command::new(&self.exe);
        c.current_dir(&self.cwd);
        // Global options go before the subcommand: its arguments end with `--`.
        let project = s(args, "project").or(self.project.as_deref());
        match project {
            Some(p) => {
                c.args(["--project", p]);
            }
            None => {
                if args.get("all").and_then(Value::as_bool).unwrap_or(self.all) {
                    c.arg("--all");
                }
            }
        }
        c.args(argv);
        if let Some(a) = &self.agent {
            c.env("REG_AGENT", a);
        }
        c.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let result = c.spawn().and_then(|mut child| {
            if let Some(text) = stdin
                && let Some(mut w) = child.stdin.take()
            {
                // A command that fails early may not read it all; that's reported below.
                let _ = w.write_all(text.as_bytes());
            }
            child.wait_with_output()
        });
        let out = match result {
            Ok(o) => o,
            Err(e) => return text_result(&format!("could not run reg-wiki: {e}"), true),
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let code = out.status.code().unwrap_or(1);
        let mut text = stdout.trim_end().to_string();
        let note = stderr.trim();
        if code == 3 {
            // write --base conflict: stdout holds the current page, stderr the new hash.
            text = format!(
                "conflict: the page changed since you read it. {note}\nMerge your change into \
                 the current content below and write again with the new hash as base.\n\n{text}"
            );
        } else if !note.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(note);
        }
        if text.is_empty() {
            text = if code == 0 { "done" } else { "failed" }.to_string();
        }
        // `lint` exits 1 when it finds errors; that's a result, not a failure.
        let failed = code != 0 && !(code == 1 && argv[0] == "lint");
        text_result(&text, failed)
    }
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn roots_request() -> Value {
    json!({ "jsonrpc": "2.0", "id": ROOTS_REQUEST, "method": "roots/list" })
}

/// `codex-mcp-client` → `codex`, `gemini-cli-mcp-client` → `gemini`, …
fn agent_name(client: &str) -> String {
    let lower = client.to_lowercase();
    for known in ["codex", "gemini", "cursor", "claude", "opencode"] {
        if lower.contains(known) {
            return known.to_string();
        }
    }
    lower
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // file://localhost/path or file:///path
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(hex) = std::str::from_utf8(&b[i + 1..i + 3])
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn s<'a>(args: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    args.get(k)
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

fn need<'a>(args: &'a Map<String, Value>, k: &str) -> Result<&'a str> {
    s(args, k).with_context(|| format!("`{k}` is required"))
}

fn strings(args: &Map<String, Value>, k: &str) -> Vec<String> {
    match args.get(k) {
        Some(Value::String(v)) if !v.is_empty() => vec![v.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn num(args: &Map<String, Value>, k: &str) -> Option<String> {
    match args.get(k)? {
        Value::Number(n) => Some(n.to_string()),
        Value::String(v) if !v.is_empty() => Some(v.clone()),
        _ => None,
    }
}

/// The reg-wiki arguments for a tool call, and what goes on stdin.
fn command_for(
    name: &str,
    a: &Map<String, Value>,
) -> Result<Option<(Vec<String>, Option<String>)>> {
    let mut v: Vec<String> = Vec::new();
    let mut stdin = None;
    let push_tags = |v: &mut Vec<String>, flag: &str, k: &str| {
        for t in strings(a, k) {
            v.push(flag.into());
            v.push(t);
        }
    };
    match name {
        "wiki_context" => {
            v.extend(["context".into(), "--".into(), need(a, "question")?.into()]);
            if let Some(b) = num(a, "budget") {
                v.splice(1..1, ["--budget".to_string(), b]);
            }
        }
        "wiki_query" => {
            v.push("query".into());
            if let Some(l) = num(a, "limit") {
                v.extend(["--limit".into(), l]);
            }
            push_tags(&mut v, "--tag", "tag");
            v.extend(["--".into(), need(a, "search")?.into()]);
        }
        "wiki_list" => {
            v.push("list".into());
            push_tags(&mut v, "--tag", "tag");
            if a.get("refs").and_then(Value::as_bool) == Some(true) {
                v.push("--refs".into());
            }
        }
        "wiki_read" => v.extend(["read".into(), "--".into(), need(a, "slug")?.into()]),
        "wiki_links" => {
            let which = if a.get("direction").and_then(Value::as_str) == Some("backlinks") {
                "backlinks"
            } else {
                "links"
            };
            v.extend([which.into(), "--".into(), need(a, "slug")?.into()]);
        }
        "wiki_write" => {
            v.extend(["write".into(), "-m".into(), need(a, "message")?.into()]);
            if let Some(b) = s(a, "base") {
                v.extend(["--base".into(), b.into()]);
            }
            v.extend(["--".into(), need(a, "slug")?.into()]);
            stdin = Some(need(a, "content")?.to_string());
        }
        "wiki_new" => {
            v.extend([
                "new".into(),
                "--title".into(),
                need(a, "title")?.into(),
                "--summary".into(),
                need(a, "summary")?.into(),
            ]);
            if let Some(p) = s(a, "parent") {
                v.extend(["--parent".into(), p.into()]);
            }
            if let Some(m) = s(a, "message") {
                v.extend(["-m".into(), m.into()]);
            }
            push_tags(&mut v, "--tag", "tags");
            push_tags(&mut v, "--source", "sources");
            v.extend(["--".into(), need(a, "slug")?.into()]);
        }
        "wiki_lint" => v.push("lint".into()),
        "wiki_ask" => v.extend([
            "ai".into(),
            "query".into(),
            "--".into(),
            need(a, "question")?.into(),
        ]),
        "wiki_record" => {
            v.extend(["ai".into(), "record".into()]);
            if a.get("dry_run").and_then(Value::as_bool) == Some(true) {
                v.push("--dry-run".into());
            }
            stdin = Some(need(a, "notes")?.to_string());
        }
        _ => return Ok(None),
    }
    Ok(Some((v, stdin)))
}

fn tools() -> Value {
    let scope = json!({
        "project": { "type": "string", "description": "Use this project instead of the one detected from the workspace." },
        "all": { "type": "boolean", "description": "Cover every project, not just the current one." }
    });
    let tool = |name: &str, desc: &str, props: Value, required: &[&str], read_only: bool| {
        let mut p = props.as_object().cloned().unwrap_or_default();
        p.extend(scope.as_object().cloned().unwrap_or_default());
        json!({
            "name": name,
            "description": desc,
            "inputSchema": { "type": "object", "properties": p, "required": required },
            "annotations": { "readOnlyHint": read_only, "destructiveHint": false },
        })
    };
    let slug = json!({ "type": "string", "description": "Page slug, e.g. myapp/billing, or billing within the current project." });
    json!([
        tool(
            "wiki_context",
            "What the wiki knows about a question: the best-matching pages' bodies plus summaries of the pages they link to, cut to a token budget. The cheapest way to find what earlier sessions recorded when you need it.",
            json!({
                "question": { "type": "string" },
                "budget": { "type": "string", "description": "Token budget, e.g. 4k or 1500." }
            }),
            &["question"],
            true
        ),
        tool(
            "wiki_query",
            "Full-text search. One `slug — summary` line per match, best first, with a snippet.",
            json!({
                "search": { "type": "string" },
                "limit": { "type": "integer" },
                "tag": { "type": "array", "items": { "type": "string" }, "description": "Only pages with all these tags." }
            }),
            &["search"],
            true
        ),
        tool(
            "wiki_list",
            "Every page in scope, one `slug — summary` line each.",
            json!({
                "tag": { "type": "array", "items": { "type": "string" } },
                "refs": { "type": "boolean", "description": "Include refs (supporting pages)." }
            }),
            &[],
            true
        ),
        tool(
            "wiki_read",
            "One page in full, then a footer with its hash (pass it as base to wiki_write), refs, links and backlinks, each with a summary.",
            json!({ "slug": slug }),
            &["slug"],
            true
        ),
        tool(
            "wiki_links",
            "Pages a page links to, or (direction: backlinks) pages linking to it.",
            json!({
                "slug": slug,
                "direction": { "type": "string", "enum": ["links", "backlinks"] }
            }),
            &["slug"],
            true
        ),
        tool(
            "wiki_write",
            "Replace a page with `content` (Markdown), validate it, set `updated` and commit it with `message` as the reason. Content without frontmatter keeps the page's frontmatter. A new slug needs frontmatter with at least title and summary (at most 200 characters, saying when to read the page). With `base`, the write is refused if the page changed since you read it, and the current content is returned to merge into.",
            json!({
                "slug": slug,
                "content": { "type": "string", "description": "The whole page." },
                "message": { "type": "string", "description": "Why the page changed; the commit message people review." },
                "base": { "type": "string", "description": "Hash from wiki_read." }
            }),
            &["slug", "content", "message"],
            false
        ),
        tool(
            "wiki_new",
            "Create a draft page (a stub with frontmatter) and commit it; warns about similar pages. Fill it in with wiki_write. With parent, the page is a ref (supporting doc) of that page.",
            json!({
                "slug": slug,
                "title": { "type": "string" },
                "summary": { "type": "string", "description": "When to read the page, at most 200 characters." },
                "parent": { "type": "string" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "sources": { "type": "array", "items": { "type": "string" }, "description": "Globs of the code the page describes, relative to the project root, e.g. src/billing/**." },
                "message": { "type": "string" }
            }),
            &["slug", "title", "summary"],
            false
        ),
        tool(
            "wiki_lint",
            "Broken or ambiguous links, orphans, weak summaries, long and stale pages.",
            json!({}),
            &[],
            true
        ),
        tool(
            "wiki_ask",
            "Ask the librarian (if one is set up): it reads the matching pages and answers in a few lines with its sources. Falls back to search results.",
            json!({ "question": { "type": "string" } }),
            &["question"],
            true
        ),
        tool(
            "wiki_record",
            "Give the librarian rough notes; it turns them into page edits, which are checked and committed. Needs a librarian.",
            json!({
                "notes": { "type": "string" },
                "dry_run": { "type": "boolean" }
            }),
            &["notes"],
            false
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_names() {
        assert_eq!(agent_name("codex-mcp-client"), "codex");
        assert_eq!(agent_name("gemini-cli-mcp-client"), "gemini");
        assert_eq!(agent_name("Cursor"), "cursor");
        assert_eq!(agent_name("mytool"), "mytool");
    }

    #[test]
    fn file_uris() {
        assert_eq!(
            file_uri_path("file:///home/me/my%20app"),
            Some(PathBuf::from("/home/me/my app"))
        );
        assert_eq!(
            file_uri_path("file://localhost/x"),
            Some(PathBuf::from("/x"))
        );
        assert_eq!(file_uri_path("https://x"), None);
    }

    #[test]
    fn tool_arguments() {
        let a = json!({ "slug": "billing", "content": "x", "message": "why", "base": "abcd" });
        let (argv, stdin) = command_for("wiki_write", a.as_object().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            argv,
            ["write", "-m", "why", "--base", "abcd", "--", "billing"]
        );
        assert_eq!(stdin.as_deref(), Some("x"));
        let a = json!({ "question": "-rf how", "budget": 2000 });
        let (argv, _) = command_for("wiki_context", a.as_object().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(argv, ["context", "--budget", "2000", "--", "-rf how"]);
        assert!(command_for("wiki_read", &Map::new()).is_err());
        assert!(command_for("nope", &Map::new()).unwrap().is_none());
    }
}
