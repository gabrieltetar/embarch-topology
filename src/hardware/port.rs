//! Auto-detection of `embarch-dev-bench`'s serial port — formerly
//! `embarch-core`'s own `dev_bench.rs`, moved here unchanged in its VID/
//! product/interface heuristic (decisions 2, 4).
//!
//! **The one real behavior change in the move: no env var overrides.**
//! `EMBARCH_DEV_BENCH_PORT`/`_PRODUCT`/`_SERIAL`/`_INTERFACE` are gone
//! outright (decision 9, retired-but-still-load-bearing) — they
//! were exactly the mechanism that caused the incident this crate exists to
//! prevent (spec.md). What's left of `EMBARCH_DEV_BENCH_SERIAL`'s old
//! job — disambiguating dev-bench from some other SEGGER-VID device on the
//! same bench — is covered by [`enrollment`](super::enrollment)'s dev-bench-
//! role fallback (the enrolled JTAG probe's own serial) *when* dev-bench's
//! link and its JTAG probe are the same physical USB device. On real
//! hardware where they aren't — dev-bench's link moved to its own UART
//! bridge chip, this module's own `SILABS_VID` doc comment — that fallback
//! can never match anything, and a second SEGGER-VID device on the bench
//! (e.g. a DUT's own separate J-Link) is then indistinguishable from dev-
//! bench's real link by VID/product alone. `EnrolledBoard::link_port_serial`
//! is the fix: a second declared fact — the link port's own USB serial, set
//! once via `enrollment::set_link_port_serial` — that [`Filter::resolve`]
//! prefers, hard, over the JTAG-probe-serial fallback whenever it's present.
//! Found live, 2026-08-24: enrolling a real DUT (its own J-Link) alongside
//! an already-enrolled dev-bench (link on a Silabs bridge) made [`select`]
//! genuinely ambiguous between the DUT's J-Link VCOM and dev-bench's real
//! link — a gap no prior session had exercised, since none had both a
//! JTAG-capable DUT and a Silabs-bridge dev-bench enrolled at once.
//!
//! No hardware is opened here — this only reads USB descriptors already
//! enumerated by the OS. Actually opening the port and running a link's own
//! handshake is each consumer's job (`embarch-core`'s `study.rs`, e.g.); this
//! module just answers "which port is it?".

#[cfg(feature = "hardware")]
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(feature = "hardware")]
use serialport::{SerialPortInfo, SerialPortType};

#[cfg(feature = "hardware")]
use super::enrollment;

/// SEGGER's USB vendor ID — every on-board J-Link (and every standalone one)
/// enumerates its VCOM interfaces under this VID.
pub const SEGGER_VID: u16 = 0x1366;

/// Espressif Systems' USB vendor ID. **Not a [`select`] link candidate** —
/// the ESP32-C5's native USB-Serial/JTAG peripheral turned out to be a bad
/// fit for dev-bench's runtime link (a core-only reset that doesn't
/// re-sample boot-strapping pins; a hardware reset that wedges the host USB
/// CDC driver outright; a DTR/RTS-on-open gotcha reproducing the same wedge
/// on demand — all real, documented Espressif/`probe-rs` silicon quirks, not
/// bugs here). Kept defined because JTAG flashing/reset still use this exact
/// port through the hardware crate's own probe enumeration — a wholly
/// separate code path from this module's serial-port detection.
pub const ESPRESSIF_VID: u16 = 0x303A;

/// Silicon Labs' USB vendor ID — the CP210x-family USB-to-UART bridge chip
/// on dev-bench's second, dedicated UART USB-C port. Unlike the other two
/// VIDs here, this chip has no JTAG/debug capability at all, so its own
/// serial can never be an enrollment candidate — see [`Filter::resolve`]'s
/// doc comment on why the dev-bench-role fallback is scoped away from
/// narrowing this VID's candidates.
pub const SILABS_VID: u16 = 0x10C4;

/// Default product-string needle, in `normalize`d form. Matches both
/// Linux's bare `J-Link` and Windows' `JLink CDC UART Port` friendly name.
#[cfg(feature = "hardware")]
pub const DEFAULT_PRODUCT_NEEDLE: &str = "jlink";

/// The enrollment `role` treated as "this entry is dev-bench" for
/// [`Filter::resolve`]'s fallback.
pub const DEV_BENCH_ROLE: &str = "dev-bench";

/// One detected serial port, plus whatever USB identity the OS reported for
/// it. Named for dev-bench because that was the only thing this module
/// resolved when it was written; [`SignalLink`](super::signal::SignalLink)'s
/// `Route::Direct` (decision 18) resolves through the same
/// machinery and gets the same shape back, which is why the type now has a
/// neutral name and [`DevBenchPort`] is an alias rather than a second type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedPort {
    pub port_name: String,
    /// One of four values. `"segger-vid-match"` or `"silabs-vid-match"` when
    /// [`select`] ran with its VID gate on (`Filter::no_vid_gate` false —
    /// dev-bench's own resolution, decision 17) and that gate is what stood
    /// between the candidate and every other serial device on the machine —
    /// [`ESPRESSIF_VID`] is not one of the two the gate admits (its own doc
    /// comment says why), so `"espressif-vid-match"` is never actually
    /// produced; [`detected_by_for_vid`] still returns it for that VID
    /// because the gate, not this function, is what excludes Espressif, and
    /// the string stays a faithful name for the value if that ever changes.
    /// [`DECLARED_SERIAL`] when the gate was off instead
    /// (`Filter::for_declared_serial`, decision 18) — a directly declared USB
    /// serial identified the candidate, and any VID at all was eligible.
    /// [`ENUMERATED`] when [`enumerate`] ran and nothing narrowed the
    /// candidate at all. No `"env-override"` variant any more (see this
    /// module's own top doc comment).
    ///
    /// **This names which of those three regimes resolved the port, not
    /// which individual comparison happened to eliminate the last other
    /// candidate** — see `embarch-topology` decision 24. Within the
    /// VID-gated regime, a declared serial or interface can still do the
    /// real narrowing among several same-vendor candidates; the VID rule is
    /// still credited because it is what excluded every other vendor, which
    /// remains true regardless of what narrowed the rest. Under the
    /// `no_vid_gate` regime the VID played no discriminating role at all
    /// (any vendor was eligible), so it is never credited there — including
    /// when the declared serial happens to also be a known-VID device.
    ///
    /// `String` rather than `&'static str` since decision 31: this type is
    /// now deserializable behind the `wire` feature, and a borrowed-static
    /// field cannot be. Every value it holds is still one of the four
    /// constants below.
    pub detected_by: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub interface: Option<u8>,
    /// How many equally-plausible ports this one was **guessed** from, when
    /// nothing declared could narrow them — `None` when the answer was
    /// actually determined rather than picked.
    ///
    /// This exists because the guess used to be invisible: [`select`] logged
    /// a `WARN` and returned a port that looked exactly like a confident
    /// answer, and `GET /dev-bench/port` reported it as one. On the
    /// nRF54L15DK the guess is wrong (its console UART is on the *higher*
    /// interface), and the resulting failure — a bench that flashes, boots
    /// and answers nothing — says nothing about a port having been chosen at
    /// all. A caller that can see this field can say "COM16, guessed among
    /// 2" instead of "COM16".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guessed_among: Option<usize>,
}

