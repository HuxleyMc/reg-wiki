//! Claude Code: the skill in `~/.claude/skills/reg-wiki/`, hooks and a permission in
//! `~/.claude/settings.json`, and a marker-fenced section in `~/.claude/CLAUDE.md`.
//! `$CLAUDE_CONFIG_DIR` replaces `~/.claude`, as it does for Claude Code itself.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Change, Plan, SKILL_FILES, tilde};
use crate::InstallArgs;

pub const PERMISSION: &str = "Bash(reg-wiki:*)";
/// The same rule in Claude Code's newer spelling; either counts as already allowed.
const PERMISSION_ALT: &str = "Bash(reg-wiki *)";

const START: &str = "<!-- reg-wiki:start -->";
const END: &str = "<!-- reg-wiki:end -->";

const CLAUDE_MD_SECTION: &str = "\
<!-- reg-wiki:start -->
## Wiki (reg-wiki)

A wiki of linked Markdown pages is available through the `reg-wiki` CLI; the current project's pages are listed at session start. Before working on a part of the code, check what the wiki knows: `reg-wiki context \"<question>\"` or `reg-wiki read <slug>`. When you learn something a later session would want (a decision and its reason, a gotcha, how a part fits together), update the closest page with `reg-wiki write` or add one with `reg-wiki new`. The reg-wiki skill has the details.
<!-- reg-wiki:end -->
";

/// What install did, so uninstall removes exactly that. Kept in the skill directory.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct Manifest {
    /// Install added the permission (it wasn't already there).
    permission_added: bool,
    /// settings.json / CLAUDE.md didn't exist before install.
    settings_created: bool,
    claude_md_created: bool,
    /// `~/.claude/skills` didn't exist before install.
    skills_dir_created: bool,
    stop_hook: bool,
}

const MANIFEST: &str = ".install.json";

struct Paths {
    skill: PathBuf,
    settings: PathBuf,
    claude_md: PathBuf,
}

fn skills_dir(p: &Paths) -> PathBuf {
    p.skill.parent().map(Path::to_path_buf).unwrap_or_default()
}

fn paths() -> Result<Paths> {
    let dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".claude"),
    };
    Ok(Paths {
        skill: dir.join("skills").join("reg-wiki"),
        settings: dir.join("settings.json"),
        claude_md: dir.join("CLAUDE.md"),
    })
}

/// (event, matcher, `reg-wiki hook` argument)
const HOOKS: &[(&str, Option<&str>, &str)] = &[
    // Not on resume or fork: the context already has the index.
    (
        "SessionStart",
        Some("startup|clear|compact"),
        "session-start",
    ),
    ("UserPromptSubmit", None, "user-prompt-submit"),
    (
        "PostToolUse",
        Some("Edit|Write|MultiEdit|NotebookEdit"),
        "post-tool-use",
    ),
    ("Stop", None, "stop"),
];

/// Seconds Claude Code waits for a hook. They take milliseconds; this only bounds a hang.
const HOOK_TIMEOUT: u64 = 10;

fn hook_groups(exe: &str, stop: bool) -> Vec<(&'static str, Value)> {
    HOOKS
        .iter()
        .filter(|(ev, _, _)| stop || *ev != "Stop")
        .map(|(ev, matcher, arg)| {
            let mut g = Map::new();
            if let Some(m) = matcher {
                g.insert("matcher".into(), json!(m));
            }
            g.insert(
                "hooks".into(),
                json!([{ "type": "command", "command": format!("{exe} hook {arg}"), "timeout": HOOK_TIMEOUT }]),
            );
            (*ev, Value::Object(g))
        })
        .collect()
}

/// Is this hook command one of ours (`reg-wiki hook …`, possibly by absolute path)?
fn is_ours(cmd: &str) -> bool {
    let c = cmd.trim();
    let Some(i) = c.find(" hook ") else {
        return false;
    };
    let exe = c[..i].trim().trim_matches(['\'', '"']);
    exe == "reg-wiki" || exe.ends_with("/reg-wiki")
}

/// How hooks should call reg-wiki: by name if it's on PATH, else by absolute path.
fn hook_exe() -> (String, Option<String>) {
    if on_path("reg-wiki") {
        return ("reg-wiki".into(), None);
    }
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "reg-wiki".into());
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

