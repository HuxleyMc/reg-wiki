//! A small, synchronous Agent Client Protocol client: JSON-RPC 2.0, one message per
//! line, over the agent's stdin and stdout.
//!
//! reg-wiki only sends text and reads text back, so it advertises no `fs` or `terminal`
//! support, answers every `session/request_permission` with a rejection and every other
//! request from the agent with "method not found".

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Value, json};

/// ACP protocol version spoken here.
const PROTOCOL_VERSION: u64 = 1;

/// How long an agent gets to stop after `session/cancel` before it is killed.
const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// What to start: a command line and extra environment.
#[derive(Debug, Clone)]
pub struct Spawn {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A model the agent offers.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// How the agent lets a client choose the model.
#[derive(Debug, Clone)]
enum ModelPicker {
    /// `configOptions` entry with category `model`; set with `session/set_config_option`.
    ConfigOption { id: String },
    /// The older `models` field; set with `session/set_model`.
    SetModel,
}

#[derive(Debug, Clone, Default)]
pub struct Models {
    pub current: Option<String>,
    pub available: Vec<Model>,
    picker: Option<ModelPicker>,
}

enum Incoming {
    Message(Value),
    /// A line that isn't JSON (agents sometimes log to stdout).
    Junk,
    Closed,
}

pub struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Incoming>,
    next_id: u64,
    deadline: Instant,
    pub id: String,
    pub models: Models,
    /// The model in use, once known.
    pub model: Option<String>,
    /// Text from `agent_message_chunk` updates for the prompt in flight.
    reply: String,
    /// Empty working directory for the session, removed on drop.
    _cwd: tempfile::TempDir,
}

impl Session {
    /// Start the agent, initialize, and open a session. Everything must finish by `deadline`.
    pub fn start(spawn: &Spawn, deadline: Instant) -> Result<Session> {
        let cwd = tempfile::Builder::new()
            .prefix("reg-librarian-")
            .tempdir()
            .context("creating the librarian's working directory")?;
        let mut cmd = Command::new(&spawn.command);
        cmd.args(&spawn.args)
            .current_dir(cwd.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_target())
            // The recursion guard: reg-wiki hooks do nothing inside the librarian.
            .env("REG_LIBRARIAN", "1")
            // A Claude Code agent started from inside a Claude Code session would think it
            // is nested in it.
            .env_remove("CLAUDECODE")
            .env_remove(crate::hooks::SESSION_ENV);
        for (k, v) in &spawn.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting the librarian agent `{}`", spawn.command))?;
        let stdin = child.stdin.take().context("agent stdin")?;
        let stdout = child.stdout.take().context("agent stdout")?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let msg = match serde_json::from_str::<Value>(line) {
                    Ok(v) if v.is_object() => Incoming::Message(v),
                    _ => Incoming::Junk,
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
            let _ = tx.send(Incoming::Closed);
        });

