use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use serde_json::json;

use super::{Ctx, print_json, warn};
use crate::{util, view};

pub fn run(ctx: &Ctx, out: Option<PathBuf>, no_open: bool) -> Result<ExitCode> {
    let scope = ctx.scope();
    let graph = view::graph(&ctx.conn, &ctx.home.wiki, scope)?;
    let html = view::document(&graph)?;

    if out.as_deref().is_some_and(|p| p.as_os_str() == "-") {
        print!("{html}");
        return Ok(ExitCode::SUCCESS);
    }
    let path = match out {
        Some(p) if p.is_absolute() => p,
        Some(p) => ctx.cwd.join(p),
        None => ctx.home.db.with_file_name("view.html"),
    };
    util::atomic_write(&path, html.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;

    let opened = !no_open && open(&path);
    if ctx.json {
        print_json(&json!({
            "path": path, "scope": scope, "pages": graph.nodes.len(),
            "links": graph.edges.len(), "opened": opened,
        }));
    } else {
        println!(
            "{} · {} pages, {} links",
            path.display(),
            graph.nodes.len(),
            graph.edges.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Open `path` in the default browser without waiting for it.
fn open(path: &std::path::Path) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        Command::new("xdg-open")
    };
    let ok = cmd
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok();
    if !ok {
        warn("could not open a browser; open the file above yourself");
    }
    ok
}