/// What every existing caller (`embarch-core`, this crate's own CLI) calls
/// [`DetectedPort`]. Kept as an alias rather than renamed at every call
/// site: the shape is identical, and "the dev-bench port" is still what
/// [`detect`] specifically returns.
pub type DevBenchPort = DetectedPort;

/// What `Filter::for_declared_serial`-resolved [`select`] reports for
/// [`DetectedPort::detected_by`]: a declared USB serial, not a VID rule,
/// identified this port (decision 18; `embarch-topology` decision 24 on why
/// this is a distinct value rather than a VID-match string or [`ENUMERATED`]).
pub const DECLARED_SERIAL: &str = "declared-serial";

#[cfg(feature = "hardware")]
fn detected_by_for_vid(vid: u16) -> &'static str {
    match vid {
        SEGGER_VID => "segger-vid-match",
        ESPRESSIF_VID => "espressif-vid-match",
        SILABS_VID => "silabs-vid-match",
        // Not reachable today: `select` only ever calls this with a VID that
        // passed its own gate (one of the three above) when that gate is on,
        // and overwrites the result with `DECLARED_SERIAL` when it's off;
        // `enumerate_in` overwrites it with `ENUMERATED` unconditionally. Kept
        // as a named fallback rather than a `panic!`/`unreachable!` so a
        // future caller that does neither gets an honest-if-generic answer
        // instead of one of those.
        _ => "vid-match",
    }
}

/// Which narrowing rule is the one that left zero candidates —
/// `embarch-topology` decision 27. Named so [`NotFound`]'s `Display` can send
/// the operator to the fix that can actually work, instead of always
/// printing the one generic "re-enroll" remedy regardless of which fact
/// actually excluded everything (the failure decision 20 records: a stale
/// declared `link_port_serial` hard-narrows detection to a port that no
/// longer exists, and re-enrolling by role — the old advice — carries that
/// same stale fact right back over, per `validate::enroll`'s own doc
/// comment on why it's keyed on probe serial).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcludingRule {
    /// The VID gate excluded every candidate — no port among however many
    /// the OS enumerated (`NotFound::total_ports_seen`) reported one of the
    /// two VIDs the gate admits ([`SEGGER_VID`], [`SILABS_VID`]).
    /// [`ESPRESSIF_VID`] is a recognized *link* VID for JTAG/flashing but not
    /// a candidate this gate ever admits (its own doc comment says why), so
    /// it can't be the reason this variant fires.
    NoRecognizedVid,
    /// A declared serial ([`EnrolledBoard::link_port_serial`](super::enrollment::EnrolledBoard::link_port_serial),
    /// applied hard — `serial_is_fallback: false`) matched no VID-recognized
    /// candidate. The JTAG-probe-serial *fallback* can never cause this: it
    /// only ever narrows when it actually matches something (`select`'s own
    /// comment on why a fallback mismatch leaves `candidates` untouched).
    DeclaredSerial,
    /// A declared [`EnrolledBoard::link_port_interface`](super::enrollment::EnrolledBoard::link_port_interface)
    /// matched no candidate remaining after the VID and serial rules.
    DeclaredInterface,
    /// Something else excluded every remaining candidate (the default
    /// product-string needle, most likely, or a fallback serial that
    /// happened to leave nothing once combined with the other rules) — the
    /// original, generic "re-enroll dev-bench" advice, unchanged, because
    /// there's no single declared fact this can point at clearing.
    Other,
}

