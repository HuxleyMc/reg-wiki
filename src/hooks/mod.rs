//! `reg-wiki hook <event>`: Claude Code hooks. Each reads the hook's JSON on stdin and
//! prints JSON that adds context for the agent, or nothing.
//!
//! A hook must never get in the agent's way: it always exits 0 (exit code 2 would block
//! the prompt or the stop), prints errors to stderr only, and does nothing at all inside
//! the librarian (`REG_LIBRARIAN=1`).

mod post_tool_use;
mod prompt;
mod session_start;
pub mod state;
mod stop;

use std::io::{IsTerminal, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::Cli;
use crate::cmd::Ctx;

/// Set in the agent's shell by the SessionStart hook (through `CLAUDE_ENV_FILE`), so
/// that wiki writes can be tied to the session.
pub const SESSION_ENV: &str = "REG_SESSION";

/// The fields of Claude Code's hook input that the hooks use. Everything is optional:
/// a field that's missing just means that hook has less to go on.
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub session_id: Option<String>,
    pub cwd: Option<PathBuf>,
    /// UserPromptSubmit.
    pub prompt: Option<String>,
    /// PostToolUse.
    pub tool_name: Option<String>,
    pub tool_input: Option<Value>,
    /// Stop: true when the agent is already continuing because of a Stop hook.
    #[serde(default)]
    pub stop_hook_active: bool,
}

pub enum Output {
    /// Text added to the agent's context.
    Context(String),
    /// Stop only: keep the agent going, with this reason as its next instruction.
    Block(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Event {
    SessionStart,
    UserPromptSubmit,
    PostToolUse,
    Stop,
}

impl Event {
    /// Accepts `session-start`, `SessionStart`, `session_start`, …
    fn parse(s: &str) -> Option<Event> {
        let norm: String = s
            .chars()
            .filter(|c| c.is_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        Some(match norm.as_str() {
            "sessionstart" => Event::SessionStart,
            "userpromptsubmit" | "prompt" => Event::UserPromptSubmit,
            "posttooluse" => Event::PostToolUse,
            "stop" => Event::Stop,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Event::SessionStart => "SessionStart",
            Event::UserPromptSubmit => "UserPromptSubmit",
            Event::PostToolUse => "PostToolUse",
            Event::Stop => "Stop",
        }
    }
}

/// Claude Code allows 10,000 characters of hook output before it moves the rest to a file.
const MAX_OUTPUT: usize = 9_000;

pub fn run(cli: &Cli, event: &str) -> ExitCode {
    if std::env::var("REG_LIBRARIAN").is_ok_and(|v| v == "1") {
        return ExitCode::SUCCESS;
    }
    let Some(ev) = Event::parse(event) else {
        eprintln!(
            "reg-wiki: unknown hook event `{event}` (expected session-start, \
             user-prompt-submit, post-tool-use or stop)"
        );
        return ExitCode::SUCCESS;
    };
    match catch_unwind(AssertUnwindSafe(|| handle(cli, ev))) {
        Ok(Ok(Some(out))) => println!("{}", render(ev, out)),
        Ok(Ok(None)) => {}
        Ok(Err(e)) => eprintln!("reg-wiki: {} hook: {e:#}", ev.name()),
        Err(_) => eprintln!("reg-wiki: {} hook panicked", ev.name()),
    }
    ExitCode::SUCCESS
}

fn render(ev: Event, out: Output) -> String {
    let v = match out {
        Output::Context(text) => json!({
            "hookSpecificOutput": {
                "hookEventName": ev.name(),
                "additionalContext": clip(&text, MAX_OUTPUT),
            }
        }),
        Output::Block(reason) => {
            json!({ "decision": "block", "reason": clip(&reason, MAX_OUTPUT) })
        }
    };
    v.to_string()
}

fn clip(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((i, _)) => {
            let head = &text[..i];
            let head = head.rfind('\n').map_or(head, |j| &head[..j]);
            format!("{head}\n…")
        }
    }
}

fn handle(cli: &Cli, ev: Event) -> Result<Option<Output>> {
    let input = read_input()?;
    let cwd = match &input.cwd {
        Some(d) if d.is_dir() => d.clone(),
        _ => std::env::current_dir()?,
    };
    let ctx = Ctx::open(cli, cwd)?;
    match ev {
        Event::SessionStart => session_start::run(&ctx, &input),
        Event::UserPromptSubmit => prompt::run(&ctx, &input),
        Event::PostToolUse => post_tool_use::run(&ctx, &input),
        Event::Stop => stop::run(&ctx, &input),
    }
}

fn read_input() -> Result<Input> {
    let stdin = std::io::stdin();
    // Run by hand in a terminal: don't wait for input that isn't coming.
    if stdin.is_terminal() {
        return Ok(Input::default());
    }
    let mut text = String::new();
    stdin
        .lock()
        .read_to_string(&mut text)
        .context("reading hook input")?;
    if text.trim().is_empty() {
        return Ok(Input::default());
    }
    serde_json::from_str(&text).context("parsing hook input")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_names() {
        assert_eq!(Event::parse("session-start"), Some(Event::SessionStart));
        assert_eq!(Event::parse("SessionStart"), Some(Event::SessionStart));
        assert_eq!(
            Event::parse("user_prompt_submit"),
            Some(Event::UserPromptSubmit)
        );
        assert_eq!(Event::parse("PostToolUse"), Some(Event::PostToolUse));
        assert_eq!(Event::parse("stop"), Some(Event::Stop));
        assert_eq!(Event::parse("PreToolUse"), None);
    }

    #[test]
    fn clip_cuts_at_a_line() {
        assert_eq!(clip("ab\ncd\nef", 7), "ab\ncd\n…");
        assert_eq!(clip("short", 10), "short");
    }
}
