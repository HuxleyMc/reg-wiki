//! The librarian: an optional agent, reached over ACP, that reads wiki pages on behalf of
//! the main session and answers or proposes edits. reg-wiki does every file read, every
//! write and all the ranking; the librarian only receives text and returns text.

pub mod acp;
pub mod prompts;
pub mod reply;

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::Serialize;

use crate::home::Config;

pub use acp::{Session, Spawn};

/// Used when `[librarian] model` isn't set: the smallest, cheapest Claude model.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
pub const DEFAULT_AGENT: &str = "claude";
const DEFAULT_BUDGET: usize = 12_000;
const DEFAULT_CANDIDATES: usize = 8;
const DEFAULT_TIMEOUT: u64 = 90;

/// Agents reg-wiki knows how to start without an `[agents.<name>]` section.
pub const BUILTIN_AGENTS: &[(&str, &str, &[&str])] = &[(
    "claude",
    "npx",
    &["-y", "@agentclientprotocol/claude-agent-acp"],
)];

/// `[librarian]` and the `[agents.<name>]` it points at.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub enabled: bool,
    pub agent: String,
    pub model: String,
    pub budget_tokens: usize,
    pub candidates: usize,
    pub timeout_secs: u64,
    #[serde(skip)]
    pub spawn: Option<Spawn>,
}

impl Settings {
    /// Off unless config.toml has a `[librarian]` section (`reg-wiki install` writes
    /// one); `enabled` defaults to true inside it.
    pub fn load(cfg: &Config) -> Settings {
        let has_section = cfg.get(&["librarian"]).is_some();
        let enabled = has_section && cfg.bool_at(&["librarian", "enabled"]).unwrap_or(true);
        let agent = cfg
            .str_at(&["librarian", "agent"])
            .unwrap_or_else(|| DEFAULT_AGENT.to_string());
        let model = cfg
            .str_at(&["librarian", "model"])
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let n = |k: &str, d: usize| cfg.int("librarian", k).map_or(d, |v| v.max(1) as usize);
        Settings {
            enabled,
            spawn: spawn_for(cfg, &agent),
            agent,
            model,
            budget_tokens: n("budget_tokens", DEFAULT_BUDGET),
            candidates: n("candidates", DEFAULT_CANDIDATES),
            timeout_secs: n("timeout_secs", DEFAULT_TIMEOUT as usize) as u64,
        }
    }

    /// Start the agent and switch to the configured model. Errors explain why the
    /// librarian can't be used, for the one-line fallback note.
    pub fn connect(&self) -> Result<Librarian> {
        if !self.enabled {
            bail!("the librarian is off (turn it on with `reg-wiki install`)");
        }
        let Some(spawn) = &self.spawn else {
            bail!(
                "no command for agent `{}`; add an [agents.{}] section to config.toml",
                self.agent,
                self.agent
            );
        };
        let deadline = Instant::now() + Duration::from_secs(self.timeout_secs);
        let mut session = Session::start(spawn, deadline)?;
        let mut note = None;
        if let Err(e) = session.set_model(&self.model) {
            // Keep going with the agent's own default: an answer beats no answer.
            note = Some(format!("{e:#}; using the agent's default model"));
        }
        Ok(Librarian { session, note })
    }
}

/// How to start `agent`: `[agents.<agent>]`, else a built-in default.
pub fn spawn_for(cfg: &Config, agent: &str) -> Option<Spawn> {
    let env: Vec<(String, String)> = cfg
        .get(&["agents", agent, "env"])
        .and_then(|i| i.as_table_like())
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| Some((k.to_string(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if let Some(command) = cfg.str_at(&["agents", agent, "command"]) {
        return Some(Spawn {
            command,
            args: cfg.strs_at(&["agents", agent, "args"]).unwrap_or_default(),
            env,
        });
    }
    BUILTIN_AGENTS
        .iter()
        .find(|(name, _, _)| *name == agent)
        .map(|(_, command, args)| Spawn {
            command: command.to_string(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env,
        })
}

pub struct Librarian {
    pub session: Session,
    /// Something worth telling the user, e.g. that the model wasn't offered.
    pub note: Option<String>,
}

impl Librarian {
    /// The model in use, for messages and the commit author.
    pub fn model(&self) -> String {
        self.session
            .model
            .clone()
            .unwrap_or_else(|| "default model".to_string())
    }

    pub fn ask(&mut self, prompt: &str) -> Result<String> {
        self.session.prompt(prompt)
    }
}
