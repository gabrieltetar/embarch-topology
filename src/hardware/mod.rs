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
pub mod signal;
mod validate;

pub use alert::{Alert, DEFAULT_UI_PORT};
pub use enrollment::EnrolledBoard;
pub use paths::{alert_log_path, data_dir, enrollment_path, ui_marker_path};
pub use port::{DetectedPort, DevBenchPort, NotFound as DevBenchNotFound, DEV_BENCH_ROLE};
pub use signal::{
    Route, SignalDirection, SignalLink, SignalMismatch, SignalNotDeclared,
};
pub use validate::{AttachedProbe, NotEnrolled, TopologyMismatch};

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

/// Declares dev-bench's runtime-link USB serial — a second fact from its
/// JTAG probe's own serial, needed when the two are different physical USB
/// devices (`EnrolledBoard::link_port_serial`'s own doc comment). The role
/// must already be enrolled; this only ever amends that existing row.
/// [`resolve_dev_bench_port`] prefers this over its old JTAG-probe-serial
/// fallback once it's set.
pub fn set_dev_bench_link_port_serial(serial: &str) -> anyhow::Result<()> {
    enrollment::set_link_port_serial(DEV_BENCH_ROLE, serial)
}

/// Every declared DUT signal link (design.md §3 decision 18).
pub fn list_signals() -> anyhow::Result<Vec<SignalLink>> {
    signal::list()
}

/// Look up one declared signal by the name a `Study` taps it by.
pub fn find_signal(name: &str) -> anyhow::Result<Option<SignalLink>> {
    signal::find(name)
}

/// Declares (or re-declares) where a named signal currently goes — the
/// write behind Core's future `POST /signals` (design.md §5). Idempotent by
/// name; re-declaring is how a route migrates.
pub fn declare_signal(link: SignalLink) -> anyhow::Result<()> {
    signal::declare(link)
}

/// Removes a declared signal. `Ok(false)` if nothing was declared under
/// that name.
pub fn remove_signal(name: &str) -> anyhow::Result<bool> {
    signal::remove(name)
}

/// Resolves a `Route::Direct` signal to the serial port currently carrying
/// it, live, reusing the same `Filter` machinery dev-bench's own link
/// resolution uses (design.md §3 decisions 17, 18). Blocking — call via
/// `spawn_blocking` on an async runtime.
pub fn resolve_signal_port(name: &str) -> anyhow::Result<DetectedPort> {
    signal::resolve_port(name)
}

/// Confirms a declared signal is where it says it is, before an operation
/// that needs it (design.md §3 decision 18). See
/// [`signal::validate`] for exactly what this can and cannot honestly
/// assert.
pub fn validate_signal(name: &str) -> anyhow::Result<SignalLink> {
    signal::validate(name)
}

/// Finds `embarch-dev-bench`'s serial port on this machine, live, on every
/// call (design.md §3 decisions 3, 9 — no env var overrides any more).
/// Blocking — call via `spawn_blocking` on an async runtime.
pub fn resolve_dev_bench_port() -> anyhow::Result<DevBenchPort> {
    port::detect()
}

/// `POST /probes/enroll`'s implementation: records a probe's live hardware
/// ID against `role`/`chip`. `probe_serial` picks which attached probe when
/// more than one is present ([`validate::enroll`]'s own doc comment);
/// `None` requires exactly one to be attached, same as before this param
/// existed.
pub fn enroll(role: &str, chip: &str, probe_serial: Option<&str>) -> anyhow::Result<EnrolledBoard> {
    validate::enroll(role, chip, probe_serial)
}

/// Every debug probe currently attached, live — read-only, nothing
/// persisted. What `embarch-topology`'s own UI/CLI shows a human *before*
/// enrolling, so "exactly one probe attached" is something they can check
/// ahead of a submission rather than discover from its error.
pub fn list_attached_probes() -> Vec<AttachedProbe> {
    validate::list_attached_probes()
}

/// Best-effort early diagnosis for an about-to-fail attach: an unpowered
/// board is the single most common real cause behind probe-rs's generic
/// "target did not respond" — confirmed against a real incident, found
/// enrolling a real DUT that turned out to simply have no power connected
/// (`embarch-core/design.md` §3 decision 26). Call this right after
/// opening a probe and before `Probe::attach` — every attach call site in
/// this crate and `embarch-core` does (decision 8's "one implementation,
/// multiple call sites," extended here from identity validation to this).
///
/// Reads the probe's own sensed target-voltage pin
/// (`Probe::get_target_voltage`) if it has one — not every probe type
/// supports this (`Ok(None)`), in which case this can't help and callers
/// just proceed to attach normally, same as a plausible-looking reading.
/// Only a suspiciously-low one short-circuits with a message naming the
/// actual likely cause, before the slower, generically-worded `attach()`
/// call ever runs.
pub fn check_target_powered(probe: &mut probe_rs::probe::Probe) -> anyhow::Result<()> {
    validate::check_target_powered(probe)
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
