//! Hardware topology: what's physically wired to what (spec.md). Gated
//! behind the `hardware` feature — the only consumer today is
//! `embarch-core` (`features = ["hardware"]`); `embarch-api`/
//! `embarch-umbrella` never pull in `probe-rs`/`serialport` transitively
//! (their own "no hardware knowledge" boundary — `embarch-umbrella`'s
//! `Cargo.toml` comment of the same name — stays true with this crate as a
//! dependency, not just without it).
//!
//! **Two gates, not one** (decision 31). Everything below is behind
//! `hardware`; the plain data types are additionally reachable behind `wire`,
//! which `hardware` implies. Under `wire` alone this module is its types and
//! nothing else — no `probe-rs`, no `serialport`, no `enrollment.toml`. The
//! boundary is exactly "a fact Core serves" versus "the machinery that
//! produces it", and it is why `embarch-core-client` can stop hand-mirroring
//! seven types the compiler had no way to compare against their originals.

mod alert;
mod enrollment;
#[cfg(feature = "hardware")]
pub mod hardware_id;
#[cfg(feature = "hardware")]
mod paths;
mod port;
pub mod signal;
#[cfg(feature = "hardware")]
mod validate;

pub use alert::{Alert, UI_HOST, UI_PORT};
pub use enrollment::EnrolledBoard;
#[cfg(feature = "hardware")]
pub use hardware_id::{compare_self_reported, SelfReportedIdentity};
#[cfg(feature = "hardware")]
pub use paths::{alert_log_path, data_dir, enrollment_path};
pub use port::{
    DetectedPort, DevBenchPort, ExcludingRule as DevBenchNotFoundRule,
    NotFound as DevBenchNotFound, DECLARED_SERIAL, DEV_BENCH_ROLE, ENUMERATED,
};
pub use signal::{
    Route, SignalDirection, SignalLink, SignalMismatch, SignalNotDeclared,
};
#[cfg(feature = "hardware")]
pub use validate::{AttachedProbe, NotEnrolled, TopologyMismatch, Validation};

/// Recent alerts from the durable log — `embarch-topology`'s own UI/CLI
/// listing, and what a `doctor`-style check reports as evidence rather than
/// just "warn".
#[cfg(feature = "hardware")]
pub fn recent_alerts(limit: usize) -> anyhow::Result<Vec<Alert>> {
    alert::recent(limit)
}

/// Every currently-enrolled board.
#[cfg(feature = "hardware")]
pub fn list_enrolled() -> anyhow::Result<Vec<EnrolledBoard>> {
    enrollment::list()
}

/// Look up one enrollment by role.
#[cfg(feature = "hardware")]
pub fn find_enrolled_by_role(role: &str) -> anyhow::Result<Option<EnrolledBoard>> {
    enrollment::find_by_role(role)
}

/// Declares dev-bench's runtime-link USB serial — a second fact from its
/// JTAG probe's own serial, needed when the two are different physical USB
/// devices (`EnrolledBoard::link_port_serial`'s own doc comment). The role
/// must already be enrolled; this only ever amends that existing row.
/// [`resolve_dev_bench_port`] prefers this over its old JTAG-probe-serial
/// fallback once it's set.
#[cfg(feature = "hardware")]
pub fn set_dev_bench_link_port_serial(serial: &str) -> anyhow::Result<()> {
    enrollment::set_link_port_serial(DEV_BENCH_ROLE, serial)
}

/// Declares which USB interface of dev-bench's link device carries the link
/// — the fact a *serial* cannot supply when one probe exposes several VCOMs
/// under one serial number (`EnrolledBoard::link_port_interface`'s own doc
/// comment, and the nRF54L15DK that forced it). Same contract as
/// [`set_dev_bench_link_port_serial`]: the role must already be enrolled.
#[cfg(feature = "hardware")]
pub fn set_dev_bench_link_port_interface(interface: u8) -> anyhow::Result<()> {
    enrollment::set_link_port_interface(DEV_BENCH_ROLE, interface)
}

/// Unsets a previously declared dev-bench link port serial — the clearing
/// affordance `tasks/topology/004`/decision 27 adds: [`DevBenchNotFound`]'s
/// `Display` now names this as the fix when that declared fact is what's
/// hard-narrowing detection to a port that no longer exists (decision 20),
/// and there was previously no way to do it short of hand-editing
/// `enrollment.toml`.
#[cfg(feature = "hardware")]
pub fn clear_dev_bench_link_port_serial() -> anyhow::Result<()> {
    enrollment::clear_link_port_serial(DEV_BENCH_ROLE)
}

/// Same, for the declared link port interface.
#[cfg(feature = "hardware")]
pub fn clear_dev_bench_link_port_interface() -> anyhow::Result<()> {
    enrollment::clear_link_port_interface(DEV_BENCH_ROLE)
}

/// Every declared DUT signal link (decision 18).
#[cfg(feature = "hardware")]
pub fn list_signals() -> anyhow::Result<Vec<SignalLink>> {
    signal::list()
}

/// Look up one declared signal by the name a `Study` taps it by.
#[cfg(feature = "hardware")]
pub fn find_signal(name: &str) -> anyhow::Result<Option<SignalLink>> {
    signal::find(name)
}

