//! UserPromptSubmit: `Relevant wiki pages: …` from a quick full-text lookup of the
//! prompt. Prints nothing when nothing matches well, and doesn't repeat a page it
//! already suggested in the same session.

use anyhow::Result;

use super::{Input, Output, state};
use crate::cmd::{Ctx, line};
use crate::search;

const SHOW: usize = 3;
/// Longer prompts are usually pasted logs or code; their head says what they're about.
const MAX_PROMPT: usize = 2000;

pub fn run(ctx: &Ctx, input: &Input) -> Result<Option<Output>> {
    let Some(prompt) = input.prompt.as_deref() else {
        return Ok(None);
    };
    let prompt = prompt.trim();
    // A bare slash command (`/clear`, `/compact`) is not a question about the code.
    if prompt.starts_with('/') && !prompt.contains(char::is_whitespace) {
        return Ok(None);
    }
    let head: String = prompt.chars().take(MAX_PROMPT).collect();
    let hits = search::relevant(&ctx.conn, &head, ctx.scope(), SHOW * 3)?;
    if hits.is_empty() {
        return Ok(None);
    }
    let fresh = match &input.session_id {
        Some(id) => state::update(&ctx.home, id, |s| {
            let fresh: Vec<_> = hits
                .iter()
                .filter(|p| !s.suggested.contains(&p.slug))
                .take(SHOW)
                .cloned()
                .collect();
            s.suggested.extend(fresh.iter().map(|p| p.slug.clone()));
            fresh
        })
        .unwrap_or_else(|_| hits.iter().take(SHOW).cloned().collect()),
        None => hits.into_iter().take(SHOW).collect(),
    };
    if fresh.is_empty() {
        return Ok(None);
    }
    let mut out = String::from("Relevant wiki pages (open with `reg-wiki read <slug>`):\n");
    for p in &fresh {
        out.push_str(&format!("- {}\n", line(p)));
    }
    Ok(Some(Output::Context(out)))
}
