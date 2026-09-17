//! Live board-identity validation — formerly `embarch-core`'s own
//! `board_gate.rs` (`embarch-core` decision 22). One implementation,
//! multiple call sites (decision 8): `embarch-core`'s
//! `hardware::flash`/`reset` and `study.rs`'s dev-bench handshake call
//! exactly the functions here, and so does `embarch-topology`'s own CLI —
//! there is no second, independently-reasoned copy of this logic anywhere in
//! the suite to disagree with it.
//! (The local web UI this crate's binary used to also serve is retired,
//! 2026-08-24 — decision 5.)
//!
//! Fails closed in every branch — an unenrolled or now-mismatched probe
//! blocks the operation entirely, never a guess. **Every constructed
//! [`TopologyMismatch`] is durably logged** (`alert.rs`) before the
//! structured error is even returned, so the record exists regardless of
//! what the caller does with the `Err` it gets back — that is decision 12's
//! claim, and it holds. **It is not every *failure* in this gate**:
//! `validate_known_timed`'s probe-open, `check_target_powered`, `attach`,
//! core-select and hardware-ID-read steps each fail closed (the operation is
//! still blocked) but return a plain `anyhow::Error` on the way, not a
//! [`TopologyMismatch`] — un-logged and not downcastable, unlike the two
//! branches that call [`raise`] (probe absent from `Lister::list_all()`, and
//! a hardware-ID compare that doesn't match). The live push that used to
//! accompany the alert log was retired 2026-08-25 (decision 19) —
//! `embarch-ui` polls the same log through `embarch-core`'s `GET /alerts`
//! instead.
//!
//! **What this does not close on its own** (decision 8's own
//! "real gap" note): confirming the enrolled
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
//! (`embarch-core` decision 35), and this crate supplies the
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
/// against a real incident (`embarch-core` decision 26).
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
/// fix-it URL (decision 12), the same idiom
/// [`super::port::NotFound`] already established for "no guessing" errors
/// in this crate.
#[derive(Debug)]
pub struct TopologyMismatch {
    pub role: String,
    pub probe_serial: String,
    pub chip: String,
    pub recorded_hardware_id: String,
    /// `None` when the enrolled probe isn't currently attached at all —
    /// not found in `Lister::list_all()` — a mismatch either way, just not
    /// one with a live hardware ID to show. **Not** the "probe is attached
    /// but `.open()` itself fails" case (another process holding it,
    /// permission denied, a half-wedged J-Link): that path returns a plain
    /// `anyhow::Error` straight out of `validate_known_timed`, never
    /// reaches this type, and logs no alert — see the module header.
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
/// expected state (decision 7's "declared facts can be unset"),
/// not a bug. Downcastable so a caller — `embarch-core`'s new `POST
/// /validate` (`embarch-core` decision 28) — can tell it apart from a genuine
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

/// A live `validate` call's full result: the enrolled record — whose own
/// `confirmed_at_utc_ms` names *enrolment* time, unmoving until someone
/// re-enrolls — paired with `validated_at_utc_ms`, the instant *this* live
/// check ran and succeeded. Added alongside [`validate_serial`]/
/// [`validate_role`] rather than in place of them (topology decision 26):
/// those two keep returning a bare [`EnrolledBoard`] so `embarch-core`'s
/// existing call sites (`hardware::flash`/`reset`, the dev-bench handshake)
/// go on compiling unchanged — this crate is linked live, in-process, not
/// over the wire, so a signature change here is a same-instant break for
/// every consumer, not a rollout. [`validate_serial_timed`]/
/// [`validate_role_timed`] are the opt-in for a caller that wants the
/// second timestamp, starting with this crate's own CLI.
#[derive(Debug, Clone, Serialize)]
pub struct Validation {
    /// The enrolled record `validate` re-checked. Its own
    /// `confirmed_at_utc_ms` is enrolment time, not this check.
    pub board: EnrolledBoard,
    /// UTC milliseconds since the epoch, read the instant this live check's
    /// hardware-ID compare passed — i.e. *now*, not when the record was
    /// made.
    pub validated_at_utc_ms: u64,
}

fn validate_known_timed(known: EnrolledBoard) -> Result<(EnrolledBoard, u64)> {
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

    // Read *after* the compare above passes — this names the instant the
    // live check succeeded, not when the attach/read attempt merely began.
    let validated_at_utc_ms = enrollment::now_utc_ms();
    Ok((known, validated_at_utc_ms))
}

/// Validate by the probe's own USB serial number — `embarch-core`'s
/// `hardware::flash`/`reset` path, once it has already resolved which
/// attached probe a call means.
pub fn validate_serial(serial: &str) -> Result<EnrolledBoard> {
    validate_serial_timed(serial).map(|v| v.board)
}

/// Same live check as [`validate_serial`], additionally reporting when it
/// ran (topology decision 26) — see [`Validation`].
pub fn validate_serial_timed(serial: &str) -> Result<Validation> {
    let known = enrollment::find(serial)?.with_context(|| {
        format!(
            "probe '{serial}' is not enrolled — enroll it first (`embarch-topology enroll`), \
             with only this board's probe attached"
        )
    })?;
    let (board, validated_at_utc_ms) = validate_known_timed(known)?;
    Ok(Validation { board, validated_at_utc_ms })
}

/// Validate by enrollment `role` rather than by serial — for a link that
/// isn't itself a probe-rs-recognized debug probe at all (dev-bench's UART
/// bridge chip; see `super::port`'s own doc comment). `embarch-core`'s
/// dev-bench handshake calls this before ever opening the link.
pub fn validate_role(role: &str) -> Result<EnrolledBoard> {
    validate_role_timed(role).map(|v| v.board)
}

/// Same live check as [`validate_role`], additionally reporting when it ran
/// (topology decision 26) — see [`Validation`].
pub fn validate_role_timed(role: &str) -> Result<Validation> {
    let known = enrollment::find_by_role(role)?
        .ok_or_else(|| anyhow::Error::new(NotEnrolled { role: role.to_string() }))?;
    let (board, validated_at_utc_ms) = validate_known_timed(known)?;
    Ok(Validation { board, validated_at_utc_ms })
}

/// The rule both [`enroll`] below and `embarch-core::resolve_probe`
/// (`embarch-core/src/hardware.rs`) apply to pick one attached debug probe
/// out of everything `probe-rs` currently enumerates: an explicit
/// `probe_serial` finds that one probe or fails naming it; omitted, exactly
/// one attached probe is required — the only sane default when there is no
/// other way to tell which one a caller means.
///
/// Takes the already-enumerated list rather than calling [`Lister`] itself
/// (topology decision 33) — the difference between a rule that can have a
/// unit test and one that cannot, since neither of the two copies this
/// reconciles was testable before. `action` is a present-tense verb naming
/// what the caller is about to do with the picked probe (`"enroll"`,
/// `"flash"`, `"reset"`, …): it appears in the multi-probe refusal below, so
/// a caller keeps its own flavor of that message without a second copy of
/// this function — decision 33's own note on why one fixed wording did not
/// win outright.
///
/// Zero probes is checked first and unconditionally, ahead of the serial
/// lookup: it is a stronger, more specific diagnosis than "no probe matches
/// that serial" regardless of whether a serial was given, and it is the one
/// case worth naming the likely cause of (a real incident behind the usbipd
/// hint — decision 33). A serial that was given but not found is echoed
/// back either way, so the caller isn't left guessing which case fired.
pub fn select_probe(
    probes: Vec<probe_rs::probe::DebugProbeInfo>,
    probe_serial: Option<&str>,
    action: &str,
) -> Result<probe_rs::probe::DebugProbeInfo> {
    if probes.is_empty() {
        return Err(match probe_serial {
            Some(wanted) => anyhow::anyhow!(
                "no debug probe found (looking for serial '{wanted}') — check the USB \
                 connection (and usbipd attach, if Core is on a Pi and the probe is elsewhere)"
            ),
            None => anyhow::anyhow!(
                "no debug probe found — check the USB connection (and usbipd attach, if Core \
                 is on a Pi and the probe is elsewhere)"
            ),
        });
    }

    if let Some(wanted) = probe_serial {
        return probes.into_iter().find(|p| p.serial_number.as_deref() == Some(wanted)).ok_or_else(|| {
            anyhow::anyhow!("no attached probe with serial '{wanted}' — is it still plugged in?")
        });
    }

    if probes.len() > 1 {
        let known: Vec<String> = probes
            .iter()
            .map(|p| format!("{} (serial={:?})", p.identifier, p.serial_number))
            .collect();
        anyhow::bail!(
            "{action} requires exactly one debug probe attached ({} seen) — plug in only the \
             board you mean to {action}, or specify which probe by serial. Attached probes: \
             {known:?}",
            probes.len()
        );
    }

    Ok(probes.into_iter().next().expect("checked len == 1 above"))
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
/// isolate them one at a time just to satisfy this function — motivated by
/// `embarch-core`'s own `GET /enroll` page's drag-and-drop UI, since retired
/// in favor of `embarch-ui`'s Enroll surface (`embarch-core` decision 25)
/// (decision 15). Omitted, the original
/// behavior is unchanged: refuses anything but exactly one attached probe,
/// the only sane default when there's no other way to tell which one a
/// caller means.
///
/// **This used to be a hand-copy of `embarch-core::resolve_probe`
/// (`embarch-core/src/hardware.rs`, `pub(crate)`), not a call to it.**
/// The two were one implementation before `embarch-core`
/// decision 22 moved the board-identity gate into this crate; `pub(crate)`
/// cannot cross the crate boundary that move created, so the move silently
/// turned one shared implementation into two independently maintained
/// copies (decision 32). The selection rule became [`select_probe`]
/// above, a `pub` function this crate exposes specifically so
/// `embarch-core` can call it instead of keeping its own copy (decision
/// 33) — and as of `embarch-core` decision 61, it does: `resolve_probe`
/// now enumerates probes itself and delegates to this function, threading
/// its own caller `action` (`"flash"`/`"reset"`) through the same way
/// `enroll` does. **Decision 32 is closed; no selection-rule copy remains
/// in either crate.**
///
/// **This still doesn't — and structurally can't — verify that the probe a
/// human *picked* really is the board they think it is.** Serial number and
/// probe identifier are exactly what a same-probe-type ambiguity (decision
/// 15's own flagged risk: two boards sharing an identical probe type, e.g.
/// two J-Links) leaves nothing to tell apart by. `enroll`'s own live hardware-ID
/// readback below still catches a *wrong chip name* for the picked probe;
/// it can't catch "right chip, wrong physical board" when both boards
/// genuinely are that chip. That case still needs physical isolation — no
/// UI can enroll around it.
pub fn enroll(role: &str, chip: &str, probe_serial: Option<&str>) -> Result<EnrolledBoard> {
    let lister = Lister::new();
    let probes = lister.list_all();
    let info = select_probe(probes, probe_serial, "enroll")?;
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
    // already-declared link-port facts rather than silently dropping them,
    // since they're independent facts this call has nothing to say about.
    // Keyed on the *probe serial*, deliberately: a role moving to different
    // silicon must NOT inherit them, because they describe the old board's
    // physical USB link and nothing about the new one.
    let prior = enrollment::find(&serial).ok().flatten();
    let link_port_serial = prior.as_ref().and_then(|b| b.link_port_serial.clone());
    let link_port_interface = prior.and_then(|b| b.link_port_interface);

    let board = EnrolledBoard {
        probe_serial: serial,
        role: role.to_string(),
        chip: chip.to_string(),
        hardware_id,
        confirmed_at_utc_ms: enrollment::now_utc_ms(),
        link_port_serial,
        link_port_interface,
    };
    // A role is unique (`enrollment::upsert`'s own doc comment). Moving one
    // onto different silicon is a legitimate thing to do — this bench's
    // dev-bench role has now been an nRF54L15DK, an ESP32-C5, and an
    // nRF54L15DK again — but it is never a thing to do *quietly*: the board
    // that just lost the role is no longer reachable by the only name
    // anything in this suite addresses it by.
    if let Some(displaced) = enrollment::upsert(board.clone())? {
        tracing::warn!(
            "role '{}' moved from probe {} (chip {}, hardware_id {}) to probe {} (chip {}, \
             hardware_id {}); the old board is no longer enrolled under any role",
            board.role,
            displaced.probe_serial,
            displaced.chip,
            displaced.hardware_id,
            board.probe_serial,
            board.chip,
            board.hardware_id,
        );
    }
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: "serial".into(),
            role: "dev-bench".into(),
            chip: "nRF54L15".into(),
            hardware_id: "6fcddc36cb781b71".into(),
            confirmed_at_utc_ms: 1_788_195_194_573,
            link_port_serial: None,
            link_port_interface: None,
        }
    }