/// Declares (or re-declares) where a named signal currently goes — the
/// write behind Core's future `POST /signals`. Idempotent by
/// name; re-declaring is how a route migrates.
#[cfg(feature = "hardware")]
pub fn declare_signal(link: SignalLink) -> anyhow::Result<()> {
    signal::declare(link)
}

/// Removes a declared signal. `Ok(false)` if nothing was declared under
/// that name.
#[cfg(feature = "hardware")]
pub fn remove_signal(name: &str) -> anyhow::Result<bool> {
    signal::remove(name)
}

/// Resolves a `Route::Direct` signal to the serial port currently carrying
/// it, live, reusing the same `Filter` machinery dev-bench's own link
/// resolution uses (decisions 17, 18). Blocking — call via
/// `spawn_blocking` on an async runtime.
#[cfg(feature = "hardware")]
pub fn resolve_signal_port(name: &str) -> anyhow::Result<DetectedPort> {
    signal::resolve_port(name)
}

/// Confirms a declared signal is where it says it is, before an operation
/// that needs it (decision 18). See
/// [`signal::validate`] for exactly what this can and cannot honestly
/// assert.
#[cfg(feature = "hardware")]
pub fn validate_signal(name: &str) -> anyhow::Result<SignalLink> {
    signal::validate(name)
}

/// Finds `embarch-dev-bench`'s serial port on this machine, live, on every
/// call (decisions 3, 9 — no env var overrides any more).
/// Blocking — call via `spawn_blocking` on an async runtime.
#[cfg(feature = "hardware")]
pub fn resolve_dev_bench_port() -> anyhow::Result<DevBenchPort> {
    port::detect()
}

/// Every USB serial port the OS currently enumerates, unnarrowed — the list
/// a human picks a [`Route::Direct`] signal's carrier from
/// (`embarch-ui` decision 10, behind Core's `GET /serial-ports`).
///
/// Deliberately *not* [`resolve_dev_bench_port`] with the gate off; see
/// [`port::enumerate`] for why a wire's carrier cannot be VID-gated the way
/// a recognized device's link is. Blocking — call via `spawn_blocking`.
#[cfg(feature = "hardware")]
pub fn list_serial_ports() -> anyhow::Result<Vec<DetectedPort>> {
    port::enumerate()
}

/// `POST /probes/enroll`'s implementation: records a probe's live hardware
/// ID against `role`/`chip`. `probe_serial` picks which attached probe when
/// more than one is present ([`validate::enroll`]'s own doc comment);
/// `None` requires exactly one to be attached, same as before this param
/// existed.
#[cfg(feature = "hardware")]
pub fn enroll(role: &str, chip: &str, probe_serial: Option<&str>) -> anyhow::Result<EnrolledBoard> {
    validate::enroll(role, chip, probe_serial)
}

/// Every debug probe currently attached, live — read-only, nothing
/// persisted. What `embarch-topology`'s own UI/CLI shows a human *before*
/// enrolling, so "exactly one probe attached" is something they can check
/// ahead of a submission rather than discover from its error.
#[cfg(feature = "hardware")]
pub fn list_attached_probes() -> Vec<AttachedProbe> {
    validate::list_attached_probes()
}

/// Best-effort early diagnosis for an about-to-fail attach: an unpowered
/// board is the single most common real cause behind probe-rs's generic
/// "target did not respond" — confirmed against a real incident, found
/// enrolling a real DUT that turned out to simply have no power connected
/// (`embarch-core` decision 26). Call this right after
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
#[cfg(feature = "hardware")]
pub fn check_target_powered(probe: &mut probe_rs::probe::Probe) -> anyhow::Result<()> {
    validate::check_target_powered(probe)
}

/// Re-verifies an already-enrolled board's live identity by the probe's own
/// USB serial number. On mismatch, the returned error durably logs the
/// finding — the live-push half that used to accompany the log is retired
/// (decision 19); `embarch-ui` polls the same log instead — and downcasts to
/// [`TopologyMismatch`] for the structured fields/fix-it URL (decisions 8,
/// 12).
#[cfg(feature = "hardware")]
pub fn validate_serial(serial: &str) -> anyhow::Result<EnrolledBoard> {
    validate::validate_serial(serial)
}

/// Same live check as [`validate_serial`], additionally reporting when it
/// ran, distinct from the returned board's own enrolment-time
/// `confirmed_at_utc_ms` (topology decision 26).
#[cfg(feature = "hardware")]
pub fn validate_serial_timed(serial: &str) -> anyhow::Result<Validation> {
    validate::validate_serial_timed(serial)
}

/// Re-verifies an already-enrolled board's live identity by enrollment
/// `role` — for a link that isn't itself a probe-rs-recognized debug probe
/// (see [`DEV_BENCH_ROLE`]'s own doc comment).
#[cfg(feature = "hardware")]
pub fn validate_role(role: &str) -> anyhow::Result<EnrolledBoard> {
    validate::validate_role(role)
}

/// Same live check as [`validate_role`], additionally reporting when it ran,
/// distinct from the returned board's own enrolment-time
/// `confirmed_at_utc_ms` (topology decision 26).
#[cfg(feature = "hardware")]
pub fn validate_role_timed(role: &str) -> anyhow::Result<Validation> {
    validate::validate_role_timed(role)
}
