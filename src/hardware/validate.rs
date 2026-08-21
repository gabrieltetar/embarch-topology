//! Live board-identity validation — formerly `embarch-core`'s own
//! `board_gate.rs` (design.md §3 decisions 2, 8). One implementation,
//! multiple call sites: `embarch-core`'s `hardware::flash`/`reset` and
//! `study.rs`'s dev-bench handshake call exactly the functions here, and so
//! does `embarch-topology`'s own CLI/UI — there is no second, independently-
//! reasoned copy of this logic anywhere in the suite to disagree with it.
//!
//! Fails closed in every branch — an unenrolled or now-mismatched probe
//! blocks the operation entirely, never a guess. Every mismatch is durably
//! logged and live-pushed (`alert.rs`) before the structured error is even
//! constructed, so the record exists regardless of what the caller does
//! with the `Err` it gets back.
//!
//! **What this does not close** (design.md §3 decision 8's own "real gap"
//! note, and §5's open question): confirming the enrolled JTAG-capable probe
//! is still attached and matches proves the *debug connection* to a role's
//! chip is genuine. It does not, on its own, prove that some other
//! currently-detected link (`super::port::detect`'s dev-bench serial port,
//! say) is wired to that *same physical chip* rather than a different board
//! that happens to share the role's VID heuristic — that would need the
//! link's own protocol to carry a hardware ID (a firmware-level change,
//! outside what this crate can add on its own). `validate_role` here is the
//! strongest check achievable without that: it re-verifies the enrolled
//! debug connection every time, which is what `embarch-core`'s dev-bench
//! handshake already calls before ever opening the link.

use anyhow::{Context, Result};
use probe_rs::probe::list::Lister;
use probe_rs::Permissions;

use super::alert::{self, Alert};
use super::enrollment::{self, EnrolledBoard};
use super::hardware_id;

/// A live check found the enrolled board isn't there, or isn't what was
/// recorded — downcast an `anyhow::Error` from [`validate_role`]/
/// [`validate_serial`] to this to get the structured fields and the
/// fix-it URL (design.md §3 decision 12), the same idiom
/// [`super::port::NotFound`] already established for "no guessing" errors
/// in this crate.
#[derive(Debug)]
pub struct TopologyMismatch {
    pub role: String,
    pub probe_serial: String,
    pub chip: String,
    pub recorded_hardware_id: String,
    /// `None` when the enrolled probe couldn't even be opened (unplugged,
    /// most likely) — a mismatch either way, just not one with a live
    /// hardware ID to show.
    pub live_hardware_id: Option<String>,
    pub reason: String,
    pub fix_it_url: String,
}

impl std::fmt::Display for TopologyMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "topology-mismatch: {} — fix it at {}",
            self.reason, self.fix_it_url
        )
    }
}

impl std::error::Error for TopologyMismatch {}

fn raise(known: &EnrolledBoard, live_hardware_id: Option<String>, reason: String) -> anyhow::Error {
    let alert = Alert::new(known, live_hardware_id.clone(), reason.clone());
    if let Err(e) = alert::record(&alert) {
        // A logging failure must never mask the real mismatch underneath it
        // — surface both, but still return the mismatch as the actual error.
        tracing::error!("failed to durably log a topology mismatch: {e:?}");
    }
    alert::push_live(&alert);

    anyhow::Error::new(TopologyMismatch {
        role: known.role.clone(),
        probe_serial: known.probe_serial.clone(),
        chip: known.chip.clone(),
        recorded_hardware_id: known.hardware_id.clone(),
        live_hardware_id,
        reason,
        fix_it_url: alert::fix_it_url(&alert.id),
    })
}

