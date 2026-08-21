//! `enrollment.toml`: a machine-local table recording which physical board a
//! debug probe's serial number is actually wired to. Formerly
//! `embarch-core`'s own `known_boards.rs` / `known_boards.toml` (design.md
//! §3 decisions 2, 3, 7) — this is "the one thing that's genuinely
//! persisted, since it's declared intent, not detectable" (design.md §2):
//! nothing in a USB descriptor says "I'm wired to the DUT." A human's
//! one-time act of physically isolating a board and enrolling its probe
//! (`enroll`, [`super::validate`]) is the only source for this table; the
//! actual enforcement — the live hardware-ID readback-and-compare that makes
//! it worth trusting — is `validate.rs`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::paths;

/// One enrolled probe↔board association. `hardware_id` is the target chip's
/// own factory-burned unique ID — independent of which probe or cable
/// answers, so it survives a probe getting physically moved to a different
/// board in a way a bare USB serial number can't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrolledBoard {
    pub probe_serial: String,
    pub role: String,
    pub chip: String,
    pub hardware_id: String,
    /// UTC milliseconds since the epoch.
    pub confirmed_at_utc_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    boards: Vec<EnrolledBoard>,
}

pub fn now_utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_at(path: &Path) -> Result<Store> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read enrollment file at {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse enrollment file at {}", path.display()))
}

fn save_at(path: &Path, store: &Store) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(store).context("failed to serialize enrollment")?;
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write enrollment file at {}", path.display()))
}

/// Look up a probe's enrollment by serial number. `Ok(None)` is a normal
/// "not enrolled yet" outcome, not an error.
pub fn find(probe_serial: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store.boards.into_iter().find(|b| b.probe_serial == probe_serial))
}

/// Look up an enrollment by `role` instead of `probe_serial` —
/// [`super::port::Filter::resolve`]'s fallback use. `role` is otherwise an
/// arbitrary, unvalidated label — this is the one place a specific value
/// (`"dev-bench"`, [`super::port::DEV_BENCH_ROLE`]) is treated as
/// conventional, and only as an opt-in convenience fallback, never enforced.
/// More than one entry sharing `role` returns the first (by file order)
/// rather than erroring — a soft, best-effort lookup, not `validate.rs`'s
/// fail-closed identity gate.
pub fn find_by_role(role: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store.boards.into_iter().find(|b| b.role == role))
}

/// Every currently-enrolled board — the topology UI/CLI's own listing.
pub fn list() -> Result<Vec<EnrolledBoard>> {
    Ok(load_at(&paths::enrollment_path()?)?.boards)
}

/// Insert or replace the entry for `board.probe_serial` — enrollment is
/// idempotent, re-enrolling the same probe overwrites its old row rather
/// than accumulating stale duplicates that could disagree with each other.
pub fn upsert(board: EnrolledBoard) -> Result<()> {
    let path = paths::enrollment_path()?;
    let mut store = load_at(&path)?;
    store.boards.retain(|b| b.probe_serial != board.probe_serial);
    store.boards.push(board);
    save_at(&path, &store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("embarch-topology-enrollment-test-{name}-{}", std::process::id()))
    }

    fn sample(serial: &str) -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: serial.to_string(),
            role: "reference-dut-fw".to_string(),
            chip: "nRF54L15".to_string(),
            hardware_id: "deadbeefcafef00d".to_string(),
            confirmed_at_utc_ms: 1_755_000_000_000,
        }
    }

    #[test]
    fn missing_file_is_no_entries_not_an_error() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let store = load_at(&path).expect("a missing file should load as empty, not error");
        assert!(store.boards.is_empty());
    }

    #[test]
    fn upsert_then_find_round_trips() {
        let dir = temp_path("round-trip-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let board = sample("000852006107");
        save_at(&path, &Store { boards: vec![board.clone()] }).unwrap();

        let found = load_at(&path).unwrap().boards.into_iter().find(|b| b.probe_serial == board.probe_serial);
        assert_eq!(found, Some(board));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_by_role_matches_on_role_not_serial() {
        let dir = temp_path("by-role-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut dev_bench = sample("D0:CF:13:ED:F9:30");
        dev_bench.role = "dev-bench".to_string();
        dev_bench.chip = "esp32c5".to_string();
        save_at(&path, &Store { boards: vec![sample("000852006107"), dev_bench.clone()] }).unwrap();

        let found = load_at(&path).unwrap().boards.into_iter().find(|b| b.role == "dev-bench");
        assert_eq!(found, Some(dev_bench));

        let missing = load_at(&path).unwrap().boards.into_iter().find(|b| b.role == "no-such-role");
        assert_eq!(missing, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_overwrites_rather_than_duplicates() {
        let dir = temp_path("overwrite-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::default();
        store.boards.push(sample("same-serial"));
        save_at(&path, &store).unwrap();

        let mut updated = sample("same-serial");
        updated.hardware_id = "a-new-hardware-id".to_string();
        let mut reloaded = load_at(&path).unwrap();
        reloaded.boards.retain(|b| b.probe_serial != updated.probe_serial);
        reloaded.boards.push(updated.clone());
        save_at(&path, &reloaded).unwrap();

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards.len(), 1, "re-enrolling the same serial must not accumulate a second row");
        assert_eq!(boards[0], updated);

        std::fs::remove_dir_all(&dir).ok();
    }
}
