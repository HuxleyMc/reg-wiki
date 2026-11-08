//! `~/.reg` (or `$REG_HOME`): config, index and the wiki itself.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::{git, util};

pub struct Home {
    pub wiki: PathBuf,
    pub db: PathBuf,
    pub config: PathBuf,
    pub lock: PathBuf,
}

/// Folder for pages that are not tied to one project.
pub const SHARED: &str = "_shared";

const DEFAULT_CONFIG: &str = "\
# reg-wiki configuration.
# Projects are added with `reg-wiki project add`, run inside the project's repo.
";

impl Home {
    pub fn locate() -> Result<Self> {
        let root = match std::env::var_os("REG_HOME") {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".reg"),
        };
        Ok(Home {
            wiki: root.join("wiki"),
            db: root.join("reg.db"),
            config: root.join("config.toml"),
            lock: root.join("write.lock"),
        })
    }

    /// Create the directory layout, default config and the wiki's git repo if missing.
    pub fn ensure(&self) -> Result<()> {
        fs::create_dir_all(self.wiki.join(SHARED))
            .with_context(|| format!("creating {}", self.wiki.display()))?;
        if !self.config.exists() {
            util::atomic_write(&self.config, DEFAULT_CONFIG.as_bytes())?;
        }
        git::ensure_repo(&self.wiki)
    }

    /// Exclusive lock held for the whole write → commit → reindex sequence.
    pub fn lock(&self) -> Result<fs::File> {
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.lock)
            .with_context(|| format!("opening {}", self.lock.display()))?;
        f.lock().context("locking the wiki for writing")?;
        Ok(f)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ProjectCfg {
    pub name: String,
    /// Normalized git remote, e.g. `github.com/acme/myapp`.
    pub remote: Option<String>,
    /// Absolute path of the repo root, used when there is no remote.
    pub root: Option<String>,
}

/// `config.toml`, edited with `toml_edit` so comments and unknown sections survive.
pub struct Config {
    path: PathBuf,
    doc: DocumentMut,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let doc = text
            .parse::<DocumentMut>()
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Config {
            path: path.to_path_buf(),
            doc,
        })
    }

    pub fn projects(&self) -> Vec<ProjectCfg> {
        let Some(aot) = self.doc.get("projects").and_then(Item::as_array_of_tables) else {
            return Vec::new();
        };
        aot.iter()
            .filter_map(|t| {
                let s = |k: &str| t.get(k).and_then(Item::as_str).map(str::to_string);
                Some(ProjectCfg {
                    name: s("name")?,
                    remote: s("remote"),
                    root: s("root"),
                })
            })
            .collect()
    }

    pub fn add_project(&mut self, p: &ProjectCfg) -> Result<()> {
        let item = self
            .doc
            .entry("projects")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
        let Some(aot) = item.as_array_of_tables_mut() else {
            bail!("`projects` in config.toml must be an array of tables ([[projects]])");
        };
        let mut t = Table::new();
        t["name"] = value(&p.name);
        if let Some(r) = &p.remote {
            t["remote"] = value(r);
        }
        if let Some(r) = &p.root {
            t["root"] = value(r);
        }
        aot.push(t);
        Ok(())
    }

    pub fn remove_project(&mut self, name: &str) -> bool {
        let Some(aot) = self
            .doc
            .get_mut("projects")
            .and_then(Item::as_array_of_tables_mut)
        else {
            return false;
        };
        let before = aot.len();
        aot.retain(|t| t.get("name").and_then(Item::as_str) != Some(name));
        aot.len() != before
    }

    /// An integer setting such as `[limits] main_page_lines`.
    pub fn int(&self, section: &str, key: &str) -> Option<i64> {
        self.doc.get(section)?.get(key)?.as_integer()
    }

    pub fn save(&self) -> Result<()> {
        util::atomic_write(&self.path, self.doc.to_string().as_bytes())
    }
}
