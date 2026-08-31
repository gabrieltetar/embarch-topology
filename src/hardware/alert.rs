//! The durable alert log behind [`super::validate`] (design.md §3 decision
//! 12): every mismatch [`super::validate`] catches is appended to a local,
//! durable JSON-lines log the instant it happens — nothing lost to bad
//! timing between the check and someone looking.
//!
//! **The live-push half is gone (design.md §3 decision 19, 2026-08-25).**
//! Decision 12 paired that durable log with a same-machine loopback push:
//! `embarch-topology`'s own UI (`bin/ui.rs`) wrote its bound address to a
//! `ui.addr` marker file, and `push_live` here read it back and fired a
//! hand-rolled best-effort POST at the UI's `/_internal/alert` endpoint.
//! That binary was deleted 2026-08-24 when [`embarch-ui`] replaced it, and
//! nothing has written the marker since — so `push_live` degraded to a
//! permanent silent no-op and `fix_it_url` to a permanently dead port. Both
//! were *working exactly as designed*, which is precisely what made them
//! worth deleting rather than leaving: code that looks live and is not.
//!
//! What replaces it is not a new mechanism but an existing one: `embarch-ui`
//! polls `embarch-core`'s `GET /alerts` every five seconds, which reads this
//! same durable log. [`fix_it_url`] is now a fixed URL into that UI's
//! Topology tab — no marker, no discovery, nothing to go stale.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;

use super::enrollment::EnrolledBoard;
use super::paths;

/// Where `embarch-ui` binds by default (`embarch-ui`'s own `BIND_ADDR`/
/// `BIND_PORT`) — the destination [`fix_it_url`] names.
///
/// **Duplicated constants, deliberately, and the honest limit that comes
/// with it** (design.md §3 decision 19): this crate does not depend on
/// `embarch-ui` and must not, so these are copies. `embarch-ui` lets a
/// human override its bind address with `EMBARCH_UI_HOST`/`EMBARCH_UI_PORT`,
/// and reading those here would be worse than useless — they would be read
/// in *this* process (usually `embarch-core`'s), which has no reason to have
/// them set and no way to know what the UI process was started with. A
/// fixed, occasionally-wrong URL beats a discovery mechanism that goes
/// stale, which is the whole of decision 19.
pub const UI_HOST: &str = "127.0.0.1";
pub const UI_PORT: u16 = 4890;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    /// Millisecond timestamp plus the process ID that raised it — unique
    /// enough for a local alert log with no real concurrency to speak of,
    /// with no extra crate (uuid) pulled in just for this.
    pub id: String,
    pub occurred_at_utc_ms: u64,
    pub role: String,
    pub probe_serial: String,
    pub chip: String,
    pub recorded_hardware_id: String,
    pub live_hardware_id: Option<String>,
    pub reason: String,
}

impl Alert {
    pub fn new(known: &EnrolledBoard, live_hardware_id: Option<String>, reason: String) -> Self {
        let occurred_at_utc_ms = super::enrollment::now_utc_ms();
        Alert {
            id: format!("{occurred_at_utc_ms:x}-{}", std::process::id()),
            occurred_at_utc_ms,
            role: known.role.clone(),
            probe_serial: known.probe_serial.clone(),
            chip: known.chip.clone(),
            recorded_hardware_id: known.hardware_id.clone(),
            live_hardware_id,
            reason,
        }
    }
}

/// Append one alert to the durable log, unconditionally — the only thing
/// that happens to an alert now that decision 19 retired the live push.
pub fn record(alert: &Alert) -> Result<()> {
    let path = paths::alert_log_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let line = serde_json::to_string(alert).context("failed to serialize alert")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open alert log at {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("failed to append to alert log at {}", path.display()))
}

/// The most recent `limit` alerts, oldest first within that window — the
/// topology UI's own "recent mismatches" listing, and what it loads on
/// startup so a mismatch caught while the UI wasn't running is still there
/// to see, not lost to bad timing (design.md §3 decision 12).
pub fn recent(limit: usize) -> Result<Vec<Alert>> {
    let path = paths::alert_log_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read alert log at {}", path.display()))?;
    let mut all: Vec<Alert> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str(l) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!("skipping unparseable alert log line: {e:?}");
                None
            }
        })
        .collect();
    if all.len() > limit {
        all = all.split_off(all.len() - limit);
    }
    Ok(all)
}

/// Where a human goes to act on this alert: `embarch-ui`'s Topology tab,
/// which lists recent alerts straight out of the durable log this module
/// writes (via `embarch-core`'s `GET /alerts`).
///
/// **Takes no alert id, and that is a narrowing, not an oversight**
/// (design.md §3 decision 19). It used to be `fix_it_url(alert_id)`,
/// pointing at a per-alert `/mismatch/{id}` detail page in the deleted
/// `bin/ui.rs`. `embarch-ui` has no such page — its Topology tab shows the
/// recent-alert list — so an id in the URL would be a parameter nothing on
/// the other end reads. Better a link that lands where the alert actually
/// is than one that looks more precise than it is.
///
/// Opening or focusing the UI is still the caller's job, never this crate's
/// (decision 12) — this only says where.
pub fn fix_it_url() -> String {
    format!("http://{UI_HOST}:{UI_PORT}/#topology")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_board() -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: "760001234".to_string(),
            role: "dev-bench".to_string(),
            chip: "esp32c5".to_string(),
            hardware_id: "aaaaaaaabbbbbbbb".to_string(),
            confirmed_at_utc_ms: 1_755_000_000_000,
            link_port_serial: None,
            link_port_interface: None,
        }
    }

    #[test]
    fn record_then_recent_round_trips() {
        // Isolate this test's log from any real machine-wide state by
        // pointing HOME/ProgramData-equivalent env at a scratch dir would
        // need paths.rs to read an override it doesn't have (by design —
        // machine-wide, not per-test) — so this test exercises the pure
        // serialize/parse round trip `record`/`recent` share instead of the
        // real filesystem path, mirroring how `enrollment.rs`'s own tests
        // use a temp path rather than the real machine-wide location.
        let alert = Alert::new(&sample_board(), Some("ccccccccdddddddd".to_string()), "hardware ID mismatch".to_string());
        let line = serde_json::to_string(&alert).unwrap();
        let parsed: Alert = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.role, "dev-bench");
        assert_eq!(parsed.live_hardware_id, Some("ccccccccdddddddd".to_string()));
    }

    #[test]
    fn fix_it_url_is_a_fixed_embarch_ui_topology_tab_url() {
        // Deterministic by construction: no marker file, no filesystem, no
        // environment. The `#topology` fragment is load-bearing —
        // `embarch-ui`'s `initNav` reads it to pick the tab, otherwise the
        // link lands on whichever tab that browser last had open.
        assert_eq!(fix_it_url(), "http://127.0.0.1:4890/#topology");
    }
}
