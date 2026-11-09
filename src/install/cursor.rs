//! Cursor: the MCP server in `~/.cursor/mcp.json`. Cursor has no global instructions
//! file to put a section in; the server's own instructions and tool descriptions tell
//! the agent how to use the wiki.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};

use super::shared::{self, Created, MCP_ARGS, MCP_NAME, abs_exe, read_opt};
use super::{Change, Plan, tilde};

struct Paths {
    mcp: PathBuf,
    manifest: PathBuf,
}

fn paths() -> Result<Paths> {
    Ok(Paths {
        mcp: shared::home_dir()?.join(".cursor").join("mcp.json"),
        manifest: shared::manifest_path("cursor")?,
    })
}

fn set_server(cfg: &mut Value, exe: &str) -> Result<()> {
    let mut entry = cfg
        .pointer(&format!("/mcpServers/{MCP_NAME}"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    entry.insert("command".into(), json!(exe));
    entry.insert("args".into(), json!(MCP_ARGS));
    shared::set_mcp_server(cfg, Value::Object(entry))
}

pub fn plan_install() -> Result<Plan> {
    let p = paths()?;
    let mut made: Created = shared::load_manifest(&p.manifest).unwrap_or_default();
    let mut changes = Vec::new();

    let text = read_opt(&p.mcp)?;
    made.note(&p.mcp, text.is_some());
    let before = shared::parse_json(&p.mcp, text.as_deref(), "cursor")?;
    let mut cfg = before.clone();
    set_server(&mut cfg, &abs_exe())?;
    if cfg != before {
        changes.push(Change {
            path: p.mcp.clone(),
            content: Some(shared::render_json(&cfg)),
            what: "MCP server `reg-wiki` (reg-wiki serve --mcp)".into(),
        });
    }
    if !changes.is_empty() || shared::load_manifest::<Created>(&p.manifest) != Some(made.clone()) {
        changes.push(Change {
            path: p.manifest.clone(),
            content: Some(shared::manifest_text(&made)?),
            what: "what install added to Cursor, for uninstall".into(),
        });
    }
    let mut notes = Vec::new();
    if changes.len() > 1 {
        notes.push(
            "Cursor picks up the server after a restart (or in Settings → MCP); it has no \
             global rules file, so the server's instructions tell the agent how to use the wiki"
                .into(),
        );
    }
    Ok(Plan {
        changes,
        prune_dirs: Vec::new(),
        notes,
        hook_command: None,
    })
}

pub fn plan_uninstall() -> Result<Plan> {
    let p = paths()?;
    let made: Option<Created> = shared::load_manifest(&p.manifest);
    let mut changes = Vec::new();
    if let Some(text) = read_opt(&p.mcp)? {
        let before = shared::parse_json(&p.mcp, Some(&text), "cursor")?;
        let mut cfg = before.clone();
        shared::remove_mcp_server(&mut cfg);
        if cfg != before {
            let empty = cfg.as_object().is_some_and(|o| o.is_empty());
            let delete = empty && made.as_ref().is_some_and(|m| m.has(&p.mcp));
            changes.push(Change {
                path: p.mcp.clone(),
                content: (!delete).then(|| shared::render_json(&cfg)),
                what: "remove the reg-wiki MCP server".into(),
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

pub fn print_config(json: bool) -> Result<()> {
    let p = paths()?;
    let mut cfg = json!({});
    set_server(&mut cfg, &abs_exe())?;
    if json {
        crate::cmd::print_json(&json!({ "mcp": { "path": p.mcp, "merge": cfg } }));
        return Ok(());
    }
    println!(
        "# reg-wiki for Cursor, set up by hand\n\n## {} (merge into the existing file)\n\n{}",
        tilde(&p.mcp),
        shared::render_json(&cfg)
    );
    Ok(())
}