/// No port matched. Distinct from every other detection failure so callers
/// can treat "dev-bench isn't plugged in" (a normal, expected state)
/// differently from "the heuristic is ambiguous" (a real configuration
/// problem) — `embarch-core`'s `api.rs` maps this one to `404`.
#[derive(Debug)]
pub struct NotFound {
    pub candidate_vid_ports_seen: usize,
    pub total_ports_seen: usize,
    /// Which rule left the candidate list empty — [`ExcludingRule`]'s own
    /// doc comment.
    pub excluding_rule: ExcludingRule,
    /// Does this process look like it's running inside a WSL2 guest —
    /// `crate::wsl2::detect_here()`'s answer, a fact that costs nothing to
    /// compute (`std`-only, no network) and is available at this exact
    /// construction site (`embarch-topology` decision 27), unlike the live,
    /// network-probed answer `embarch-topology status` gives — that one
    /// needs `software`'s `reqwest`/`tokio`, which `embarch-core`'s
    /// `hardware`-only build (this crate's own `lib.rs` doc comment) never
    /// links, so it cannot be embedded here for every consumer.
    ///
    /// [`select`] itself never sets this to `true` — it's the pure half,
    /// deliberately kept free of any real-machine read (its own doc
    /// comment); only [`detect`], the live wrapper, fills in the real
    /// answer.
    pub likely_wsl2: bool,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.total_ports_seen == 0 && self.likely_wsl2 {
            // The split-host possibility leads, rather than trailing a
            // sentence that already sent the reader to the hardware
            // (`tasks/topology/004`'s own "Measured on the bench" finding):
            // zero ports visible on what looks like a WSL2 guest is far more
            // often "this process is on the wrong host" than "every cable
            // fell out." This can't embed the live-resolved answer
            // `embarch-topology status` would show (see `likely_wsl2`'s own
            // doc comment on why), so it names the check to run instead of
            // asserting a result this call never made.
            return write!(
                f,
                "no embarch-dev-bench serial port found (0 serial port(s) visible) — this looks \
                 like WSL2, and 0 ports visible almost always means Core (and this port scan) is \
                 running on the wrong host, not that every cable fell out: run `embarch-topology \
                 status` to see whether Core resolved on the Windows host instead, then `usbipd \
                 attach` the board from there. If dev-bench really is meant to be reachable from \
                 here, check its USB connection."
            );
        }

        write!(
            f,
            "no embarch-dev-bench serial port found ({} serial port(s) visible, {} with a recognized link VID ({SEGGER_VID:#06x} SEGGER / {SILABS_VID:#06x} Silicon Labs — {ESPRESSIF_VID:#06x} Espressif's native USB-Serial/JTAG is JTAG-only, see that constant's own doc comment))",
            self.total_ports_seen, self.candidate_vid_ports_seen
        )?;
        match self.excluding_rule {
            ExcludingRule::NoRecognizedVid => write!(
                f,
                " — check dev-bench's USB connection (and `usbipd attach`, if Core and the board \
                 are on different hosts)"
            ),
            ExcludingRule::DeclaredSerial => write!(
                f,
                " — the declared link port serial no longer matches any attached port; clear it \
                 with `embarch-topology set-dev-bench-link --clear-serial` (falls back to the \
                 enrolled probe's own serial), or declare the current one with `embarch-topology \
                 set-dev-bench-link --serial <serial>`"
            ),
            ExcludingRule::DeclaredInterface => write!(
                f,
                " — the declared link port interface matches no attached port; clear it with \
                 `embarch-topology set-dev-bench-link --clear-interface`, or declare the current \
                 one with `embarch-topology set-dev-bench-link --interface <n>`"
            ),
            ExcludingRule::Other => write!(
                f,
                " — a matching probe/board is attached but enrollment's dev-bench-role fallback \
                 excluded it; re-enroll dev-bench (`embarch-topology enroll --role dev-bench`) \
                 with only its own probe attached"
            ),
        }
    }
}

impl std::error::Error for NotFound {}

/// The narrowing rules applied on top of the VID match. No env vars feed
/// this any more — the only source for `serial`/`serial_is_fallback` is
/// [`Filter::resolve`]'s enrollment lookup.
#[derive(Debug, Default, Clone)]
#[cfg(feature = "hardware")]
pub struct Filter {
    pub serial: Option<String>,
    pub product_needle: Option<String>,
    pub product_needle_is_default: bool,
    /// Always `true` now that `EMBARCH_DEV_BENCH_SERIAL` is gone — kept as a
    /// field (rather than deleted outright) because [`select`]'s own
    /// asymmetric-fallback logic still depends on the distinction being
    /// *nameable*, even though there's only one source for it left. See its
    /// own doc comment for why a fallback-sourced serial is applied more
    /// cautiously than an explicit one used to be.
    pub serial_is_fallback: bool,
    pub interface: Option<u8>,
    /// Skip [`select`]'s VID pre-filter entirely.
    ///
    /// `false` (the default, and dev-bench's own path) keeps the
    /// SEGGER/Silicon-Labs gate that stands in for "this is plausibly a
    /// bench link at all" when nothing more specific is known. `true` is for
    /// a route whose `port_serial` is a **declared** fact
    /// ([`SignalLink`](super::signal::SignalLink)'s `Route::Direct`,
    /// decision 18): the serial already identifies exactly one
    /// device, so gating on VID could only ever exclude the right answer —
    /// a DUT signal may perfectly well land on an FTDI or CH340 bridge
    /// nobody has taught this module about.
    pub no_vid_gate: bool,
}

#[cfg(feature = "hardware")]
impl Filter {
    /// Always `DEFAULT_PRODUCT_NEEDLE`, with `known_boards`'s successor
    /// (`super::enrollment::find_by_role`) as the only serial source, via the
    /// `DEV_BENCH_ROLE` enrollment (`DEV_BENCH_ROLE`'s own doc comment has
    /// the gap this closes: once a JTAG-capable DUT is attached alongside
    /// dev-bench, VID+product string alone can't tell them apart, but the
    /// exact serial recorded at enrollment can).
    ///
    /// The enrollment lookup itself failing (an unreadable/corrupt file)
    /// degrades to no serial fallback at all, plus a logged warning, rather
    /// than breaking detection entirely over what's meant to be a
    /// convenience default, not a hard requirement.
    ///
    /// Prefers the dev-bench role's declared
    /// [`EnrolledBoard::link_port_serial`](super::enrollment::EnrolledBoard::link_port_serial)
    /// when set — a directly declared fact about the link port itself, so
    /// it narrows *hard* (`serial_is_fallback: false`), same as an explicit
    /// serial always has. Falls back to the JTAG probe's own serial
    /// (`serial_is_fallback: true`, unchanged from before this field
    /// existed) only when no link serial has been declared — the common
    /// case for dev-bench hardware whose link and JTAG probe really are the
    /// same physical device, where the old inference already works.
    pub fn resolve() -> Result<Self> {
        let (serial, serial_is_fallback, interface) = match enrollment::find_by_role(DEV_BENCH_ROLE)
        {
            Ok(Some(board)) => {
                let interface = board.link_port_interface;
                match board.link_port_serial {
                    Some(link_serial) => (Some(link_serial), false, interface),
                    None => (Some(board.probe_serial), true, interface),
                }
            }
            Ok(None) => (None, true, None),
            Err(e) => {
                tracing::warn!(
                    "failed to read enrollment while resolving dev-bench's serial fallback, \
                     continuing without it: {e:?}"
                );
                (None, true, None)
            }
        };

        Ok(Self {
            serial,
            product_needle: Some(DEFAULT_PRODUCT_NEEDLE.to_string()),
            product_needle_is_default: true,
            serial_is_fallback,
            // `EnrolledBoard::link_port_interface`, the second declared fact
            // a two-VCOM probe needs — see [`select`]'s multi-candidate
            // branch for what happens without one.
            interface,
            no_vid_gate: false,
        })
    }

