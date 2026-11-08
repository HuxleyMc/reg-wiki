//! Stop (opt-in): if the session edited code in a registered project and wrote nothing to
//! the wiki, ask the agent once to record what it learned.

use anyhow::Result;
use rusqlite::OptionalExtension;

use super::{Input, Output, state};
use crate::cmd::Ctx;

pub fn run(ctx: &Ctx, input: &Input) -> Result<Option<Output>> {
    // Already continuing because of a Stop hook: let it stop.
    if input.stop_hook_active {
        return Ok(None);
    }
    let Some(id) = input.session_id.as_deref() else {
        return Ok(None);
    };
    let s = state::load(&ctx.home, id);
    if s.nudged || s.wrote || s.edited.is_empty() {
        return Ok(None);
    }
    // Without REG_SESSION in the agent's shell, writes can't be tied to this session;
    // any write since it started will do (another session's, at worst).
    if !s.tagged && wrote_since(ctx, s.started.as_deref())? {
        return Ok(None);
    }
    state::update(&ctx.home, id, |s| s.nudged = true)?;

    let n = s.edited.len();
    let mut files: Vec<String> = s
        .edited
        .iter()
        .take(5)
        .map(|f| {
            let p = std::path::Path::new(f);
            crate::project::git_root(p)
                .and_then(|r| crate::util::rel_str(&r, p))
                .unwrap_or_else(|| f.clone())
        })
        .collect();
    if n > files.len() {
        files.push(format!("{} more", n - files.len()));
    }
    let ai = if crate::librarian::Settings::load(&ctx.cfg).enabled {
        " Or send rough notes to `reg-wiki ai record` (on stdin) and the librarian will turn \
         them into page edits."
    } else {
        ""
    };
    Ok(Some(Output::Block(format!(
        "reg-wiki: this session changed {n} file(s) ({}) but recorded nothing in the wiki. \
         If you learned something a later session would want (a decision and its reason, a \
         gotcha, how a part of the code fits together), record it now: update the closest \
         page with `reg-wiki read <slug>` then `reg-wiki write <slug> --base <hash> -m \"why\"`, \
         or add one with `reg-wiki new`. Search first with `reg-wiki query`.{ai} If nothing \
         is worth keeping, just finish; this won't be asked again in this session.",
        files.join(", ")
    ))))
}

fn wrote_since(ctx: &Ctx, started: Option<&str>) -> Result<bool> {
    let Some(ts) = started else {
        return Ok(false);
    };
    Ok(ctx
        .conn
        .query_row(
            "SELECT 1 FROM events WHERE kind = 'write' AND ts >= ?1 LIMIT 1",
            [ts],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
