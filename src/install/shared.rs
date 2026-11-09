//! Pieces every target uses: reading and writing the agent's files, the marker-fenced
//! section in its instructions file, hook groups in a JSON settings file, and how the
//! agent should call reg-wiki.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

use crate::home::Home;

pub const START: &str = "<!-- reg-wiki:start -->";
pub const END: &str = "<!-- reg-wiki:end -->";

/// Name of the MCP server entry in each agent's config.
pub const MCP_NAME: &str = "reg-wiki";
pub const MCP_ARGS: &[&str] = &["serve", "--mcp"];

/// The section for agents that reach the wiki through the MCP server.
pub const MCP_SECTION: &str = "\
<!-- reg-wiki:start -->
## Wiki (reg-wiki)

A wiki of linked Markdown pages is available through the `reg-wiki` MCP server (tools `wiki_context`, `wiki_query`, `wiki_read`, `wiki_write`, `wiki_new`, …). Before working on a part of the code, check what the wiki knows: `wiki_context` with a question, or `wiki_read` for a page. When you learn something a later session would want (a decision and its reason, a gotcha, how a part fits together), record it before you finish: `wiki_read` the closest page and `wiki_write` the whole page with `base` set to its hash and a `message` saying why, or `wiki_write` a new slug with full frontmatter (title, a summary that says when to read the page, `sources:` globs for the code it describes). Search before adding a page: updating beats a duplicate.
<!-- reg-wiki:end -->
";

pub fn read_opt(p: &Path) -> Result<Option<String>> {
    match fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

pub fn home_dir() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}

/// `$VAR` if set and not empty, else `~/<dir>`.
pub fn config_dir(var: &str, dir: &str) -> Result<PathBuf> {
    match std::env::var_os(var) {
        Some(d) if !d.is_empty() => Ok(PathBuf::from(d)),
        _ => Ok(home_dir()?.join(dir)),
    }
}

pub fn parse_json(p: &Path, text: Option<&str>, target: &str) -> Result<Value> {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        return Ok(Value::Object(Map::new()));
    };
    let v: Value = serde_json::from_str(text).with_context(|| {
        format!(
            "{} isn't valid JSON; fix it (or use --print-config {target} and edit by hand)",
            p.display()
        )
    })?;
    if !v.is_object() {
        bail!("{} should contain a JSON object", p.display());
    }
    Ok(v)
}

pub fn render_json(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

/// How hooks should call reg-wiki: by name if it's on PATH, else by absolute path.
pub fn hook_exe() -> (String, Option<String>) {
    if on_path("reg-wiki") {
        return ("reg-wiki".into(), None);
    }
    let exe = abs_exe();
    let quoted = if exe.contains(|c: char| !(c.is_alphanumeric() || "/._-+".contains(c))) {
        format!("'{}'", exe.replace('\'', r"'\''"))
    } else {
        exe.clone()
    };
    let note = format!(
        "reg-wiki isn't on PATH, so the hooks call {exe}. The agent runs `reg-wiki …` \
         commands by name, so put it on PATH (e.g. `cargo install --path .`) and run \
         install again."
    );
    (quoted, Some(note))
}

/// The absolute path of this binary. MCP servers are started by path, without a
/// shell: an editor started from the desktop may not have `~/.local/bin` on PATH.
pub fn abs_exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "reg-wiki".into())
}

fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| {
            let f = d.join(name);
            f.is_file()
                && fs::metadata(&f).is_ok_and(|m| {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o111 != 0
                })
        })
    })
}

// ---- the fenced section ----

/// Put `section` in `text`, replacing an existing one.
pub fn upsert_section(text: &str, section: &str) -> String {
    if let Some((a, b)) = section_range(text) {
        let mut s = text[..a].to_string();
        s.push_str(section);
        s.push_str(&text[b..]);
        return s;
    }
    let mut s = text.to_string();
    if !s.is_empty() {
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str(section);
    s
}

pub fn remove_section(text: &str) -> String {
    let Some((a, b)) = section_range(text) else {
        return text.to_string();
    };
    let mut before = text[..a].to_string();
    let after = &text[b..];
    // Undo the blank line install put in front of the section.
    if after.is_empty() && before.ends_with("\n\n") {
        before.pop();
    }
    before + after
}

/// Byte range of the section, including the newline after the end marker.
fn section_range(text: &str) -> Option<(usize, usize)> {
    let a = text.find(START)?;
    let e = a + text[a..].find(END)?;
    let mut b = e + END.len();
    if text[b..].starts_with('\n') {
        b += 1;
    }
    Some((a, b))
}

// ---- hooks in a JSON settings file (Claude Code and Gemini CLI share the shape) ----

/// One hook: the agent's event name, an optional matcher, the `reg-wiki hook` argument.
#[derive(Clone, Copy)]
pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub arg: &'static str,
}

/// Is this hook command one of ours (`reg-wiki hook …`, possibly by absolute path)?
pub fn is_ours(cmd: &str) -> bool {
    let c = cmd.trim();
    let Some(i) = c.find(" hook ") else {
        return false;
    };
    let exe = c[..i].trim().trim_matches(['\'', '"']);
    exe == "reg-wiki" || exe.ends_with("/reg-wiki")
}

/// Add a group per hook to `settings.hooks.<event>`. `timeout` is in the agent's unit.
pub fn add_hooks(settings: &mut Value, specs: &[HookSpec], exe: &str, timeout: u64) -> Result<()> {
    let obj = settings
        .as_object_mut()
        .context("settings must be an object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("`hooks` in the settings should be an object");
    };
    for h in specs {
        let mut g = Map::new();
        if let Some(m) = h.matcher {
            g.insert("matcher".into(), json!(m));
        }
        g.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": format!("{exe} hook {}", h.arg), "timeout": timeout }]),
        );
        let groups = hooks.entry(h.event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("`hooks.{}` in the settings should be an array", h.event);
        };
        groups.push(Value::Object(g));
    }
    Ok(())
}

