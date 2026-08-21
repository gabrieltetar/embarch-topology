//! Where this crate's own persisted state lives — the machine-wide directory
//! `embarch-core`'s `token_store.rs` already established the convention for
//! (`%ProgramData%\embarch` / `/var/lib/embarch`), one level down in a
//! `topology` subdirectory this crate owns outright.
//!
//! This resolves design.md §5's "crate-internal storage schema/location"
//! open question: reuse the exact directory `known_boards.toml` already used
//! (continuity — this *is* that file's replacement, decision 3), scoped under
//! its own subdirectory so this crate's files (`enrollment.toml`,
//! `alerts.jsonl`, the UI's own `ui.addr` marker) don't sit loose next to
//! Core's `token`/`logs`.

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

/// Where the running `embarch-topology` UI (if any) records its own address,
/// so a `validate()` call anywhere on the machine can find it and push a
/// live event (design.md §3 decision 12) — see `crate::hardware::alert`'s
/// own doc comment for the full mechanism this file is one half of.
pub fn ui_marker_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("ui.addr"))
}