fn validate_known(known: EnrolledBoard) -> Result<EnrolledBoard> {
    let lister = Lister::new();
    let probe_info = match lister
        .list_all()
        .into_iter()
        .find(|p| p.serial_number.as_deref() == Some(known.probe_serial.as_str()))
    {
        Some(p) => p,
        None => {
            return Err(raise(
                &known,
                None,
                format!(
                    "probe '{}' enrolled as role '{}' is not currently attached",
                    known.probe_serial, known.role
                ),
            ))
        }
    };

    let probe = probe_info
        .open()
        .context("failed to open the enrolled probe for the board-identity gate")?;
    let mut session = probe
        .attach(known.chip.as_str(), Permissions::default())
        .with_context(|| format!("failed to attach to '{}' for the board-identity gate", known.chip))?;
    let mut core = session
        .core(0)
        .context("failed to select core 0 for the board-identity gate")?;
    let live_hardware_id = hardware_id::read(&mut core, &known.chip)?;
    drop(core);
    drop(session);

    if live_hardware_id != known.hardware_id {
        return Err(raise(
            &known,
            Some(live_hardware_id.clone()),
            format!(
                "probe '{}' is enrolled as role '{}' (chip '{}') with hardware ID '{}', but the \
                 attached chip now reports '{live_hardware_id}' — re-enroll if this is deliberate",
                known.probe_serial, known.role, known.chip, known.hardware_id
            ),
        ));
    }

    Ok(known)
}

/// Validate by the probe's own USB serial number — `embarch-core`'s
/// `hardware::flash`/`reset` path, once it has already resolved which
/// attached probe a call means.
pub fn validate_serial(serial: &str) -> Result<EnrolledBoard> {
    let known = enrollment::find(serial)?.with_context(|| {
        format!(
            "probe '{serial}' is not enrolled — enroll it first (`embarch-topology enroll`), \
             with only this board's probe attached"
        )
    })?;
    validate_known(known)
}

/// Validate by enrollment `role` rather than by serial — for a link that
/// isn't itself a probe-rs-recognized debug probe at all (dev-bench's UART
/// bridge chip; see `super::port`'s own doc comment). `embarch-core`'s
/// dev-bench handshake calls this before ever opening the link.
pub fn validate_role(role: &str) -> Result<EnrolledBoard> {
    let known = enrollment::find_by_role(role)?.with_context(|| {
        format!(
            "no board enrolled under role '{role}' — enroll it first (`embarch-topology enroll`), \
             with only this board's probe attached"
        )
    })?;
    validate_known(known)
}

/// `enroll_probe`'s implementation (`embarch-api`'s MCP tool of the same
/// name, via `POST /probes/enroll`): refuses anything but exactly one
/// attached probe, attaches as `chip`, reads its live hardware ID, and
/// records the association — overwriting any prior entry for the same probe
/// serial.
pub fn enroll(role: &str, chip: &str) -> Result<EnrolledBoard> {
    let lister = Lister::new();
    let probes = lister.list_all();
    if probes.len() != 1 {
        anyhow::bail!(
            "enrollment requires exactly one debug probe attached ({} seen) — plug in only the \
             board you mean to enroll, then retry",
            probes.len()
        );
    }
    let info = probes.into_iter().next().expect("checked len == 1 above");
    let serial = info.serial_number.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "the attached probe ({}) reports no USB serial number — it can't be enrolled \
             without one to key on",
            info.identifier
        )
    })?;

    let probe = info.open().context("failed to open the attached debug probe")?;
    let mut session = probe
        .attach(chip, Permissions::default())
        .with_context(|| format!("failed to attach to '{chip}'"))?;
    let mut core = session.core(0).context("failed to select core 0")?;
    let hardware_id = hardware_id::read(&mut core, chip)?;
    drop(core);
    drop(session);

    let board = EnrolledBoard {
        probe_serial: serial,
        role: role.to_string(),
        chip: chip.to_string(),
        hardware_id,
        confirmed_at_utc_ms: enrollment::now_utc_ms(),
    };
    enrollment::upsert(board.clone())?;
    Ok(board)
}