    /// Narrows to exactly one declared USB serial, with no VID or
    /// product-string gate — [`super::signal`]'s `Route::Direct` resolution
    /// (decision 18). A declared serial is a fact a human read
    /// off the actual device, so it narrows hard, the same way
    /// `EnrolledBoard::link_port_serial` does (decision 17).
    pub fn for_declared_serial(serial: &str) -> Self {
        Self {
            serial: Some(serial.to_string()),
            product_needle: None,
            product_needle_is_default: false,
            serial_is_fallback: false,
            interface: None,
            no_vid_gate: true,
        }
    }
}

/// Lowercase, alphanumerics only — lets one default needle cover every
/// spelling of the same probe across platforms.
#[cfg(feature = "hardware")]
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(feature = "hardware")]
fn as_candidate(info: &SerialPortInfo) -> Option<DetectedPort> {
    let SerialPortType::UsbPort(usb) = &info.port_type else {
        return None;
    };

    Some(DetectedPort {
        port_name: info.port_name.clone(),
        detected_by: detected_by_for_vid(usb.vid).to_string(),
        vendor_id: Some(usb.vid),
        product_id: Some(usb.pid),
        serial_number: usb.serial_number.clone(),
        product: usb.product.clone(),
        interface: usb.interface,
        guessed_among: None,
    })
}

/// Applies the VID + serial/product/interface rules to an already-enumerated
/// port list. Split out from [`detect`] so the whole heuristic is
/// unit-testable with no hardware involved.
#[cfg(feature = "hardware")]
pub fn select(ports: &[SerialPortInfo], filter: &Filter) -> Result<DetectedPort> {
    let mut candidates: Vec<DetectedPort> = ports
        .iter()
        .filter_map(as_candidate)
        .filter(|c| {
            filter.no_vid_gate || matches!(c.vendor_id, Some(SEGGER_VID) | Some(SILABS_VID))
        })
        .map(|mut c| {
            // With the VID gate off, no VID rule stood between this
            // candidate and any other vendor's device — a declared serial
            // did that job (`Filter::for_declared_serial`), so it, not
            // `detected_by_for_vid`'s answer, is the honest provenance.
            // Overwritten here rather than in `as_candidate`, which has no
            // `Filter` to consult (`embarch-topology` decision 24).
            if filter.no_vid_gate {
                c.detected_by = DECLARED_SERIAL.to_string();
            }
            c
        })
        .collect();
    let candidate_vid_ports_seen = candidates.len();
    // First cause wins: once this is `Some`, every later branch's own
    // `is_none()` guard leaves it alone, and a filter step on an
    // already-empty `Vec` can only ever keep it empty — so the rule
    // recorded here is genuinely the one that emptied the list, in the
    // order the rules actually apply.
    let mut excluding_rule = if candidates.is_empty() {
        Some(ExcludingRule::NoRecognizedVid)
    } else {
        None
    };

    if let Some(serial) = &filter.serial {
        let want = normalize(serial);
        let narrowed: Vec<DetectedPort> = candidates
            .iter()
            .filter(|c| c.serial_number.as_deref().map(normalize) == Some(want.clone()))
            .cloned()
            .collect();

        if filter.serial_is_fallback {
            // A fallback-sourced serial is a JTAG probe's serial, only
            // guaranteed to equal a link candidate's own serial when the
            // link and the debug probe are the same physical USB device —
            // not true once dev-bench's link moved to a separate USB-UART
            // bridge chip (SILABS_VID). Apply it only when it actually
            // matches something; a non-match leaves `candidates` untouched
            // — so this branch can never be the rule that empties the list.
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        } else {
            candidates = narrowed;
            if candidates.is_empty() && excluding_rule.is_none() {
                excluding_rule = Some(ExcludingRule::DeclaredSerial);
            }
        }
    }
    if let Some(needle) = &filter.product_needle {
        candidates.retain(|c| {
            (filter.product_needle_is_default && c.vendor_id != Some(SEGGER_VID))
                || c.product
                    .as_deref()
                    .is_none_or(|p| normalize(p).contains(needle))
        });
        if candidates.is_empty() && excluding_rule.is_none() {
            excluding_rule = Some(ExcludingRule::Other);
        }
    }
    if let Some(interface) = filter.interface {
        candidates.retain(|c| c.interface == Some(interface));
        if candidates.is_empty() && excluding_rule.is_none() {
            excluding_rule = Some(ExcludingRule::DeclaredInterface);
        }
    }

    candidates.sort_by(|a, b| {
        a.interface
            .cmp(&b.interface)
            .then_with(|| a.port_name.cmp(&b.port_name))
    });

    if candidates.len() > 1 {
        let one_probe = candidates.iter().all(|c| {
            c.vendor_id == candidates[0].vendor_id && c.serial_number == candidates[0].serial_number
        });
        let interfaces_known = candidates.iter().all(|c| c.interface.is_some());

        if !(one_probe && interfaces_known) {
            bail!(
                "ambiguous embarch-dev-bench detection — {} candidate ports match:\n{}\nif \
                 dev-bench's runtime link is on its own USB device (a UART bridge, separate from \
                 its JTAG probe), declare that port's own serial with `embarch-topology \
                 set-dev-bench-link --serial <serial>`; otherwise re-enroll the intended board's \
                 serial via `embarch-topology enroll --role dev-bench`, or physically disconnect \
                 the other candidate",
                candidates.len(),
                describe(&candidates)
            );
        }

        // **A guess, and it has been wrong on real hardware.** One probe
        // exposing several VCOMs shares a serial across all of them, so
        // neither the enrolled probe serial nor a declared
        // `link_port_serial` can narrow them — the only thing left is the
        // interface index, and until 2026-08-31 nothing could declare one,
        // so this took the lowest. That rule had no evidence behind it (no
        // bench before the nRF54L15DK had two VCOMs) and the DK falsified
        // it: its `zephyr,console` is `uart20`, wired to VCOM1 at interface
        // 2, while interface 0 is a port that accepts bytes and never
        // answers. Declare the right one with
        // `embarch-topology set-dev-bench-link --interface <n>`; the guess
        // stays as the behaviour for a bench that has never needed to.
        tracing::warn!(
            "{} VCOM interfaces on one J-Link ({:?}) match and nothing declared narrows them; \
             GUESSING the lowest interface index ({}). If dev-bench does not answer, this is the \
             first thing to suspect — declare the right one with `embarch-topology \
             set-dev-bench-link --interface <n>`.\n{}",
            candidates.len(),
            candidates[0].serial_number,
            candidates[0].port_name,
            describe(&candidates)
        );
        candidates[0].guessed_among = Some(candidates.len());
    }

    if candidates.is_empty() {
        return Err(anyhow::Error::new(NotFound {
            candidate_vid_ports_seen,
            total_ports_seen: ports.len(),
            excluding_rule: excluding_rule.unwrap_or(ExcludingRule::Other),
            // `select` is the pure half (this fn's own doc comment) — never
            // a real-machine read. `detect` fills in the true answer.
            likely_wsl2: false,
        }));
    }

    Ok(candidates.remove(0))
}

