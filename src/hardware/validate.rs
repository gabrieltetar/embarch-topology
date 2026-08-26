//! Live board-identity validation — formerly `embarch-core`'s own
//! `board_gate.rs` (design.md §3 decisions 2, 8). One implementation,
//! multiple call sites: `embarch-core`'s `hardware::flash`/`reset` and
//! `study.rs`'s dev-bench handshake call exactly the functions here, and so
//! does `embarch-topology`'s own CLI/UI — there is no second, independently-
//! reasoned copy of this logic anywhere in the suite to disagree with it.
//!
//! Fails closed in every branch — an unenrolled or now-mismatched probe
//! blocks the operation entirely, never a guess. Every mismatch is durably
//! logged (`alert.rs`) before the structured error is even constructed, so
//! the record exists regardless of what the caller does with the `Err` it
//! gets back. The live push that used to accompany that log was retired
//! 2026-08-25 (design.md §3 decision 19) — `embarch-ui` polls the same log
//! through `embarch-core`'s `GET /alerts` instead.
//!
//! **What this does not close on its own** (design.md §3 decision 8's own
//! "real gap" note, and §5's open question): confirming the enrolled
//! JTAG-capable probe is still attached and matches proves the *debug
//! connection* to a role's chip is genuine. It does not, by itself, prove
//! that some other currently-detected link (`super::port::detect`'s
//! dev-bench serial port, say) is wired to that *same physical chip* rather
//! than a different board that happens to share the role's VID heuristic.
//!
//! **That gap is closed as of 2026-08-25, and this comment used to say it
//! could not be.** It said the fix "would need the link's own protocol to
//! carry a hardware ID (a firmware-level change, outside what this crate can
//! add on its own)" — the first half was exactly right and the second half
//! was the wrong conclusion to draw from it. `embarch-study-designer`'s
//! `HelloAck` now carries dev-bench's self-reported chip ID
//! (`embarch-core/design.md` §3 decision 35), and this crate supplies the
//! piece that makes it usable: [`super::compare_self_reported`], which is
//! chip knowledge and therefore belongs here rather than in Core. A
//! firmware protocol change being outside this crate's reach never meant the
//! *comparison* was.
//!
//! `validate_role` itself is unchanged, and still the strongest check
//! available at the moment it runs — it re-verifies the enrolled debug
//! connection every time, which is what `embarch-core`'s dev-bench handshake
//! calls before ever opening the link. What is new is that the handshake now
//! also checks the *other* end against what this returned.

use anyhow::{Context, Result};
use probe_rs::probe::list::Lister;
use probe_rs::Permissions;
use serde::Serialize;

use super::alert::{self, Alert};
use super::enrollment::{self, EnrolledBoard};
use super::hardware_id;

/// One currently-attached debug probe, as `probe-rs` sees it right now —
/// not persisted anywhere, unlike [`EnrolledBoard`]. What the UI/CLI shows a
/// human *before* they enroll, so "plug in only the board you mean to
/// enroll" (this module's own `enroll` error) is something they can check
/// ahead of time rather than discover from a failed submission.
#[derive(Debug, Clone, Serialize)]
pub struct AttachedProbe {
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
}

/// No real target this suite has ever run against senses below roughly
/// 1.6V on its normal supply/reference rail (the nRF54L15's own lowest
/// operating voltage) — placeholder-but-concrete, same posture as every
/// other hardware-unvalidated constant in this crate's design doc, chosen
/// to sit comfortably below any real target's operating range while
/// staying well above genuine unpowered leakage/noise (typically well
/// under 0.3V on an open, unpowered pin).
const UNPOWERED_VOLTAGE_THRESHOLD_V: f32 = 1.0;

/// Best-effort early diagnosis for an attach that's about to fail because
/// the board genuinely has no power — the single most common real-world
/// cause behind probe-rs's own generic "target did not respond," confirmed
/// against a real incident (`embarch-core/design.md` §3 decision 26).
/// Reads the probe's own sensed target-voltage pin
/// (`Probe::get_target_voltage`) if it has one; not every probe type
/// supports this (`Ok(None)`), in which case — same as a plausible-looking
/// reading — this can't help, and the caller just proceeds to attach
/// normally. Only a suspiciously-low reading short-circuits, with a
/// message naming the actual likely cause up front rather than leaving a
/// human to guess from `attach()`'s own generic ARM/access-port error
/// chain.
pub fn check_target_powered(probe: &mut probe_rs::probe::Probe) -> Result<()> {
    if let Ok(Some(voltage)) = probe.get_target_voltage() {
        if voltage < UNPOWERED_VOLTAGE_THRESHOLD_V {
            anyhow::bail!(
                "target appears unpowered — the probe senses only {voltage:.2}V on its target-\
                 voltage pin, too low for a real supply rail; check the board's power/USB \
                 connection, then retry"
            );
        }
    }
    Ok(())
}