    /// The whole point of [`Validation`] is that a caller can tell the two
    /// timestamps apart: the enrolled record's own `confirmed_at_utc_ms`
    /// (enrolment time) stays nested under `board`, distinct from the new
    /// top-level `validated_at_utc_ms` (this live check's time) — not
    /// flattened into one namespace where the field name is the only thing
    /// separating them.
    #[test]
    fn validation_keeps_the_two_timestamps_distinct_in_json() {
        let v = Validation { board: board(), validated_at_utc_ms: 1_788_723_019_911 };
        let json = serde_json::to_value(&v).expect("Validation must serialize");
        let board_confirmed_at = json["board"]["confirmed_at_utc_ms"]
            .as_u64()
            .expect("board.confirmed_at_utc_ms must be present");
        let validated_at =
            json["validated_at_utc_ms"].as_u64().expect("top-level validated_at_utc_ms must be present");
        assert_eq!(board_confirmed_at, 1_788_195_194_573);
        assert_eq!(validated_at, 1_788_723_019_911);
        assert_ne!(board_confirmed_at, validated_at, "a real regression this guards against");
    }

    // `select_probe` needs a `&'static dyn ProbeFactory` to build a fake
    // `DebugProbeInfo` (its one private field) — any concrete factory works,
    // since these tests never call `.open()`. `JLinkFactory` is a public
    // zero-sized type, so a `static` of it coerces to the trait object with
    // no unsafe code.
    static TEST_FACTORY: probe_rs::probe::jlink::JLinkFactory = probe_rs::probe::jlink::JLinkFactory;