#[cfg(feature = "hardware")]
fn describe(candidates: &[DetectedPort]) -> String {
    candidates
        .iter()
        .map(|c| {
            format!(
                "  {} (pid {:#06x}, serial {:?}, product {:?}, interface {:?})",
                c.port_name,
                c.product_id.unwrap_or(0),
                c.serial_number,
                c.product,
                c.interface
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finds dev-bench's port on this machine, live, on every call — no env var
/// short-circuits this any more (decisions 3, 9).
///
/// Blocking (`serialport::available_ports` is synchronous, and so is the
/// enrollment file read `Filter::resolve` does) — callers on an async
/// runtime should run this via `spawn_blocking`, same as `embarch-core`
/// already does for every other hardware-touching call.
#[cfg(feature = "hardware")]
pub fn detect() -> Result<DevBenchPort> {
    let filter = Filter::resolve()?;
    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    select(&ports, &filter).map_err(|e| match e.downcast::<NotFound>() {
        // The one thing this live wrapper adds over `select`'s own pure
        // result: `likely_wsl2`, a real-machine read `select` deliberately
        // never makes (its own doc comment; `crate::wsl2::detect_here`'s own
        // doc comment on why this is the one call site allowed to do it).
        Ok(mut not_found) => {
            not_found.likely_wsl2 = crate::wsl2::detect_here();
            anyhow::Error::new(not_found)
        }
        Err(e) => e,
    })
}

/// What [`enumerate`] reports as a port's provenance: nothing narrowed it,
/// the OS simply listed it. Deliberately not one of
/// [`detected_by_for_vid`]'s answers — those name the rule that *selected* a
/// port, and an unfiltered listing applied no rule at all.
pub const ENUMERATED: &str = "enumerated";

/// Every serial port the OS currently enumerates that reports a USB
/// identity, with no VID gate and no narrowing — the list a human picks from
/// when declaring a [`Route::Direct`](super::signal::Route::Direct) signal's
/// carrier (`embarch-ui` decision 10).
///
/// **Not [`select`], and not a superset of it.** `select` answers "which port
/// is dev-bench's link", applying the VID gate and every narrowing rule.
/// This answers "what is plugged in", because a `Direct` route's USB-UART
/// bridge is a wire's carrier rather than a recognized device and can carry
/// any VID at all — gating this list by the three link VIDs would hide
/// exactly the port the route exists to name.
///
/// **Ports with no USB identity are omitted, and that is not a gap.** A
/// `Direct` route is declared by `port_serial` and resolved through
/// [`Filter::for_declared_serial`], so a port that reports no USB serial can
/// never be declared as one; listing it would offer a choice nothing could
/// act on.
///
/// Blocking, same as [`detect`] — call via `spawn_blocking` on an async
/// runtime.
#[cfg(feature = "hardware")]
pub fn enumerate() -> Result<Vec<DetectedPort>> {
    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    Ok(enumerate_in(&ports))
}

/// [`enumerate`]'s pure half, split out for the same reason [`select`] is:
/// the shape of the answer is testable with no hardware attached.
#[cfg(feature = "hardware")]
pub fn enumerate_in(ports: &[SerialPortInfo]) -> Vec<DetectedPort> {
    let mut out: Vec<DetectedPort> = ports
        .iter()
        .filter_map(as_candidate)
        .map(|mut p| {
            p.detected_by = ENUMERATED.to_string();
            p
        })
        .collect();
    out.sort_by(|a, b| a.port_name.cmp(&b.port_name));
    out
}

#[cfg(all(test, feature = "hardware"))]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    fn usb(
        port_name: &str,
        vid: u16,
        product: Option<&str>,
        serial: Option<&str>,
        interface: Option<u8>,
    ) -> SerialPortInfo {
        SerialPortInfo {
            port_name: port_name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x0105,
                serial_number: serial.map(str::to_string),
                manufacturer: Some("SEGGER".to_string()),
                product: product.map(str::to_string),
                interface,
            }),
        }
    }

    fn default_filter() -> Filter {
        Filter {
            serial: None,
            product_needle: Some(DEFAULT_PRODUCT_NEEDLE.to_string()),
            product_needle_is_default: true,
            serial_is_fallback: true,
            interface: None,
            no_vid_gate: false,
        }
    }

    #[test]
    fn picks_the_only_segger_port_among_noise() {
        let ports = vec![
            SerialPortInfo {
                port_name: "/dev/ttyS0".to_string(),
                port_type: SerialPortType::Unknown,
            },
            usb("/dev/ttyACM0", 0x0483, Some("STM32 STLink"), None, Some(2)),
            usb(
                "/dev/ttyACM1",
                SEGGER_VID,
                Some("J-Link"),
                Some("760001"),
                Some(0),
            ),
        ];

        let found = select(&ports, &default_filter()).unwrap();
        assert_eq!(found.port_name, "/dev/ttyACM1");
        assert_eq!(found.detected_by, "segger-vid-match");
    }

    #[test]
    fn windows_friendly_name_matches_the_same_default_needle() {
        let ports = vec![usb(
            "COM4",
            SEGGER_VID,
            Some("JLink CDC UART Port"),
            Some("760001"),
            Some(0),
        )];
        assert_eq!(select(&ports, &default_filter()).unwrap().port_name, "COM4");
    }

    #[test]
    fn a_port_reporting_no_product_string_is_not_excluded() {
        let ports = vec![usb("/dev/ttyACM0", SEGGER_VID, None, Some("760001"), Some(0))];
        assert_eq!(
            select(&ports, &default_filter()).unwrap().port_name,
            "/dev/ttyACM0"
        );
    }

    #[test]
    fn absence_is_reported_as_not_found() {
        let ports = vec![usb("/dev/ttyACM0", 0x0483, Some("STM32 STLink"), None, Some(2))];
        let err = select(&ports, &default_filter()).unwrap_err();
        let not_found = err.downcast_ref::<NotFound>().expect("NotFound");
        assert_eq!(not_found.candidate_vid_ports_seen, 0);
        assert_eq!(not_found.total_ports_seen, 1);
        assert_eq!(not_found.excluding_rule, ExcludingRule::NoRecognizedVid);
        assert!(
            !not_found.likely_wsl2,
            "select is the pure half and must never claim a real-machine read"
        );
    }

    /// `tasks/topology/004`'s "Measured on the bench" case: zero ports
    /// visible on what looks like WSL2 must lead with the split-host
    /// possibility, not bury it behind a sentence that already sent the
    /// reader to the hardware. `select` itself never sets `likely_wsl2`
    /// (previous test) — this pins what `Display` does once something else
    /// (`detect`) has.
    #[test]
    fn zero_ports_on_what_looks_like_wsl2_leads_with_split_host() {
        let not_found = NotFound {
            candidate_vid_ports_seen: 0,
            total_ports_seen: 0,
            excluding_rule: ExcludingRule::NoRecognizedVid,
            likely_wsl2: true,
        };
        let msg = format!("{not_found}");
        assert!(
            msg.find("WSL2").unwrap() < msg.find("USB connection").unwrap(),
            "the split-host possibility must come before the cable check: {msg}"
        );
        assert!(msg.contains("embarch-topology status"), "{msg}");
    }

    /// Zero ports and "ports visible but none match a recognized VID" are
    /// different diagnoses (supervisor direction, leg 045) — the latter must
    /// not get the split-host lead-in even when `likely_wsl2` is true,
    /// because a wrong-VID device being plugged in says nothing about which
    /// host the process is on.
    #[test]
    fn ports_visible_but_wrong_vid_does_not_get_the_split_host_lead_in() {
        let not_found = NotFound {
            candidate_vid_ports_seen: 0,
            total_ports_seen: 1,
            excluding_rule: ExcludingRule::NoRecognizedVid,
            likely_wsl2: true,
        };
        let msg = format!("{not_found}");
        assert!(!msg.contains("WSL2"), "{msg}");
        assert!(msg.contains("USB connection"), "{msg}");
    }

    /// Fixture test: a declared link serial that matches nothing routes to
    /// clearing/re-declaring that serial, not to the generic re-enroll
    /// advice that (per decision 20) carries the same stale fact right back.
    #[test]
    fn a_declared_serial_matching_nothing_routes_to_clearing_the_serial() {
        let ports = vec![usb("/dev/ttyACM0", SEGGER_VID, Some("J-Link"), Some("760001"), Some(0))];
        let filter = Filter {
            serial: Some("no-such-serial".to_string()),
            serial_is_fallback: false,
            ..default_filter()
        };
        let err = select(&ports, &filter).unwrap_err();
        let not_found = err.downcast_ref::<NotFound>().expect("NotFound");
        assert_eq!(not_found.excluding_rule, ExcludingRule::DeclaredSerial);
        let msg = format!("{not_found}");
        assert!(msg.contains("set-dev-bench-link --clear-serial"), "{msg}");
    }

    /// Fixture test: a declared link interface that matches nothing routes
    /// to clearing/re-declaring that interface.
    #[test]
    fn a_declared_interface_matching_nothing_routes_to_clearing_the_interface() {
        let ports = vec![usb("/dev/ttyACM0", SEGGER_VID, Some("J-Link"), Some("760001"), Some(0))];
        let filter = Filter { interface: Some(9), ..default_filter() };
        let err = select(&ports, &filter).unwrap_err();
        let not_found = err.downcast_ref::<NotFound>().expect("NotFound");
        assert_eq!(not_found.excluding_rule, ExcludingRule::DeclaredInterface);
        let msg = format!("{not_found}");
        assert!(msg.contains("set-dev-bench-link --clear-interface"), "{msg}");
    }

    /// Fixture test: no candidate VID at all keeps the original generic
    /// USB-connection advice — the third of the three routes the Done-when
    /// asks to be distinguishable from each other.
    #[test]
    fn no_candidate_vid_at_all_keeps_the_generic_usb_advice() {
        let ports = vec![usb("/dev/ttyACM0", 0x0483, Some("STM32 STLink"), None, Some(2))];
        let err = select(&ports, &default_filter()).unwrap_err();
        let not_found = err.downcast_ref::<NotFound>().expect("NotFound");
        assert_eq!(not_found.excluding_rule, ExcludingRule::NoRecognizedVid);
        let msg = format!("{not_found}");
        assert!(msg.contains("check dev-bench's USB connection"), "{msg}");
        assert!(!msg.contains("set-dev-bench-link"), "{msg}");
    }

    #[test]
    fn serial_number_disambiguates_two_probes_via_fallback() {
        let ports = vec![
            usb("/dev/ttyACM0", SEGGER_VID, Some("J-Link"), Some("760001"), Some(0)),
            usb("/dev/ttyACM1", SEGGER_VID, Some("J-Link"), Some("760002"), Some(0)),
        ];

        let err = select(&ports, &default_filter()).unwrap_err();
        assert!(err.downcast_ref::<NotFound>().is_none());
        assert!(format!("{err}").contains("ambiguous"));

        let filter = Filter {
            serial: Some("760002".to_string()),
            ..default_filter()
        };
        assert_eq!(select(&ports, &filter).unwrap().port_name, "/dev/ttyACM1");
    }

    #[test]
    fn two_vcoms_on_one_probe_resolve_to_the_lowest_interface() {
        let ports = vec![
            usb("/dev/ttyACM1", SEGGER_VID, Some("J-Link"), Some("760001"), Some(2)),
            usb("/dev/ttyACM0", SEGGER_VID, Some("J-Link"), Some("760001"), Some(0)),
        ];
        assert_eq!(
            select(&ports, &default_filter()).unwrap().port_name,
            "/dev/ttyACM0"
        );
    }

    #[test]
    fn an_espressif_vid_only_port_is_not_a_link_candidate() {
        let ports = vec![usb("COM12", ESPRESSIF_VID, Some("USB Serial Device"), None, Some(0))];
        let err = select(&ports, &default_filter()).unwrap_err();
        assert_eq!(
            err.downcast_ref::<NotFound>().expect("NotFound").candidate_vid_ports_seen,
            0
        );
    }

    #[test]
    fn a_segger_probe_and_a_silabs_bridge_together_are_ambiguous_with_no_fallback() {
        let ports = vec![
            usb("/dev/ttyACM0", SEGGER_VID, Some("J-Link"), None, Some(0)),
            usb(
                "/dev/ttyACM1",
                SILABS_VID,
                Some("Silicon Labs CP210x USB to UART Bridge"),
                None,
                Some(0),
            ),
        ];
        let err = select(&ports, &default_filter()).unwrap_err();
        assert!(format!("{err}").contains("ambiguous"));
    }

    #[test]
    fn a_declared_link_serial_resolves_the_real_ambiguity_a_dut_probe_introduces() {
        // The exact real-hardware shape found live 2026-08-24: dev-bench's
        // real link (Silabs bridge, COM13) alongside a separately-enrolled
        // DUT's own J-Link, whose VCOM (COM5) shares the JLink product
        // string and so passes the default product-needle filter too.
        let ports = vec![
            usb(
                "COM13",
                SILABS_VID,
                Some("Silicon Labs CP210x USB to UART Bridge"),
                Some("D607104BD96EF0119D5C489B1045C30F"),
                None,
            ),
            usb(
                "COM5",
                SEGGER_VID,
                Some("JLink CDC UART Port"),
                Some("000852006107"), // the DUT's own J-Link, not dev-bench
                Some(0),
            ),
        ];

        // Unresolved (link_port_serial unset): genuinely ambiguous, same as
        // before this fix — the JTAG-probe-serial fallback can't match
        // either candidate, so both remain.
        let unresolved = Filter { serial: None, serial_is_fallback: true, ..default_filter() };
        let err = select(&ports, &unresolved).unwrap_err();
        assert!(format!("{err}").contains("ambiguous"));

        // Declared (serial_is_fallback: false, as `Filter::resolve` now sets
        // when `link_port_serial` is present): hard-narrows to COM13 alone.
        let declared = Filter {
            serial: Some("D607104BD96EF0119D5C489B1045C30F".to_string()),
            serial_is_fallback: false,
            ..default_filter()
        };
        assert_eq!(select(&ports, &declared).unwrap().port_name, "COM13");
    }

    /// The shape this bench actually has as of 2026-08-31, read off the
    /// real hardware. The nRF54L15DK's link is *not* on a separate bridge
    /// chip the way the ESP32-C5's had to be — its console UART goes through
    /// the DK's own onboard J-Link OB — so the enrolled JTAG probe's serial
    /// and the link port's serial are the same string and no
    /// `link_port_serial` is needed. But that same probe exposes **two**
    /// VCOMs under that one serial, so the serial narrows to a pair and
    /// stops; only a declared interface finishes the job. Meanwhile the
    /// separately-enrolled DUT's own J-Link VCOM is excluded by serial
    /// despite matching both VID and product string.
    #[test]
    fn a_two_vcom_probe_needs_a_declared_interface_not_a_declared_serial() {
        let ports = vec![
            usb("COM16", SEGGER_VID, Some("JLink CDC UART Port"), Some("001057729826"), Some(0)),
            usb("COM17", SEGGER_VID, Some("JLink CDC UART Port"), Some("001057729826"), Some(2)),
            usb("COM5", SEGGER_VID, Some("JLink CDC UART Port"), Some("000852006107"), Some(0)),
        ];

        // Undeclared: the probe serial narrows to the pair and no further,
        // so this is a *guess* — and on this DK it is the wrong one. What
        // matters is that it now says so.
        let guessing = Filter {
            serial: Some("001057729826".to_string()),
            serial_is_fallback: true,
            ..default_filter()
        };
        let guessed = select(&ports, &guessing).unwrap();
        assert_eq!(guessed.port_name, "COM16");
        assert_eq!(
            guessed.guessed_among,
            Some(2),
            "a pick among several VCOMs must be reported as a guess, not as an answer"
        );

        // Declared: interface 2 is the DK's `uart20`/VCOM1, the port that
        // actually answers a Hello. This is the whole fix.
        let declared = Filter { interface: Some(2), ..guessing.clone() };
        let found = select(&ports, &declared).unwrap();
        assert_eq!(found.port_name, "COM17");
        assert_eq!(found.detected_by, "segger-vid-match");
        assert_eq!(
            found.guessed_among, None,
            "a declared interface determines the port; nothing was guessed"
        );

        // And with nothing enrolled at all it is genuinely ambiguous across
        // two different probes, which is a hard error rather than any pick.
        let err = select(&ports, &Filter { serial: None, ..default_filter() }).unwrap_err();
        assert!(format!("{err}").contains("ambiguous"), "{err}");
    }

    #[test]
    fn a_fallback_serial_mismatch_does_not_exclude_the_only_candidate() {
        let ports = vec![usb(
            "COM13",
            SILABS_VID,
            Some("Silicon Labs CP210x USB to UART Bridge"),
            Some("D607104BD96EF0119D5C489B1045C30F"),
            Some(0),
        )];
        let filter = Filter {
            serial: Some("D0:CF:13:ED:F9:30".to_string()), // an enrolled JTAG probe's serial
            serial_is_fallback: true,
            ..default_filter()
        };
        assert_eq!(select(&ports, &filter).unwrap().port_name, "COM13");
    }

    #[test]
    fn a_silabs_vid_port_is_picked_without_a_product_string_match() {
        let ports = vec![usb(
            "COM13",
            SILABS_VID,
            Some("Silicon Labs CP210x USB to UART Bridge"),
            Some("D607104BD96EF0119D5C489B1045C30F"),
            Some(0),
        )];
        let found = select(&ports, &default_filter()).unwrap();
        assert_eq!(found.port_name, "COM13");
        assert_eq!(found.detected_by, "silabs-vid-match");
    }

    /// The whole point of `enumerate`: a bridge with a VID none of the three
    /// link constants name is still offered, because a `Route::Direct` wire's
    /// carrier is not a device this crate recognizes.
    #[test]
    fn enumerate_offers_every_usb_port_whatever_its_vid() {
        let ports = vec![
            usb("COM13", SILABS_VID, Some("CP210x"), Some("AAA"), Some(0)),
            usb("COM3", 0x0403, Some("FT232R USB UART"), Some("FTBBB"), None),
            SerialPortInfo { port_name: "COM1".to_string(), port_type: SerialPortType::Unknown },
        ];
        let listed = enumerate_in(&ports);
        let names: Vec<&str> = listed.iter().map(|p| p.port_name.as_str()).collect();
        assert_eq!(
            names,
            vec!["COM13", "COM3"],
            "an unrecognized-VID bridge must be offered, and a port with no USB identity must not"
        );
        assert!(listed.iter().all(|p| p.detected_by == ENUMERATED));
    }

    /// `select` and `enumerate` answer different questions, and this pins
    /// that they do: the same port list narrows to one dev-bench link and
    /// lists two candidates for a human to pick a wire's carrier from.
    #[test]
    fn enumerate_is_not_select_with_the_gate_off() {
        let ports = vec![
            usb("COM13", SILABS_VID, Some("CP210x"), Some("AAA"), Some(0)),
            usb("COM3", 0x0403, Some("FT232R USB UART"), Some("FTBBB"), None),
        ];
        assert_eq!(select(&ports, &default_filter()).unwrap().port_name, "COM13");
        assert_eq!(enumerate_in(&ports).len(), 2);
    }

    /// The exact shape measured live 2026-09-06 (leg 020, `tasks/topology/006`):
    /// three SEGGER candidates on the bench, so the VID rule matched all of
    /// them and narrowed nothing — the declared serial and the declared
    /// interface did the actual work, same as
    /// [`a_two_vcom_probe_needs_a_declared_interface_not_a_declared_serial`].
    /// **Pinned as accepted, not fixed** (`embarch-topology` decision 24):
    /// `detected_by` still credits `"segger-vid-match"` here, because the
    /// VID gate is genuinely on for this (`Filter::resolve`) path and did
    /// exclude every non-SEGGER/Silabs device on the machine — under-selling
    /// how much was pinned down, but not asserting something false, unlike
    /// the `no_vid_gate` case [`DECLARED_SERIAL`] fixes.
    #[test]
    fn a_vid_rule_that_narrowed_nothing_is_still_credited_when_the_gate_ran() {
        let ports = vec![
            usb("COM16", SEGGER_VID, Some("JLink CDC UART Port"), Some("001057729826"), Some(0)),
            usb("COM17", SEGGER_VID, Some("JLink CDC UART Port"), Some("001057729826"), Some(2)),
            usb("COM5", SEGGER_VID, Some("JLink CDC UART Port"), Some("000852006107"), Some(0)),
        ];
        let filter = Filter {
            serial: Some("001057729826".to_string()),
            serial_is_fallback: false,
            interface: Some(2),
            ..default_filter()
        };
        let found = select(&ports, &filter).unwrap();
        assert_eq!(found.port_name, "COM17");
        assert_eq!(
            found.detected_by, "segger-vid-match",
            "the VID gate ran and did exclude every other vendor, so it is credited even though \
             the declared serial and interface did the narrowing among these three"
        );
    }
}