/// Remove our hook handlers, and the groups, events and `hooks` key left empty by that.
/// Returns the events we had a hook on.
pub fn remove_hooks(settings: &mut Value) -> Vec<String> {
    let mut found = Vec::new();
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return found;
    };
    let mut emptied_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
        // Gemini CLI keeps settings such as `enabled` next to the events.
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        let before = groups.len();
        groups.retain_mut(|g| {
            let Some(handlers) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let n = handlers.len();
            handlers.retain(|h| {
                let ours = h
                    .get("command")
                    .and_then(Value::as_str)
                    .is_some_and(is_ours);
                if ours && !found.contains(event) {
                    found.push(event.clone());
                }
                !ours
            });
            // Drop only groups we emptied.
            !(handlers.is_empty() && n > 0)
        });
        if groups.is_empty() && before > 0 {
            emptied_events.push(event.clone());
        }
    }
    for e in emptied_events {
        hooks.remove(&e);
    }
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    found
}

// ---- `mcpServers` in a JSON config (Cursor, Gemini CLI) ----

pub fn set_mcp_server(cfg: &mut Value, entry: Value) -> Result<()> {
    let obj = cfg.as_object_mut().context("config must be an object")?;
    let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else {
        bail!("`mcpServers` should be an object");
    };
    servers.insert(MCP_NAME.into(), entry);
    Ok(())
}

pub fn remove_mcp_server(cfg: &mut Value) {
    let Some(servers) = cfg.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return;
    };
    if servers.remove(MCP_NAME).is_some() && servers.is_empty() {
        cfg.as_object_mut().map(|o| o.remove("mcpServers"));
    }
}

// ---- what install did, for targets without a directory of their own ----

/// Kept in `$REG_HOME/installs/<target>.json`.
pub fn manifest_path(target: &str) -> Result<PathBuf> {
    let home = Home::locate()?;
    let root = home
        .config
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    Ok(root.join("installs").join(format!("{target}.json")))
}

pub fn load_manifest<T: DeserializeOwned + Default>(p: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(p).ok()?).ok()
}

pub fn manifest_text<T: Serialize>(m: &T) -> Result<String> {
    Ok(serde_json::to_string_pretty(m)? + "\n")
}

/// Files a target's install created, so uninstall can delete them once they're empty.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Clone)]
#[serde(default)]
pub struct Created {
    pub files: Vec<PathBuf>,
    /// Gemini CLI: install set `trust` on the server.
    pub trusted: bool,
}

impl Created {
    pub fn note(&mut self, p: &Path, existed: bool) {
        if !existed && !self.files.iter().any(|f| f == p) {
            self.files.push(p.to_path_buf());
        }
    }

    pub fn has(&self, p: &Path) -> bool {
        self.files.iter().any(|f| f == p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_our_hooks() {
        assert!(is_ours("reg-wiki hook session-start"));
        assert!(is_ours("/home/me/.cargo/bin/reg-wiki hook stop"));
        assert!(is_ours("'/opt/my tools/reg-wiki' hook stop"));
        assert!(!is_ours("other-tool hook stop"));
        assert!(!is_ours("reg-wiki list"));
    }

    #[test]
    fn section_round_trips() {
        for orig in ["", "# Mine\n", "# Mine\n\nmore\n"] {
            let with = upsert_section(orig, MCP_SECTION);
            assert!(with.contains(START));
            assert_eq!(upsert_section(&with, MCP_SECTION), with, "idempotent");
            assert_eq!(remove_section(&with), orig);
        }
        // A section in the middle is replaced in place.
        let mid = format!("a\n\n{START}\nold\n{END}\n\nb\n");
        let new = upsert_section(&mid, MCP_SECTION);
        assert!(new.starts_with("a\n\n<!-- reg-wiki:start -->"));
        assert!(new.ends_with("<!-- reg-wiki:end -->\n\nb\n"));
        assert_eq!(remove_section(&new), "a\n\n\nb\n");
    }

    #[test]
    fn hooks_merge_and_unmerge() {
        let orig = json!({
            "model": "x",
            "hooks": {
                "enabled": true,
                "PostToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "mine" } ] } ]
            }
        });
        let mut s = orig.clone();
        let specs = [
            HookSpec {
                event: "PostToolUse",
                matcher: Some("Edit"),
                arg: "post-tool-use",
            },
            HookSpec {
                event: "Stop",
                matcher: None,
                arg: "stop",
            },
        ];
        add_hooks(&mut s, &specs, "reg-wiki", 10).unwrap();
        assert_eq!(s["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(remove_hooks(&mut s), ["PostToolUse", "Stop"]);
        assert_eq!(s, orig);
    }

    #[test]
    fn mcp_entries() {
        let mut c = json!({ "mcpServers": { "other": {} } });
        set_mcp_server(&mut c, json!({ "command": "x" })).unwrap();
        assert_eq!(c["mcpServers"]["reg-wiki"]["command"], "x");
        remove_mcp_server(&mut c);
        assert_eq!(c, json!({ "mcpServers": { "other": {} } }));
        let mut c = json!({});
        set_mcp_server(&mut c, json!({})).unwrap();
        remove_mcp_server(&mut c);
        assert_eq!(c, json!({}));
    }
}
