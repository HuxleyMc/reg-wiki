//! What the hooks remember about one agent session, in `$REG_HOME/sessions/<id>.json`.
//!
//! Every access is best effort: state that can't be read starts empty, and state that
//! can't be written is dropped. The worst outcome is a repeated or missing reminder.

use std::fs;
use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::home::Home;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// When the session started (or when a hook first saw it).
    pub started: Option<String>,
    /// `REG_SESSION` was exported into the session's shell, so wiki writes mark `wrote`.
    pub tagged: bool,
    /// Files edited in registered projects (absolute paths).
    pub edited: Vec<String>,
    /// Pages the PostToolUse hook has already asked the agent to update.
    pub reminded: Vec<String>,
    /// Pages the UserPromptSubmit hook has already suggested.
    pub suggested: Vec<String>,
    /// The session wrote to the wiki.
    pub wrote: bool,
    /// The agent was told it edited a wiki file by hand.
    pub warned_hand_edit: bool,
    /// The Stop hook has asked the agent to record what it learned.
    pub nudged: bool,
}

/// Sessions not touched for this long are deleted at the next session start.
const KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

fn path(home: &Home, id: &str) -> Option<PathBuf> {
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(128)
        .collect();
    (!safe.is_empty()).then(|| home.sessions.join(format!("{safe}.json")))
}

pub fn load(home: &Home, id: &str) -> Session {
    path(home, id)
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Read, change and write back a session's state under an exclusive lock, so hooks
/// running in parallel (several edits at once) don't lose each other's updates.
pub fn update<T>(home: &Home, id: &str, f: impl FnOnce(&mut Session) -> T) -> Result<T> {
    let Some(p) = path(home, id) else {
        return Ok(f(&mut Session::default()));
    };
    fs::create_dir_all(&home.sessions)?;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&p)?;
    file.lock()?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let mut s: Session = serde_json::from_str(&text).unwrap_or_default();
    let out = f(&mut s);
    let new = serde_json::to_string(&s)?;
    if new != text {
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(new.as_bytes())?;
    }
    Ok(out)
}

/// Called by commands that commit to the wiki when `REG_SESSION` is set.
pub fn mark_wrote(home: &Home, id: &str) {
    let _ = update(home, id, |s| s.wrote = true);
}

/// Delete state of sessions that ended long ago.
pub fn prune(home: &Home) {
    let Ok(entries) = fs::read_dir(&home.sessions) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > KEEP);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_sanitised() {
        let home = Home {
            wiki: "/w".into(),
            db: "/d".into(),
            config: "/c".into(),
            lock: "/l".into(),
            sessions: "/s".into(),
        };
        assert_eq!(
            path(&home, "../../etc/x").unwrap(),
            PathBuf::from("/s/etcx.json")
        );
        assert!(path(&home, "../").is_none());
    }
}
