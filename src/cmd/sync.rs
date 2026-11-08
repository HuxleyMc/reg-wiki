use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;

use super::{Ctx, print_json};
use crate::db::reconcile::{self, Stats};

/// The reconcile already ran when the context opened; report what it did.
pub fn sync(ctx: &Ctx) -> Result<ExitCode> {
    report(ctx, "synced", &ctx.stats);
    Ok(ExitCode::SUCCESS)
}

pub fn reindex(ctx: &mut Ctx) -> Result<ExitCode> {
    let _lock = ctx.home.lock()?;
    let stats = reconcile::reindex(&mut ctx.conn, &ctx.home.wiki, &ctx.cfg.projects())?;
    for w in &stats.warnings {
        super::warn(w);
    }
    report(ctx, "reindexed", &stats);
    Ok(ExitCode::SUCCESS)
}

fn report(ctx: &Ctx, what: &str, s: &Stats) {
    if ctx.json {
        print_json(&json!({ "status": what, "stats": s }));
    } else {
        println!(
            "{what}: {} pages ({} added, {} updated, {} removed)",
            s.pages, s.added, s.updated, s.removed
        );
    }
}