        let mut s = Session {
            child,
            stdin,
            rx,
            next_id: 0,
            deadline,
            id: String::new(),
            models: Models::default(),
            model: None,
            reply: String::new(),
            _cwd: cwd,
        };
        let cwd = s._cwd.path().to_string_lossy().into_owned();
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                },
                "clientInfo": { "name": "reg-wiki", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        if let Some(v) = init.get("protocolVersion").and_then(Value::as_u64)
            && v != PROTOCOL_VERSION
        {
            bail!("the agent speaks ACP version {v}; reg-wiki speaks {PROTOCOL_VERSION}");
        }
        let new = s.request("session/new", json!({ "cwd": cwd, "mcpServers": [] }))?;
        s.id = new
            .get("sessionId")
            .and_then(Value::as_str)
            .context("session/new returned no sessionId")?
            .to_string();
        s.models = parse_models(&new);
        s.model = s.models.current.clone();
        Ok(s)
    }

    /// Switch to `want`, matched against the offered models by id, then by name.
    /// Returns the id chosen, or an error naming what is on offer.
    pub fn set_model(&mut self, want: &str) -> Result<String> {
        let Some(m) = find_model(&self.models.available, want) else {
            let offered: Vec<&str> = self
                .models
                .available
                .iter()
                .map(|m| m.id.as_str())
                .collect();
            if offered.is_empty() {
                bail!("the agent doesn't let clients choose a model");
            }
            bail!(
                "the agent doesn't offer model `{want}` (it offers: {})",
                offered.join(", ")
            );
        };
        let id = m.id.clone();
        if self.model.as_deref() == Some(id.as_str()) {
            return Ok(id);
        }
        match self.models.picker.clone() {
            Some(ModelPicker::ConfigOption { id: config_id }) => {
                let r = self.request(
                    "session/set_config_option",
                    json!({ "sessionId": self.id, "configId": config_id, "value": id }),
                )?;
                if r.get("configOptions").is_some() {
                    let current = parse_models(&r).current;
                    if current.is_some() {
                        self.models.current = current;
                    }
                }
            }
            Some(ModelPicker::SetModel) => {
                self.request(
                    "session/set_model",
                    json!({ "sessionId": self.id, "modelId": id }),
                )?;
            }
            None => bail!("the agent doesn't let clients choose a model"),
        }
        self.model = Some(id.clone());
        Ok(id)
    }

    /// Send one prompt and collect the agent's reply text until the turn ends.
    pub fn prompt(&mut self, text: &str) -> Result<String> {
        self.reply.clear();
        let id = self.send_request(
            "session/prompt",
            json!({ "sessionId": self.id, "prompt": [{ "type": "text", "text": text }] }),
        )?;
        let r = match self.wait(id, "session/prompt") {
            Ok(r) => r,
            Err(e) => {
                if Instant::now() >= self.deadline {
                    // Ask the agent to stop, and give it a moment to wind down cleanly
                    // before the process is killed.
                    self.cancel();
                    self.deadline = Instant::now() + CANCEL_GRACE;
                    let _ = self.wait(id, "session/prompt");
                }
                return Err(e);
            }
        };
        let stop = r.get("stopReason").and_then(Value::as_str).unwrap_or("");
        if stop != "end_turn" {
            bail!("the agent stopped with `{stop}` instead of finishing its reply");
        }
        Ok(std::mem::take(&mut self.reply))
    }

    fn cancel(&mut self) {
        let _ = self.send(&json!({
            "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": self.id },
        }));
    }

    fn send(&mut self, v: &Value) -> Result<()> {
        let mut line = serde_json::to_string(v)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|_| self.stdin.flush())
            .context("writing to the agent (did it exit?)")
    }

    /// Send a request and wait for its response.
    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.send_request(method, params)?;
        self.wait(id, method)
    }

    fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        Ok(id)
    }

    /// Wait for the response to request `id`, handling whatever arrives meanwhile.
    fn wait(&mut self, id: u64, method: &str) -> Result<Value> {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            let msg = match self.rx.recv_timeout(left) {
                Ok(Incoming::Message(m)) => m,
                Ok(Incoming::Junk) => continue,
                Ok(Incoming::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.try_wait().ok().flatten();
                    match status {
                        Some(s) => bail!("the agent exited ({s}) during `{method}`"),
                        None => bail!("the agent closed its output during `{method}`"),
                    }
                }
                Err(RecvTimeoutError::Timeout) => bail!("timed out waiting for `{method}`"),
            };
            let has_method = msg.get("method").and_then(Value::as_str).is_some();
            match (msg.get("id"), has_method) {
                // A response.
                (Some(rid), false) => {
                    if rid.as_u64() != Some(id) {
                        continue;
                    }
                    if let Some(err) = msg.get("error") {
                        let text = err.get("message").and_then(Value::as_str).unwrap_or("");
                        bail!("the agent refused `{method}`: {text}");
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
                // A request from the agent.
                (Some(rid), true) => {
                    let rid = rid.clone();
                    self.answer(rid, &msg)?;
                }
                // A notification.
                (None, true) => self.notification(&msg),
                (None, false) => {}
            }
        }
    }

    fn answer(&mut self, id: Value, msg: &Value) -> Result<()> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let reply = if method == "session/request_permission" {
            // Reject whatever the agent wants to do: it only needs to read and write text.
            let options = msg
                .pointer("/params/options")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let reject = ["reject_once", "reject_always"].iter().find_map(|kind| {
                options.iter().find_map(|o| {
                    (o.get("kind").and_then(Value::as_str) == Some(kind))
                        .then(|| o.get("optionId").cloned())
                        .flatten()
                })
            });
            let outcome = match reject {
                Some(opt) => json!({ "outcome": "selected", "optionId": opt }),
                None => json!({ "outcome": "cancelled" }),
            };
            json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } })
        } else {
            json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32601, "message": format!("reg-wiki does not support `{method}`") },
            })
        };
        self.send(&reply)
    }

    fn notification(&mut self, msg: &Value) {
        if msg.get("method").and_then(Value::as_str) != Some("session/update") {
            return;
        }
        let Some(update) = msg.pointer("/params/update") else {
            return;
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if let Some(t) = update.pointer("/content/text").and_then(Value::as_str) {
                    self.reply.push_str(t);
                }
            }
            Some("config_option_update") => {
                let m = parse_models(update);
                if m.current.is_some() {
                    self.model = m.current;
                }
            }
            Some("current_model_update") => {
                if let Some(id) = update.get("modelId").and_then(Value::as_str) {
                    self.model = Some(id.to_string());
                }
            }
            _ => {}
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The agent's stderr goes to `REG_LIBRARIAN_LOG` when set, else nowhere: an agent
/// that logs freely would otherwise clutter the main session's output.
fn stderr_target() -> Stdio {
    if let Some(p) = std::env::var_os("REG_LIBRARIAN_LOG")
        && let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
    {
        return Stdio::from(f);
    }
    Stdio::null()
}

/// Models from a `session/new` (or `set_config_option`) result or a config update.
fn parse_models(v: &Value) -> Models {
    if let Some(opts) = v.get("configOptions").and_then(Value::as_array) {
        for o in opts {
            if o.get("category").and_then(Value::as_str) != Some("model")
                || o.get("type").and_then(Value::as_str) != Some("select")
            {
                continue;
            }
            let mut available = Vec::new();
            for item in o
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                // Options are either a flat list or groups of them.
                match item.get("options").and_then(Value::as_array) {
                    Some(group) => available.extend(group.iter().filter_map(select_option)),
                    None => available.extend(select_option(item)),
                }
            }
            return Models {
                current: o
                    .get("currentValue")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                available,
                picker: o
                    .get("id")
                    .and_then(Value::as_str)
                    .map(|id| ModelPicker::ConfigOption { id: id.to_string() }),
            };
        }
    }
    if let Some(m) = v.get("models").filter(|m| m.is_object()) {
        let available = m
            .get("availableModels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|x| {
                let id = x.get("modelId")?.as_str()?.to_string();
                Some(Model {
                    name: x
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_string(),
                    description: x
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    id,
                })
            })
            .collect();
        return Models {
            current: m
                .get("currentModelId")
                .and_then(Value::as_str)
                .map(str::to_string),
            available,
            picker: Some(ModelPicker::SetModel),
        };
    }
    Models::default()
}

