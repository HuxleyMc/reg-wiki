use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Length of the hash shown to users and accepted by `write --base`.
pub const SHORT_HASH: usize = 12;

pub fn today() -> String {
    jiff::Zoned::now().date().to_string()
}

pub fn now() -> String {
    jiff::Timestamp::now().to_string()
}

pub fn hash_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest.iter() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn short(hash: &str) -> &str {
    &hash[..hash.len().min(SHORT_HASH)]
}

/// Write to a temp file in the same directory, then rename over the target.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".reg-tmp-")
        .tempfile_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

/// Path relative to `base`, always with `/` separators.
pub fn rel_str(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}
