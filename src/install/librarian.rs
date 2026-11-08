//! The librarian part of `reg-wiki install`: `[librarian]` and `[agents.<name>]` in
//! reg-wiki's own config.toml. Asked interactively, it starts the agent to list the
//! models it really offers; with `--yes` it only writes settings and never starts it.

use std::io::{BufRead, Write};

use anyhow::Result;
use toml_edit::Array;

use super::Change;
use crate::InstallArgs;
use crate::home::{Config, Home};
use crate::librarian::{self, BUILTIN_AGENTS, DEFAULT_AGENT, DEFAULT_MODEL, Settings, acp};

pub fn plan(a: &InstallArgs, interactive: bool, notes: &mut Vec<String>) -> Result<Option<Change>> {
    let home = Home::locate()?;
    let mut cfg = Config::load(&home.config)?;
    let before = cfg.render();
    let had_section = cfg.get(&["librarian"]).is_some();

    if a.no_librarian {
        if had_section && cfg.bool_at(&["librarian", "enabled"]) != Some(false) {
            cfg.set_at(&["librarian", "enabled"], false)?;
        }
        return Ok(change(&home, &cfg, &before, "turn the librarian off"));
    }

    if interactive && !had_section && a.librarian_agent.is_none() && a.librarian_model.is_none() {
        let yes = ask(
            "Set up the librarian? It answers `reg-wiki ai query` from the wiki with a small \
             model, and turns notes into pages with `reg-wiki ai record`. [Y/n] ",
        )?;
        if !matches!(yes.to_lowercase().as_str(), "" | "y" | "yes") {
            notes.push("librarian not set up; `reg-wiki install` again to add it".into());
            return Ok(None);
        }
    }

    let agent = a
        .librarian_agent
        .clone()
        .or_else(|| cfg.str_at(&["librarian", "agent"]))
        .unwrap_or_else(|| DEFAULT_AGENT.to_string());
    if cfg.str_at(&["agents", &agent, "command"]).is_none() {
        match BUILTIN_AGENTS.iter().find(|(n, _, _)| *n == agent) {
            Some((_, command, args)) => {
                cfg.set_at(&["agents", &agent, "command"], *command)?;
                cfg.set_at(
                    &["agents", &agent, "args"],
                    Array::from_iter(args.iter().copied()),
                )?;
            }
            None => anyhow::bail!(
                "no agent `{agent}`: add `[agents.{agent}]` with `command` and `args` to {}, \
                 then run install again",
                home.config.display()
            ),
        }
    }
    if a.librarian_agent.is_some() || !had_section {
        cfg.set_at(&["librarian", "agent"], agent.as_str())?;
    }
    if cfg.bool_at(&["librarian", "enabled"]) != Some(true) {
        cfg.set_at(&["librarian", "enabled"], true)?;
    }

    let configured = cfg
        .str_at(&["librarian", "model"])
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let model = match &a.librarian_model {
        Some(m) => m.clone(),
        None if interactive => choose_model(&cfg, &agent, &configured)?,
        None => configured,
    };
    if cfg.str_at(&["librarian", "model"]).as_deref() != Some(model.as_str()) {
        cfg.set_at(&["librarian", "model"], model.as_str())?;
    }
    if !interactive && a.librarian_model.is_none() && !had_section {
        notes.push(format!(
            "librarian set to {model} on agent `{agent}`; `reg-wiki ai models` lists what the agent offers"
        ));
    }
    Ok(change(
        &home,
        &cfg,
        &before,
        &format!("librarian: agent `{agent}`, model {model}"),
    ))
}

fn change(home: &Home, cfg: &Config, before: &str, what: &str) -> Option<Change> {
    let after = cfg.render();
    (after != before).then(|| Change {
        path: home.config.clone(),
        content: Some(after),
        what: what.to_string(),
    })
}

/// Start the agent, list its models and let the person pick one. If the agent can't be
/// reached, keep `configured` and say so.
fn choose_model(cfg: &Config, agent: &str, configured: &str) -> Result<String> {
    let mut st = Settings::load(cfg);
    st.enabled = true;
    st.agent = agent.to_string();
    st.spawn = librarian::spawn_for(cfg, agent);
    eprintln!("starting `{agent}` to see which models it offers…");
    let lib = match st.connect() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("reg-wiki: couldn't list models ({e:#}); keeping {configured}");
            return Ok(configured.to_string());
        }
    };
    let models = lib.session.models.available.clone();
    drop(lib);
    if models.is_empty() {
        eprintln!("`{agent}` doesn't offer a choice of model; keeping {configured}");
        return Ok(configured.to_string());
    }
    let default = acp::find_model(&models, configured)
        .and_then(|m| models.iter().position(|x| x.id == m.id))
        .unwrap_or(0);
    eprintln!("Models (a small one is plenty: the librarian only reads and summarises):");
    for (i, m) in models.iter().enumerate() {
        let desc = m
            .description
            .as_deref()
            .map(|d| format!(" — {d}"))
            .unwrap_or_default();
        eprintln!("  {}. {} ({}){desc}", i + 1, m.id, m.name);
    }
    loop {
        let a = ask(&format!("Model [{}]: ", default + 1))?;
        if a.is_empty() {
            return Ok(models[default].id.clone());
        }
        if let Ok(n) = a.parse::<usize>()
            && (1..=models.len()).contains(&n)
        {
            return Ok(models[n - 1].id.clone());
        }
        if let Some(m) = acp::find_model(&models, &a) {
            return Ok(m.id.clone());
        }
        eprintln!("pick a number from 1 to {}", models.len());
    }
}

fn ask(q: &str) -> Result<String> {
    eprint!("{q}");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim().to_string())
}