fn select_option(o: &Value) -> Option<Model> {
    let id = o.get("value")?.as_str()?.to_string();
    Some(Model {
        name: o
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string(),
        description: o
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        id,
    })
}

/// Exact id, then case-insensitive id or name, then a unique id or name containing it.
pub fn find_model<'a>(models: &'a [Model], want: &str) -> Option<&'a Model> {
    let w = want.trim().to_lowercase();
    if w.is_empty() {
        return None;
    }
    if let Some(m) = models.iter().find(|m| m.id == want.trim()) {
        return Some(m);
    }
    if let Some(m) = models
        .iter()
        .find(|m| m.id.to_lowercase() == w || m.name.to_lowercase() == w)
    {
        return Some(m);
    }
    let partial: Vec<&Model> = models
        .iter()
        .filter(|m| m.id.to_lowercase().contains(&w) || m.name.to_lowercase().contains(&w))
        .collect();
    match partial.as_slice() {
        [m] => Some(m),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_from_config_options() {
        let v = json!({ "configOptions": [
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select",
              "currentValue": "default", "options": [] },
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "default",
              "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "group": "small", "name": "Small", "options": [
                    { "value": "haiku", "name": "Haiku", "description": "fast" } ] } ] } ] });
        let m = parse_models(&v);
        assert_eq!(m.current.as_deref(), Some("default"));
        let ids: Vec<&str> = m.available.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["default", "haiku"]);
        assert!(matches!(m.picker, Some(ModelPicker::ConfigOption { ref id }) if id == "model"));
    }

    #[test]
    fn models_from_the_older_field() {
        let v = json!({ "models": { "currentModelId": "a",
            "availableModels": [ { "modelId": "a", "name": "A" }, { "modelId": "b", "name": "B" } ] } });
        let m = parse_models(&v);
        assert_eq!(m.available.len(), 2);
        assert!(matches!(m.picker, Some(ModelPicker::SetModel)));
    }

    #[test]
    fn finding_models() {
        let ms = vec![
            Model {
                id: "claude-haiku-5-5".into(),
                name: "Haiku".into(),
                description: None,
            },
            Model {
                id: "claude-sonnet-5-5".into(),
                name: "Sonnet".into(),
                description: None,
            },
            Model {
                id: "default".into(),
                name: "Default (recommended)".into(),
                description: None,
            },
        ];
        assert_eq!(
            find_model(&ms, "claude-haiku-5-5").unwrap().id,
            "claude-haiku-5-5"
        );
        assert_eq!(find_model(&ms, "haiku").unwrap().id, "claude-haiku-5-5");
        assert_eq!(find_model(&ms, "Sonnet").unwrap().id, "claude-sonnet-5-5");
        assert!(find_model(&ms, "claude").is_none(), "ambiguous");
        assert!(find_model(&ms, "gpt").is_none());
    }
}
