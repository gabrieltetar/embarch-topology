//! The durable alert log + live event push behind [`super::validate`]
//! (design.md §3 decision 12): every mismatch [`super::validate`] catches is
//! (a) appended to a local, durable JSON-lines log the instant it happens —
//! nothing lost to bad timing between the check and someone looking — and
//! (b) pushed live to `embarch-topology`'s own UI process, *if* one happens
//! to be running on this machine right now.
//!
//! **How the live push actually crosses the process boundary.** `validate()`
//! typically runs inside `embarch-core` (or `embarch-topology`'s own CLI) —
//! a different OS process than the UI server, so an in-process
//! `tokio::sync::broadcast` channel alone can't carry an event from one to
//! the other. The fix: when `embarch-topology`'s UI starts, it writes its
//! own loopback address to [`paths::ui_marker_path`] (a plain
//! `127.0.0.1:PORT` text file); `push_live` here reads that file and, if
//! present, fires a tiny best-effort HTTP POST to the UI's own
//! `/_internal/alert` endpoint (see `bin/ui.rs`), which is what actually
//! feeds the UI's in-process `broadcast` channel that a connected browser
//! tab's SSE stream reads from. No queue, no retry, no dependency on the UI
//! being up — a missing/stale marker file, or a POST that fails outright,
//! just means nothing happened live; the durable log entry this always
//! writes first is the fallback (design.md §3 decision 12: "the durable
//! record and the live view are two ends of one alert, not separate
//! mechanisms").
//!
//! The POST is hand-rolled over a raw `TcpStream` rather than pulling in an
//! async HTTP client for a call this narrow (loopback, one hop, a few dozen
//! bytes of JSON, best-effort) — `reqwest` is already a dependency for
//! [`crate::software`]'s real Core-reachability probing, but that's a
//! meaningfully different job (arbitrary hosts, redirects, TLS) from "POST
//! to a socket address I just read from a file on this same machine."

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use super::enrollment::EnrolledBoard;
use super::paths;

/// The port `embarch-topology`'s UI binds by default (`bin/ui.rs`) — also
/// what a [`TopologyMismatch`](super::validate::TopologyMismatch)'s
/// `fix_it_url` points at when no UI happens to be running right now (in
/// which case opening the URL is what starts one, on this same port, that
/// then loads the same alert straight out of the durable log).
pub const DEFAULT_UI_PORT: u16 = 4886;

const PUSH_TIMEOUT: Duration = Duration::from_millis(300);

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

/// Append one alert to the durable log, unconditionally — called before
/// [`push_live`] on every path, never the other way around.
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

/// Best-effort: if `embarch-topology`'s UI is running on this machine (per
/// [`paths::ui_marker_path`]), push this alert to it live. Never fails
/// loudly — the durable [`record`] call is what actually matters; this is
/// strictly additive.
pub fn push_live(alert: &Alert) {
    if let Err(e) = try_push_live(alert) {
        tracing::debug!("no live topology UI to push this alert to: {e:?}");
    }
}

fn try_push_live(alert: &Alert) -> Result<()> {
    let marker = paths::ui_marker_path()?;
    let addr = std::fs::read_to_string(&marker)
        .with_context(|| format!("no UI marker file at {}", marker.display()))?
        .trim()
        .to_string();

    let body = serde_json::to_string(alert)?;
    let request = format!(
        "POST /_internal/alert HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let mut stream = TcpStream::connect(&addr).context("connect to UI marker address")?;
    stream.set_write_timeout(Some(PUSH_TIMEOUT)).ok();
    stream.set_read_timeout(Some(PUSH_TIMEOUT)).ok();
    stream.write_all(request.as_bytes()).context("write alert push request")?;
    // Drain (and discard) whatever the UI sends back — just confirms the
    // connection was accepted, not that the browser side is listening.
    let mut buf = [0u8; 64];
    let _ = stream.read(&mut buf);
    Ok(())
}

/// Where opening (or focusing) `embarch-topology`'s UI lands for this alert
/// — whether or not a UI happens to be running right now (design.md §3
/// decision 12: opening/focusing the UI is the caller's job, never Core's).
pub fn fix_it_url(alert_id: &str) -> String {
    let port = paths::ui_marker_path()
        .ok()
        .and_then(|marker| std::fs::read_to_string(marker).ok())
        .and_then(|addr| addr.trim().rsplit(':').next().map(str::to_string))
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_UI_PORT);
    format!("http://127.0.0.1:{port}/mismatch/{alert_id}")
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
    fn fix_it_url_falls_back_to_the_default_port_with_no_marker_file() {
        // No real marker file exists in this test environment (nothing has
        // started a UI here), so this always exercises the fallback branch.
        let url = fix_it_url("deadbeef-123");
        assert!(url.starts_with("http://127.0.0.1:"));
        assert!(url.ends_with("/mismatch/deadbeef-123"));
    }
}