/// Every debug probe `probe-rs` currently enumerates, live — the same
/// enumeration [`enroll`] itself refuses to proceed past more than one of.
pub fn list_attached_probes() -> Vec<AttachedProbe> {
    Lister::new()
        .list_all()
        .into_iter()
        .map(|p| AttachedProbe {
            identifier: p.identifier.clone(),
            vendor_id: p.vendor_id,
            product_id: p.product_id,
            serial_number: p.serial_number.clone(),
        })
        .collect()
}

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
    anyhow::Error::new(TopologyMismatch {
        role: known.role.clone(),
        probe_serial: known.probe_serial.clone(),
        chip: known.chip.clone(),
        recorded_hardware_id: known.hardware_id.clone(),
        live_hardware_id,
        reason,
        fix_it_url: alert::fix_it_url(),
    })
}

/// No board is enrolled under this role (or serial) yet — a normal,
/// expected state (design.md §3 decision 7's "declared facts can be unset"),
/// not a bug. Downcastable so a caller — `embarch-core`'s new `POST
/// /validate` (design.md §3 decision 28) — can tell it apart from a genuine
/// [`TopologyMismatch`] or an unrelated I/O error and answer with a `404`
/// rather than a `500`, the same "no guessing" idiom [`super::port::NotFound`]
/// already established for this crate's other structured errors.
#[derive(Debug)]
pub struct NotEnrolled {
    pub role: String,
}

impl std::fmt::Display for NotEnrolled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no board enrolled under role '{}' — enroll it first (`embarch-topology enroll`), \
             with only this board's probe attached",
            self.role
        )
    }
}

impl std::error::Error for NotEnrolled {}

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

    let mut probe = probe_info
        .open()
        .context("failed to open the enrolled probe for the board-identity gate")?;
    check_target_powered(&mut probe)
        .with_context(|| format!("can't validate role '{}'", known.role))?;
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
    let known = enrollment::find_by_role(role)?
        .ok_or_else(|| anyhow::Error::new(NotEnrolled { role: role.to_string() }))?;
    validate_known(known)
}

/// `enroll_probe`'s implementation (`embarch-api`'s MCP tool of the same
/// name, via `POST /probes/enroll`): attaches as `chip`, reads its live
/// hardware ID, and records the association — overwriting any prior entry
/// for the same probe serial.
///
/// `probe_serial`, when given, selects which of possibly-several currently-
/// attached probes to enroll — the same disambiguation shape `embarch-
/// core`'s `flash`/`reset` already use (that crate's decision 9), extended
/// here so a human enrolling two visibly-different boards at once (e.g. a
/// J-Link DUT alongside dev-bench's own ESP JTAG) doesn't have to physically
/// isolate them one at a time just to satisfy this function — `embarch-
/// core`'s own `GET /enroll` page's drag-and-drop UI is the first caller
/// that needs this (design.md decision 15). Omitted, the original
/// behavior is unchanged: refuses anything but exactly one attached probe,
/// the only sane default when there's no other way to tell which one a
/// caller means.
///
/// **This still doesn't — and structurally can't — verify that the probe a
/// human *picked* really is the board they think it is.** Serial number and
/// probe identifier are exactly what a same-probe-type ambiguity (decision
/// 10's own flagged risk: two boards sharing a chip family, e.g. two
/// J-Links) leaves nothing to tell apart by. `enroll`'s own live hardware-ID
/// readback below still catches a *wrong chip name* for the picked probe;
/// it can't catch "right chip, wrong physical board" when both boards
/// genuinely are that chip. That case still needs physical isolation — no
/// UI can enroll around it.
pub fn enroll(role: &str, chip: &str, probe_serial: Option<&str>) -> Result<EnrolledBoard> {
    let lister = Lister::new();
    let probes = lister.list_all();
    let info = match probe_serial {
        Some(wanted) => probes
            .into_iter()
            .find(|p| p.serial_number.as_deref() == Some(wanted))
            .ok_or_else(|| anyhow::anyhow!("no attached probe with serial '{wanted}' — is it still plugged in?"))?,
        None => {
            if probes.len() != 1 {
                anyhow::bail!(
                    "enrollment requires exactly one debug probe attached ({} seen) — plug in only the \
                     board you mean to enroll, or specify which probe by serial",
                    probes.len()
                );
            }
            probes.into_iter().next().expect("checked len == 1 above")
        }
    };
    let serial = info.serial_number.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "the attached probe ({}) reports no USB serial number — it can't be enrolled \
             without one to key on",
            info.identifier
        )
    })?;

    let mut probe = info.open().context("failed to open the attached debug probe")?;
    check_target_powered(&mut probe).context("can't enroll")?;
    let mut session = probe
        .attach(chip, Permissions::default())
        .with_context(|| format!("failed to attach to '{chip}'"))?;
    let mut core = session.core(0).context("failed to select core 0")?;
    let hardware_id = hardware_id::read(&mut core, chip)?;
    drop(core);
    drop(session);

    // Re-enrolling under the same probe_serial replaces the whole row
    // (`enrollment::upsert`'s own doc comment) — carry over any
    // already-declared `link_port_serial` rather than silently dropping it,
    // since it's an independent fact this call has nothing to say about.
    let link_port_serial =
        enrollment::find(&serial).ok().flatten().and_then(|b| b.link_port_serial);

    let board = EnrolledBoard {
        probe_serial: serial,
        role: role.to_string(),
        chip: chip.to_string(),
        hardware_id,
        confirmed_at_utc_ms: enrollment::now_utc_ms(),
        link_port_serial,
    };
    enrollment::upsert(board.clone())?;
    Ok(board)
}
