//! Codex CLI: the MCP server in `~/.codex/config.toml` (`[mcp_servers.reg-wiki]`) and a
//! marker-fenced section in `~/.codex/AGENTS.md`. `$CODEX_HOME` replaces `~/.codex`.
//!
//! Codex reaches the wiki through MCP rather than the shell: its sandbox lets commands
//! write only inside the workspace, and every `reg-wiki` command updates the index in
//! `~/.reg`. MCP servers run outside the sandbox.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use super::shared::{
    self, Created, MCP_ARGS, MCP_NAME, MCP_SECTION, abs_exe, read_opt, remove_section,
    upsert_section,
};
use super::{Change, Plan, tilde};

/// Codex gives an MCP tool call 60 seconds by default; `ai` calls can take longer.
const TOOL_TIMEOUT_SECS: i64 = 120;

struct Paths {
    config: PathBuf,
    agents_md: PathBuf,
    manifest: PathBuf,
}

fn paths() -> Result<Paths> {
    let dir = shared::config_dir("CODEX_HOME", ".codex")?;
    Ok(Paths {
        config: dir.join("config.toml"),
        agents_md: dir.join("AGENTS.md"),
        manifest: shared::manifest_path("codex")?,
    })
}

fn parse(p: &Path, text: &str) -> Result<DocumentMut> {
    text.parse::<DocumentMut>().with_context(|| {
        format!(
            "{} isn't valid TOML; fix it (or use --print-config codex and edit by hand)",
            p.display()
        )
    })
}

fn set_server(doc: &mut DocumentMut, exe: &str) -> Result<()> {
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        })
        .as_table_mut()
        .context("`mcp_servers` in config.toml should be a table")?;
    let entry = servers
        .entry(MCP_NAME)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .context("`mcp_servers.reg-wiki` in config.toml should be a table")?;
    let want_args = Array::from_iter(MCP_ARGS.iter().copied());
    if entry.get("command").and_then(Item::as_str) != Some(exe) {
        entry["command"] = value(exe);
    }
    let args_now: Option<Vec<&str>> = entry
        .get("args")
        .and_then(Item::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect());
    if args_now.as_deref() != Some(MCP_ARGS) {
        entry["args"] = value(want_args);
    }
    if entry.get("tool_timeout_sec").is_none() {
        entry["tool_timeout_sec"] = value(TOOL_TIMEOUT_SECS);
    }
    Ok(())
}

fn remove_server(doc: &mut DocumentMut) {
    let Some(servers) = doc.get_mut("mcp_servers").and_then(Item::as_table_mut) else {
        return;
    };
    if servers.remove(MCP_NAME).is_some() && servers.is_empty() {
        doc.remove("mcp_servers");
    }
}

pub fn plan_install() -> Result<Plan> {
    let p = paths()?;
    let old: Created = shared::load_manifest(&p.manifest).unwrap_or_default();
    let mut made = old.clone();
    let mut changes = Vec::new();

    let text = read_opt(&p.config)?;
    made.note(&p.config, text.is_some());
    let mut doc = parse(&p.config, text.as_deref().unwrap_or(""))?;
    set_server(&mut doc, &abs_exe())?;
    let new = doc.to_string();
    if text.as_deref() != Some(new.as_str()) {
        changes.push(Change {
            path: p.config.clone(),
            content: Some(new),
            what: "MCP server `reg-wiki` (reg-wiki serve --mcp)".into(),
        });
    }

    let md = read_opt(&p.agents_md)?;
    made.note(&p.agents_md, md.is_some());
    let new_md = upsert_section(md.as_deref().unwrap_or(""), MCP_SECTION);
    if md.as_deref() != Some(new_md.as_str()) {
        changes.push(Change {
            path: p.agents_md.clone(),
            content: Some(new_md),
            what: "reg-wiki section (between reg-wiki:start/end markers)".into(),
        });
    }

    if !changes.is_empty() || shared::load_manifest::<Created>(&p.manifest) != Some(made.clone()) {
        changes.push(Change {
            path: p.manifest.clone(),
            content: Some(shared::manifest_text(&made)?),
            what: "what install added to Codex, for uninstall".into(),
        });
    }
    let mut notes = Vec::new();
    if changes.len() > 1 {
        notes.push(
            "new Codex sessions get the wiki's MCP tools; Codex has no hooks for reg-wiki, \
             so the AGENTS.md section asks the agent to check the wiki"
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
    let created = |f: &Path| made.as_ref().is_some_and(|m| m.has(f));
    let mut changes = Vec::new();

    if let Some(text) = read_opt(&p.config)? {
        let mut doc = parse(&p.config, &text)?;
        remove_server(&mut doc);
        let new = doc.to_string();
        if new != text {
            let delete = new.trim().is_empty() && created(&p.config);
            changes.push(Change {
                path: p.config.clone(),
                content: (!delete).then_some(new),
                what: "remove the reg-wiki MCP server".into(),
            });
        }
    }
    if let Some(md) = read_opt(&p.agents_md)? {
        let new_md = remove_section(&md);
        if new_md != md {
            let delete = new_md.trim().is_empty() && created(&p.agents_md);
            changes.push(Change {
                path: p.agents_md.clone(),
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

pub fn print_config(json: bool) -> Result<()> {
    let p = paths()?;
    let mut doc = DocumentMut::new();
    set_server(&mut doc, &abs_exe())?;
    if json {
        crate::cmd::print_json(&serde_json::json!({
            "config": { "path": p.config, "merge": doc.to_string() },
            "agents_md": { "path": p.agents_md, "section": MCP_SECTION },
        }));
        return Ok(());
    }
    println!(
        "# reg-wiki for Codex, set up by hand\n\n## {} (add)\n\n{doc}",
        tilde(&p.config)
    );
    println!("## {} (append)\n\n{MCP_SECTION}", tilde(&p.agents_md));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_round_trips() {
        let orig = "model = \"o4\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
        let mut doc: DocumentMut = orig.parse().unwrap();
        set_server(&mut doc, "/bin/reg-wiki").unwrap();
        let with = doc.to_string();
        assert!(
            with.contains("[mcp_servers.reg-wiki]\ncommand = \"/bin/reg-wiki\""),
            "{with}"
        );
        let mut again: DocumentMut = with.parse().unwrap();
        set_server(&mut again, "/bin/reg-wiki").unwrap();
        assert_eq!(again.to_string(), with, "idempotent");
        remove_server(&mut again);
        assert_eq!(again.to_string(), orig);

        let mut doc = DocumentMut::new();
        set_server(&mut doc, "r").unwrap();
        remove_server(&mut doc);
        assert_eq!(doc.to_string(), "");
    }
}