fn read_opt(p: &Path) -> Result<Option<String>> {
    match fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn parse_settings(p: &Path, text: Option<&str>) -> Result<Value> {
    let Some(text) = text.filter(|t| !t.trim().is_empty()) else {
        return Ok(Value::Object(Map::new()));
    };
    let v: Value = serde_json::from_str(text).with_context(|| {
        format!(
            "{} isn't valid JSON; fix it (or use --print-config claude and edit by hand)",
            p.display()
        )
    })?;
    if !v.is_object() {
        bail!("{} should contain a JSON object", p.display());
    }
    Ok(v)
}

fn render_settings(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    s.push('\n');
    s
}

fn load_manifest(p: &Path) -> Option<Manifest> {
    let text = fs::read_to_string(p.join(MANIFEST)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove our hook handlers, and the groups, events and `hooks` key left empty by that.
/// Returns whether a Stop hook of ours was there.
fn remove_hooks(settings: &mut Value) -> bool {
    let mut had_stop = false;
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else {
        return false;
    };
    let mut emptied_events = Vec::new();
    for (event, groups) in hooks.iter_mut() {
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
                if ours && event == "Stop" {
                    had_stop = true;
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
    had_stop
}

fn add_hooks(settings: &mut Value, exe: &str, stop: bool) -> Result<()> {
    let obj = settings
        .as_object_mut()
        .context("settings must be an object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        bail!("`hooks` in settings.json should be an object");
    };
    for (event, group) in hook_groups(exe, stop) {
        let groups = hooks.entry(event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            bail!("`hooks.{event}` in settings.json should be an array");
        };
        groups.push(group);
    }
    Ok(())
}

fn allow_list(settings: &mut Value) -> Result<&mut Vec<Value>> {
    let obj = settings
        .as_object_mut()
        .context("settings must be an object")?;
    let perms = obj.entry("permissions").or_insert_with(|| json!({}));
    let Some(perms) = perms.as_object_mut() else {
        bail!("`permissions` in settings.json should be an object");
    };
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    allow
        .as_array_mut()
        .context("`permissions.allow` in settings.json should be an array")
}

fn has_permission(settings: &Value) -> bool {
    settings
        .pointer("/permissions/allow")
        .and_then(Value::as_array)
        .is_some_and(|a| {
            a.iter().any(|v| {
                v.as_str()
                    .is_some_and(|s| s == PERMISSION || s == PERMISSION_ALT)
            })
        })
}

/// Remove our permission, and `allow` / `permissions` if that leaves them empty.
fn remove_permission(settings: &mut Value) {
    let Some(perms) = settings
        .get_mut("permissions")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(allow) = perms.get_mut("allow").and_then(Value::as_array_mut) else {
        return;
    };
    let before = allow.len();
    allow.retain(|v| v.as_str() != Some(PERMISSION));
    if allow.len() == before {
        return;
    }
    if allow.is_empty() {
        perms.remove("allow");
    }
    if perms.is_empty() {
        settings.as_object_mut().map(|o| o.remove("permissions"));
    }
}

/// Put the fenced section in `text`, replacing an existing one.
fn upsert_section(text: &str) -> String {
    if let Some((a, b)) = section_range(text) {
        let mut s = text[..a].to_string();
        s.push_str(CLAUDE_MD_SECTION);
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
    s.push_str(CLAUDE_MD_SECTION);
    s
}

fn remove_section(text: &str) -> String {
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

pub fn plan_install(a: &InstallArgs) -> Result<Plan> {
    let p = paths()?;
    let old = load_manifest(&p.skill).unwrap_or_default();
    let mut changes = Vec::new();
    let mut notes = Vec::new();

    for (rel, content) in SKILL_FILES {
        let path = p.skill.join(rel);
        if read_opt(&path)?.as_deref() != Some(*content) {
            changes.push(Change {
                path,
                content: Some(content.to_string()),
                what: "skill file".into(),
            });
        }
    }

    let (exe, exe_note) = hook_exe();
    notes.extend(exe_note);
    let settings_text = read_opt(&p.settings)?;
    let before = parse_settings(&p.settings, settings_text.as_deref())?;
    let mut settings = before.clone();
    let had_stop = remove_hooks(&mut settings);
    let stop = a.stop_hook || had_stop || old.stop_hook;
    add_hooks(&mut settings, &exe, stop)?;
    let mut permission_added = old.permission_added;
    if a.no_permissions {
        if old.permission_added {
            remove_permission(&mut settings);
        }
        permission_added = false;
    } else if !has_permission(&settings) {
        allow_list(&mut settings)?.push(json!(PERMISSION));
        permission_added = true;
    }
    if settings != before {
        let mut what = vec![format!(
            "hooks: SessionStart, UserPromptSubmit, PostToolUse{}",
            if stop { ", Stop" } else { "" }
        )];
        if has_permission(&settings) && !has_permission(&before) {
            what.push(format!("allow {PERMISSION}"));
        }
        if !has_permission(&settings) && has_permission(&before) {
            what.push(format!("remove {PERMISSION}"));
        }
        changes.push(Change {
            path: p.settings.clone(),
            content: Some(render_settings(&settings)),
            what: what.join("; "),
        });
    }

    let md = read_opt(&p.claude_md)?;
    let new_md = upsert_section(md.as_deref().unwrap_or(""));
    if md.as_deref() != Some(new_md.as_str()) {
        changes.push(Change {
            path: p.claude_md.clone(),
            content: Some(new_md),
            what: "reg-wiki section (between reg-wiki:start/end markers)".into(),
        });
    }

    let manifest = Manifest {
        permission_added,
        settings_created: old.settings_created || settings_text.is_none(),
        claude_md_created: old.claude_md_created || md.is_none(),
        skills_dir_created: old.skills_dir_created || !skills_dir(&p).exists(),
        stop_hook: stop,
    };
    // The manifest only matters alongside other changes, or if it's out of date.
    if !changes.is_empty() || load_manifest(&p.skill).as_ref() != Some(&manifest) {
        changes.push(Change {
            path: p.skill.join(MANIFEST),
            content: Some(serde_json::to_string_pretty(&manifest)? + "\n"),
            what: "what install added, for uninstall".into(),
        });
    }

    if !changes.is_empty() {
        notes.push(
            "new Claude Code sessions pick this up; in each repo you want a wiki for, \
             run `reg-wiki project add`"
                .into(),
        );
    }
    Ok(Plan {
        changes,
        prune_dirs: Vec::new(),
        notes,
        hook_command: Some(exe),
    })
}

pub fn plan_uninstall() -> Result<Plan> {
    let p = paths()?;
    let manifest = load_manifest(&p.skill);
    let m = manifest.as_ref();
    let mut changes = Vec::new();
    let mut notes = Vec::new();

    if let Some(text) = read_opt(&p.settings)? {
        let before = parse_settings(&p.settings, Some(&text))?;
        let mut settings = before.clone();
        remove_hooks(&mut settings);
        match m {
            Some(m) if m.permission_added => remove_permission(&mut settings),
            Some(_) => {}
            None if has_permission(&settings) => notes.push(format!(
                "left {PERMISSION} in {}: without install's record it may not be ours",
                tilde(&p.settings)
            )),
            None => {}
        }
        if settings != before {
            let empty = settings.as_object().is_some_and(Map::is_empty);
            let delete = empty && m.is_some_and(|m| m.settings_created);
            changes.push(Change {
                path: p.settings.clone(),
                content: (!delete).then(|| render_settings(&settings)),
                what: "remove reg-wiki hooks and permission".into(),
            });
        }
    }

    if let Some(md) = read_opt(&p.claude_md)? {
        let new_md = remove_section(&md);
        if new_md != md {
            let delete = new_md.trim().is_empty() && m.is_some_and(|m| m.claude_md_created);
            changes.push(Change {
                path: p.claude_md.clone(),
                content: (!delete).then_some(new_md),
                what: "remove the reg-wiki section".into(),
            });
        }
    }

    let skill_files = SKILL_FILES
        .iter()
        .map(|(rel, _)| *rel)
        .chain(std::iter::once(MANIFEST));
    for rel in skill_files {
        let path = p.skill.join(rel);
        if path.exists() {
            changes.push(Change {
                path,
                content: None,
                what: "delete".into(),
            });
        }
    }
    let mut prune_dirs = vec![p.skill.join("references"), p.skill.clone()];
    if m.is_some_and(|m| m.skills_dir_created) {
        prune_dirs.push(skills_dir(&p));
    }
    Ok(Plan {
        changes,
        prune_dirs,
        notes,
        hook_command: None,
    })
}

pub fn print_config(json: bool, a: &InstallArgs) -> Result<()> {
    let p = paths()?;
    let (exe, _) = hook_exe();
    let mut settings = json!({});
    add_hooks(&mut settings, &exe, a.stop_hook)?;
    if !a.no_permissions {
        allow_list(&mut settings)?.push(json!(PERMISSION));
    }
    if json {
        let files: Map<String, Value> = SKILL_FILES
            .iter()
            .map(|(rel, c)| (p.skill.join(rel).display().to_string(), json!(c)))
            .collect();
        crate::cmd::print_json(&json!({
            "settings": { "path": p.settings, "merge": settings },
            "claude_md": { "path": p.claude_md, "section": CLAUDE_MD_SECTION },
            "skill_files": files,
        }));
        return Ok(());
    }
    println!(
        "# reg-wiki for Claude Code, set up by hand\n\n\
         ## {} (merge into the existing settings)\n\n{}",
        tilde(&p.settings),
        render_settings(&settings)
    );
    println!("## {} (append)\n\n{CLAUDE_MD_SECTION}", tilde(&p.claude_md));
    for (rel, content) in SKILL_FILES {
        println!("## {}\n\n{content}", tilde(&p.skill.join(rel)));
    }
    Ok(())
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
            let with = upsert_section(orig);
            assert!(with.contains(START));
            assert_eq!(upsert_section(&with), with, "idempotent");
            assert_eq!(remove_section(&with), orig);
        }
        // A section in the middle is replaced in place.
        let mid = format!("a\n\n{START}\nold\n{END}\n\nb\n");
        let new = upsert_section(&mid);
        assert!(new.starts_with("a\n\n<!-- reg-wiki:start -->"));
        assert!(new.ends_with("<!-- reg-wiki:end -->\n\nb\n"));
        assert_eq!(remove_section(&new), "a\n\n\nb\n");
    }

    #[test]
    fn hooks_merge_and_unmerge() {
        let orig = json!({
            "model": "x",
            "hooks": { "PostToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "mine" } ] } ] }
        });
        let mut s = orig.clone();
        add_hooks(&mut s, "reg-wiki", true).unwrap();
        assert_eq!(s["hooks"]["PostToolUse"].as_array().unwrap().len(), 2);
        assert!(remove_hooks(&mut s));
        assert_eq!(s, orig);
    }
}
