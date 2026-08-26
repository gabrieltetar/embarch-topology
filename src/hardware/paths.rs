//! Where this crate's own persisted state lives — the machine-wide directory
//! `embarch-core`'s `token_store.rs` already established the convention for
//! (`%ProgramData%\embarch` / `/var/lib/embarch`), one level down in a
//! `topology` subdirectory this crate owns outright.
//!
//! This resolves design.md §5's "crate-internal storage schema/location"
//! open question: reuse the exact directory `known_boards.toml` already used
//! (continuity — this *is* that file's replacement, decision 3), scoped under
//! its own subdirectory so this crate's files (`enrollment.toml`,
//! `alerts.jsonl`) don't sit loose next to Core's `token`/`logs`.

use anyhow::Result;
use std::path::PathBuf;

#[cfg(windows)]
fn machine_data_dir() -> Result<PathBuf> {
    use anyhow::Context;
    let program_data =
        std::env::var("ProgramData").context("ProgramData environment variable is not set")?;
    Ok(PathBuf::from(program_data).join("embarch"))
}

#[cfg(unix)]
fn machine_data_dir() -> Result<PathBuf> {
    Ok(PathBuf::from("/var/lib/embarch"))
}

/// `<machine_data_dir>/topology` — created on first write if it doesn't
/// exist yet. Every path this module hands out is a child of this one.
pub fn data_dir() -> Result<PathBuf> {
    let dir = machine_data_dir()?.join("topology");
    Ok(dir)
}

pub fn enrollment_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("enrollment.toml"))
}

pub fn alert_log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("alerts.jsonl"))
}

// `ui_marker_path()` (`<data_dir>/ui.addr`) lived here until 2026-08-25 —
// the running UI's own bound address, written by `bin/ui.rs` so a
// `validate()` call elsewhere on the machine could push it a live alert.
// `bin/ui.rs` was deleted 2026-08-24 and `embarch-ui` never wrote the
// marker, so nothing has produced this file since; retired with the rest of
// the live-push mechanism (design.md §3 decision 19). Named here rather
// than silently dropped because the file may still exist on a machine that
// ran the old UI, and nothing reads it any more.
