//! Gemini CLI: in `~/.gemini/settings.json`, the MCP server (trusted, so its tools run
//! without a prompt, unless `--no-permissions`) and hooks: `SessionStart` for the
//! project's index, `BeforeAgent` for pages matching the prompt and `AfterTool` on file
//! edits for pages whose sources changed. Plus a marker-fenced section in
//! `~/.gemini/GEMINI.md`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::shared::{
    self, Created, HookSpec, MCP_ARGS, MCP_NAME, MCP_SECTION, abs_exe, hook_exe, read_opt,
    remove_section, upsert_section,
};
use super::{Change, Plan, tilde};
use crate::InstallArgs;

const HOOKS: &[HookSpec] = &[
    HookSpec {
        event: "SessionStart",
        matcher: None,
        arg: "session-start",
    },
    HookSpec {
        event: "BeforeAgent",
        matcher: None,
        arg: "before-agent",
    },
    HookSpec {
        event: "AfterTool",
        matcher: Some("write_file|replace"),
        arg: "after-tool",
    },
];

/// Gemini CLI hook timeouts are in milliseconds.
const HOOK_TIMEOUT_MS: u64 = 10_000;
/// MCP tool calls: `ai` calls can take a while.
const TOOL_TIMEOUT_MS: u64 = 120_000;

struct Paths {
    settings: PathBuf,
    gemini_md: PathBuf,
    manifest: PathBuf,
}

fn paths() -> Result<Paths> {
    let dir = shared::home_dir()?.join(".gemini");
    Ok(Paths {
        settings: dir.join("settings.json"),
        gemini_md: dir.join("GEMINI.md"),
        manifest: shared::manifest_path("gemini")?,
    })
}

/// Add or update our server entry, keeping anything else the person set on it.
fn set_server(settings: &mut Value, exe: &str, trust: Option<bool>) -> Result<()> {
    let mut entry = settings
        .pointer(&format!("/mcpServers/{MCP_NAME}"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    entry.insert("command".into(), json!(exe));
    entry.insert("args".into(), json!(MCP_ARGS));
    entry
        .entry("timeout")
        .or_insert_with(|| json!(TOOL_TIMEOUT_MS));
    match trust {
        Some(true) => {
            entry.insert("trust".into(), json!(true));
        }
        Some(false) => {
            entry.remove("trust");
        }
        None => {}
    }
    shared::set_mcp_server(settings, Value::Object(entry))
}

fn trusted(settings: &Value) -> bool {
    settings
        .pointer(&format!("/mcpServers/{MCP_NAME}/trust"))
        .and_then(Value::as_bool)
        == Some(true)
}

pub fn plan_install(a: &InstallArgs) -> Result<Plan> {
    let p = paths()?;
    let old: Created = shared::load_manifest(&p.manifest).unwrap_or_default();
    let mut made = old.clone();
    let mut changes = Vec::new();
    let mut notes = Vec::new();

    let (exe, exe_note) = hook_exe();
    notes.extend(exe_note);
    let text = read_opt(&p.settings)?;
    made.note(&p.settings, text.is_some());
    let before = shared::parse_json(&p.settings, text.as_deref(), "gemini")?;
    let mut settings = before.clone();
    shared::remove_hooks(&mut settings);
    shared::add_hooks(&mut settings, HOOKS, &exe, HOOK_TIMEOUT_MS)?;
    let was_trusted = trusted(&before);
    let trust = if a.no_permissions {
        // Take back only what install set.
        old.trusted.then_some(false)
    } else if !was_trusted {
        made.trusted = true;
        Some(true)
    } else {
        None
    };
    if a.no_permissions {
        made.trusted = false;
    }
    set_server(&mut settings, &abs_exe(), trust)?;
    if settings != before {
        let mut what = vec!["MCP server `reg-wiki`".to_string()];
        if trusted(&settings) && !was_trusted {
            what.push("trusted (its tools run without asking)".into());
        }
        what.push("hooks: SessionStart, BeforeAgent, AfterTool".into());
        changes.push(Change {
            path: p.settings.clone(),
            content: Some(shared::render_json(&settings)),
            what: what.join("; "),
        });
    }

    let md = read_opt(&p.gemini_md)?;
    made.note(&p.gemini_md, md.is_some());
    let new_md = upsert_section(md.as_deref().unwrap_or(""), MCP_SECTION);
    if md.as_deref() != Some(new_md.as_str()) {
        changes.push(Change {
            path: p.gemini_md.clone(),
            content: Some(new_md),
            what: "reg-wiki section (between reg-wiki:start/end markers)".into(),
        });
    }

    if !changes.is_empty() || shared::load_manifest::<Created>(&p.manifest) != Some(made.clone()) {
        changes.push(Change {
            path: p.manifest.clone(),
            content: Some(shared::manifest_text(&made)?),
            what: "what install added to Gemini CLI, for uninstall".into(),
        });
    }
    if a.stop_hook {
        notes.push("the Stop hook is for Claude Code only; Gemini CLI doesn't get one".into());
    }
    if changes.len() > 1 {
        notes.push("new Gemini CLI sessions pick this up".into());
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
    let made: Option<Created> = shared::load_manifest(&p.manifest);
    let created = |f: &Path| made.as_ref().is_some_and(|m| m.has(f));
    let mut changes = Vec::new();

    if let Some(text) = read_opt(&p.settings)? {
        let before = shared::parse_json(&p.settings, Some(&text), "gemini")?;
        let mut settings = before.clone();
        shared::remove_hooks(&mut settings);
        shared::remove_mcp_server(&mut settings);
        if settings != before {
            let empty = settings.as_object().is_some_and(|o| o.is_empty());
            let delete = empty && created(&p.settings);
            changes.push(Change {
                path: p.settings.clone(),
                content: (!delete).then(|| shared::render_json(&settings)),
                what: "remove the reg-wiki MCP server and hooks".into(),
            });
        }
    }
    if let Some(md) = read_opt(&p.gemini_md)? {
        let new_md = remove_section(&md);
        if new_md != md {
            let delete = new_md.trim().is_empty() && created(&p.gemini_md);
            changes.push(Change {
                path: p.gemini_md.clone(),
                content: (!delete).then_some(new_md),
                what: "remove the reg-wiki section".into(),
            });
        }
    }
    if p.manifest.exists() {
        changes.push(Change {
            path: p.manifest.clone(),
            content: None,
            what: "delete".into(),
        });
    }
    Ok(Plan {
        changes,
        prune_dirs: p
            .manifest
            .parent()
            .map(Path::to_path_buf)
            .into_iter()
            .collect(),
        notes: Vec::new(),
        hook_command: None,
    })
}

pub fn print_config(json: bool, a: &InstallArgs) -> Result<()> {
    let p = paths()?;
    let (exe, _) = hook_exe();
    let mut settings = json!({});
    set_server(&mut settings, &abs_exe(), Some(!a.no_permissions))?;
    shared::add_hooks(&mut settings, HOOKS, &exe, HOOK_TIMEOUT_MS)?;
    if json {
        crate::cmd::print_json(&json!({
            "settings": { "path": p.settings, "merge": settings },
            "gemini_md": { "path": p.gemini_md, "section": MCP_SECTION },
        }));
        return Ok(());
    }
    println!(
        "# reg-wiki for Gemini CLI, set up by hand\n\n## {} (merge into the existing settings)\n\n{}",
        tilde(&p.settings),
        shared::render_json(&settings)
    );
    println!("## {} (append)\n\n{MCP_SECTION}", tilde(&p.gemini_md));
    Ok(())
}