    fn probe(identifier: &str, serial: Option<&str>) -> probe_rs::probe::DebugProbeInfo {
        probe_rs::probe::DebugProbeInfo::new(
            identifier,
            0x1366,
            0x0101,
            serial.map(String::from),
            &TEST_FACTORY,
            None,
            false,
        )
    }

    #[test]
    fn select_probe_zero_probes_names_the_usb_connection() {
        let err = select_probe(vec![], None, "enroll").expect_err("no probes must refuse");
        assert!(err.to_string().contains("no debug probe found"));
        assert!(err.to_string().contains("usbipd"), "the one diagnostic hint both callers should keep");
    }

    #[test]
    fn select_probe_zero_probes_with_a_serial_still_names_the_usb_connection() {
        let err = select_probe(vec![], Some("S1"), "flash").expect_err("no probes must refuse even with a serial");
        let msg = err.to_string();
        assert!(msg.contains("no debug probe found"), "zero probes is diagnosed before the serial lookup runs");
        assert!(msg.contains("S1"), "the requested serial is still echoed back");
    }

    #[test]
    fn select_probe_exactly_one_is_accepted_without_a_serial() {
        let picked = select_probe(vec![probe("only", Some("S1"))], None, "enroll")
            .expect("exactly one attached probe must be accepted");
        assert_eq!(picked.serial_number.as_deref(), Some("S1"));
    }

    #[test]
    fn select_probe_two_probes_without_a_serial_refuses_and_names_the_action() {
        let probes = vec![probe("a", Some("S1")), probe("b", Some("S2"))];
        let err = select_probe(probes, None, "enroll").expect_err("more than one attached probe must refuse");
        let msg = err.to_string();
        assert!(msg.contains("enroll requires exactly one debug probe attached (2 seen)"));
        assert!(msg.contains("S1") && msg.contains("S2"), "both attached probes are named, not just counted");
    }

    #[test]
    fn select_probe_serial_hit_picks_the_matching_probe_among_several() {
        let probes = vec![probe("a", Some("S1")), probe("b", Some("S2"))];
        let picked =
            select_probe(probes, Some("S2"), "flash").expect("a serial matching an attached probe must resolve");
        assert_eq!(picked.identifier, "b");
    }

    #[test]
    fn select_probe_serial_miss_names_the_serial_it_could_not_find() {
        let err = select_probe(vec![probe("a", Some("S1"))], Some("does-not-exist"), "flash")
            .expect_err("a serial matching nothing attached must refuse");
        assert!(err.to_string().contains("no attached probe with serial 'does-not-exist'"));
    }
}
