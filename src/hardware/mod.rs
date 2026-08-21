//! Hardware topology: what's physically wired to what (design.md §2). Gated
//! behind the `hardware` feature — the only consumer today is
//! `embarch-core` (`features = ["hardware"]`); `embarch-api`/
//! `embarch-umbrella` never pull in `probe-rs`/`serialport` transitively
//! (their own "no hardware knowledge" boundary — `embarch-umbrella`'s
//! `Cargo.toml` comment of the same name — stays true with this crate as a
//! dependency, not just without it).

mod alert;
mod enrollment;
mod hardware_id;
mod paths;
mod port;
mod validate;

pub use alert::{Alert, DEFAULT_UI_PORT};
pub use enrollment::EnrolledBoard;
pub use paths::{alert_log_path, data_dir, enrollment_path, ui_marker_path};
pub use port::{DevBenchPort, NotFound as DevBenchNotFound, DEV_BENCH_ROLE};
pub use validate::TopologyMismatch;

/// Recent alerts from the durable log — `embarch-topology`'s own UI/CLI
/// listing, and what a `doctor`-style check reports as evidence rather than
/// just "warn".
pub fn recent_alerts(limit: usize) -> anyhow::Result<Vec<Alert>> {
    alert::recent(limit)
}

/// Every currently-enrolled board.
pub fn list_enrolled() -> anyhow::Result<Vec<EnrolledBoard>> {
    enrollment::list()
}

/// Look up one enrollment by role.
pub fn find_enrolled_by_role(role: &str) -> anyhow::Result<Option<EnrolledBoard>> {
    enrollment::find_by_role(role)
}

/// Finds `embarch-dev-bench`'s serial port on this machine, live, on every
/// call (design.md §3 decisions 3, 9 — no env var overrides any more).
/// Blocking — call via `spawn_blocking` on an async runtime.
pub fn resolve_dev_bench_port() -> anyhow::Result<DevBenchPort> {
    port::detect()
}

/// `POST /probes/enroll`'s implementation: refuses anything but exactly one
/// attached probe, records its live hardware ID against `role`/`chip`.
pub fn enroll(role: &str, chip: &str) -> anyhow::Result<EnrolledBoard> {
    validate::enroll(role, chip)
}

/// Re-verifies an already-enrolled board's live identity by the probe's own
/// USB serial number. On mismatch, the returned error durably logs the
/// finding and live-pushes it to `embarch-topology`'s UI if one is running,
/// and downcasts to [`TopologyMismatch`] for the structured fields/fix-it
/// URL (design.md §3 decisions 8, 12).
pub fn validate_serial(serial: &str) -> anyhow::Result<EnrolledBoard> {
    validate::validate_serial(serial)
}

/// Re-verifies an already-enrolled board's live identity by enrollment
/// `role` — for a link that isn't itself a probe-rs-recognized debug probe
/// (see [`DEV_BENCH_ROLE`]'s own doc comment).
pub fn validate_role(role: &str) -> anyhow::Result<EnrolledBoard> {
    validate::validate_role(role)
}
