//! `reg-wiki ai schedule`: run `ai tend` regularly from the user's crontab.
//!
//! The entry is two lines, a marker comment and the job, and nothing else in the
//! crontab is touched. The job runs `reg-wiki --all ai tend` with the PATH and REG_HOME
//! in effect when it was scheduled (cron's own PATH wouldn't find `npx`), and appends
//! its output to `tend.log` in REG_HOME. `$REG_CRONTAB` names another `crontab` binary.

use std::io::Write;
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{Ctx, print_json, warn};
use crate::librarian::Settings;

const MARKER: &str = "# reg-wiki: scheduled tend; change it with `reg-wiki ai schedule`";
const DEFAULT_AT: (u32, u32) = (3, 17);

pub fn run(ctx: &Ctx, when: Option<&str>, at: Option<&str>) -> Result<ExitCode> {
    let current = read_crontab()?;
    let (rest, ours) = split(&current);
    let Some(when) = when else {
        if at.is_some() {
            bail!("--at goes with daily or weekly");
        }
        return show(ctx, ours.as_deref());
    };

    let root = ctx
        .home
        .config
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let log = root.join("tend.log");
    let new_entry = match when {
        "off" => None,
        _ => {
            let (h, m) = match at {
                Some(t) => parse_at(t)?,
                None => DEFAULT_AT,
            };
            let dow = if when == "weekly" { "0" } else { "*" };
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok())
                .context("finding the reg-wiki executable")?;
            let path = std::env::var("PATH").unwrap_or_default();
            let job = format!(
                "{m} {h} * * {dow} PATH={} REG_HOME={} {} --all ai tend >> {} 2>&1",
                cron_quote(&path),
                cron_quote(&root.to_string_lossy()),
                cron_quote(&exe.to_string_lossy()),
                cron_quote(&log.to_string_lossy()),
            );
            Some(job)
        }
    };

    let mut text = rest;
    if let Some(job) = &new_entry {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{MARKER}\n{job}\n"));
    }
    let changed = text != current;
    if changed {
        write_crontab(&text)?;
    }
    if new_entry.is_some() && !Settings::load(&ctx.cfg).enabled {
        warn(
            "the librarian is off, so the scheduled tend will only report that; turn it on with `reg-wiki install`",
        );
    }

    if ctx.json {
        print_json(&json!({
            "schedule": when,
            "entry": new_entry,
            "log": new_entry.as_ref().map(|_| log.clone()),
            "changed": changed,
        }));
    } else {
        match &new_entry {
            Some(job) => {
                println!("scheduled `reg-wiki ai tend` {} (crontab):", describe(job));
                println!("  {job}");
                println!("output goes to {}", log.display());
            }
            None if ours.is_some() => println!("removed the scheduled tend from the crontab"),
            None => println!("no scheduled tend; nothing to change"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn show(ctx: &Ctx, ours: Option<&str>) -> Result<ExitCode> {
    if ctx.json {
        print_json(&json!({
            "schedule": ours.map(schedule_name),
            "entry": ours,
        }));
        return Ok(ExitCode::SUCCESS);
    }
    match ours {
        Some(job) => {
            println!("`reg-wiki ai tend` runs {} (crontab):", describe(job));
            println!("  {job}");
        }
        None => println!("no scheduled tend; `reg-wiki ai schedule daily` (or weekly) sets one up"),
    }
    Ok(ExitCode::SUCCESS)
}

fn schedule_name(job: &str) -> &'static str {
    match job.split_whitespace().nth(4) {
        Some("*") => "daily",
        _ => "weekly",
    }
}

/// "daily at 03:17" from the job's cron fields.
fn describe(job: &str) -> String {
    let f: Vec<&str> = job.split_whitespace().take(5).collect();
    let time = match (f.first(), f.get(1)) {
        (Some(m), Some(h)) => format!(
            "{:0>2}:{:0>2}",
            h.parse::<u32>().unwrap_or(0),
            m.parse::<u32>().unwrap_or(0)
        ),
        _ => "?".into(),
    };
    match schedule_name(job) {
        "daily" => format!("daily at {time}"),
        _ => format!("weekly, Sundays at {time}"),
    }
}

fn parse_at(t: &str) -> Result<(u32, u32)> {
    let bad = || anyhow::anyhow!("--at wants a time like 03:17, not `{t}`");
    let (h, m) = t.trim().split_once(':').ok_or_else(bad)?;
    let h: u32 = h.parse().map_err(|_| bad())?;
    let m: u32 = m.parse().map_err(|_| bad())?;
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok((h, m))
}

/// Single-quoted for sh, with `%` escaped: cron turns a bare `%` into a newline.
fn cron_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''").replace('%', r"\%"))
}

/// The crontab without our entry, and our job line if there was one.
fn split(text: &str) -> (String, Option<String>) {
    let mut rest = String::new();
    let mut ours = None;
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if l.trim() == MARKER {
            if let Some(job) = lines.next() {
                ours = Some(job.to_string());
            }
            continue;
        }
        rest.push_str(l);
        rest.push('\n');
    }
    (rest, ours)
}

fn crontab() -> Command {
    let bin = std::env::var_os("REG_CRONTAB")
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "crontab".into());
    Command::new(bin)
}

fn read_crontab() -> Result<String> {
    let out = crontab()
        .arg("-l")
        .stdin(Stdio::null())
        .output()
        .context("running `crontab -l` (is cron installed?)")?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // "no crontab for <user>": an empty crontab, not an error. Anything else is, so that
    // a failed read never becomes a write that wipes the person's jobs.
    if err.to_lowercase().contains("no crontab") {
        return Ok(String::new());
    }
    bail!("`crontab -l` failed: {}", err.trim());
}

fn write_crontab(text: &str) -> Result<()> {
    let mut child = crontab()
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running `crontab -`")?;
    child
        .stdin
        .take()
        .context("crontab stdin")?
        .write_all(text.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "`crontab -` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_other_jobs() {
        let text =
            format!("MAILTO=me\n0 * * * * backup\n{MARKER}\n17 3 * * * x\n5 5 * * * other\n");
        let (rest, ours) = split(&text);
        assert_eq!(rest, "MAILTO=me\n0 * * * * backup\n5 5 * * * other\n");
        assert_eq!(ours.as_deref(), Some("17 3 * * * x"));
        assert_eq!(split("a\n"), ("a\n".to_string(), None));
    }

    #[test]
    fn times_and_quoting() {
        assert_eq!(parse_at("9:05").unwrap(), (9, 5));
        assert!(parse_at("24:00").is_err());
        assert!(parse_at("noon").is_err());
        assert_eq!(cron_quote("/a b/50%'x"), r"'/a b/50\%'\''x'");
        assert_eq!(describe("5 9 * * 0 x"), "weekly, Sundays at 09:05");
        assert_eq!(describe("17 3 * * * x"), "daily at 03:17");
    }
}
