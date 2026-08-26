//! Auto-detection of `embarch-dev-bench`'s serial port — formerly
//! `embarch-core`'s own `dev_bench.rs`, moved here unchanged in its VID/
//! product/interface heuristic (design.md §3 decisions 2, 4).
//!
//! **The one real behavior change in the move: no env var overrides.**
//! `EMBARCH_DEV_BENCH_PORT`/`_PRODUCT`/`_SERIAL`/`_INTERFACE` are gone
//! outright (design.md §3 decision 9, retired-but-still-load-bearing) — they
//! were exactly the mechanism that caused the incident this crate exists to
//! prevent (design.md §1). What's left of `EMBARCH_DEV_BENCH_SERIAL`'s old
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

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serialport::{SerialPortInfo, SerialPortType};

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
pub const DEFAULT_PRODUCT_NEEDLE: &str = "jlink";

/// The enrollment `role` treated as "this entry is dev-bench" for
/// [`Filter::resolve`]'s fallback.
pub const DEV_BENCH_ROLE: &str = "dev-bench";

/// One detected serial port, plus whatever USB identity the OS reported for
/// it. Named for dev-bench because that was the only thing this module
/// resolved when it was written; [`SignalLink`](super::signal::SignalLink)'s
/// `Route::Direct` (design.md §3 decision 18) resolves through the same
/// machinery and gets the same shape back, which is why the type now has a
/// neutral name and [`DevBenchPort`] is an alias rather than a second type.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedPort {
    pub port_name: String,
    /// `"segger-vid-match"`, `"espressif-vid-match"`, or `"silabs-vid-match"`
    /// — which rule produced this. No `"env-override"` variant any more
    /// (see this module's own top doc comment).
    pub detected_by: &'static str,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<String>,
    pub product: Option<String>,
    pub interface: Option<u8>,
}

/// What every existing caller (`embarch-core`, this crate's own CLI) calls
/// [`DetectedPort`]. Kept as an alias rather than renamed at every call
/// site: the shape is identical, and "the dev-bench port" is still what
/// [`detect`] specifically returns.
pub type DevBenchPort = DetectedPort;

fn detected_by_for_vid(vid: u16) -> &'static str {
    match vid {
        SEGGER_VID => "segger-vid-match",
        ESPRESSIF_VID => "espressif-vid-match",
        SILABS_VID => "silabs-vid-match",
        _ => "vid-match", // unreachable given how candidates are filtered
    }
}

/// No port matched. Distinct from every other detection failure so callers
/// can treat "dev-bench isn't plugged in" (a normal, expected state)
/// differently from "the heuristic is ambiguous" (a real configuration
/// problem) — `embarch-core`'s `api.rs` maps this one to `404`.
#[derive(Debug)]
pub struct NotFound {
    pub candidate_vid_ports_seen: usize,
    pub total_ports_seen: usize,
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no embarch-dev-bench serial port found ({} serial port(s) visible, {} with a recognized link VID ({SEGGER_VID:#06x} SEGGER / {SILABS_VID:#06x} Silicon Labs — {ESPRESSIF_VID:#06x} Espressif's native USB-Serial/JTAG is JTAG-only, see that constant's own doc comment))",
            self.total_ports_seen, self.candidate_vid_ports_seen
        )?;
        if self.candidate_vid_ports_seen > 0 {
            write!(
                f,
                " — a matching probe/board is attached but enrollment's dev-bench-role fallback \
                 excluded it; re-enroll dev-bench (`embarch-topology enroll --role dev-bench`) \
                 with only its own probe attached"
            )?;
        } else {
            write!(
                f,
                " — check dev-bench's USB connection (and `usbipd attach`, if Core and the board \
                 are on different hosts)"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for NotFound {}

/// The narrowing rules applied on top of the VID match. No env vars feed
/// this any more — the only source for `serial`/`serial_is_fallback` is
/// [`Filter::resolve`]'s enrollment lookup.
#[derive(Debug, Default, Clone)]
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
    /// design.md §3 decision 18): the serial already identifies exactly one
    /// device, so gating on VID could only ever exclude the right answer —
    /// a DUT signal may perfectly well land on an FTDI or CH340 bridge
    /// nobody has taught this module about.
    pub no_vid_gate: bool,
}

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
        let (serial, serial_is_fallback) = match enrollment::find_by_role(DEV_BENCH_ROLE) {
            Ok(Some(board)) => match board.link_port_serial {
                Some(link_serial) => (Some(link_serial), false),
                None => (Some(board.probe_serial), true),
            },
            Ok(None) => (None, true),
            Err(e) => {
                tracing::warn!(
                    "failed to read enrollment while resolving dev-bench's serial fallback, \
                     continuing without it: {e:?}"
                );
                (None, true)
            }
        };

        Ok(Self {
            serial,
            product_needle: Some(DEFAULT_PRODUCT_NEEDLE.to_string()),
            product_needle_is_default: true,
            serial_is_fallback,
            interface: None,
            no_vid_gate: false,
        })
    }

    /// Narrows to exactly one declared USB serial, with no VID or
    /// product-string gate — [`super::signal`]'s `Route::Direct` resolution
    /// (design.md §3 decision 18). A declared serial is a fact a human read
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
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn as_candidate(info: &SerialPortInfo) -> Option<DetectedPort> {
    let SerialPortType::UsbPort(usb) = &info.port_type else {
        return None;
    };

    Some(DetectedPort {
        port_name: info.port_name.clone(),
        detected_by: detected_by_for_vid(usb.vid),
        vendor_id: Some(usb.vid),
        product_id: Some(usb.pid),
        serial_number: usb.serial_number.clone(),
        product: usb.product.clone(),
        interface: usb.interface,
    })
}

/// Applies the VID + serial/product/interface rules to an already-enumerated
/// port list. Split out from [`detect`] so the whole heuristic is
/// unit-testable with no hardware involved.
pub fn select(ports: &[SerialPortInfo], filter: &Filter) -> Result<DetectedPort> {
    let mut candidates: Vec<DetectedPort> = ports
        .iter()
        .filter_map(as_candidate)
        .filter(|c| {
            filter.no_vid_gate || matches!(c.vendor_id, Some(SEGGER_VID) | Some(SILABS_VID))
        })
        .collect();
    let candidate_vid_ports_seen = candidates.len();

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
            // matches something; a non-match leaves `candidates` untouched.
            if !narrowed.is_empty() {
                candidates = narrowed;
            }
        } else {
            candidates = narrowed;
        }
    }
    if let Some(needle) = &filter.product_needle {
        candidates.retain(|c| {
            (filter.product_needle_is_default && c.vendor_id != Some(SEGGER_VID))
                || c.product
                    .as_deref()
                    .is_none_or(|p| normalize(p).contains(needle))
        });
    }
    if let Some(interface) = filter.interface {
        candidates.retain(|c| c.interface == Some(interface));
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

        tracing::warn!(
            "{} VCOM interfaces on one J-Link ({:?}) match; using the lowest interface index ({}).\n{}",
            candidates.len(),
            candidates[0].serial_number,
            candidates[0].port_name,
            describe(&candidates)
        );
    }

    if candidates.is_empty() {
        return Err(anyhow::Error::new(NotFound {
            candidate_vid_ports_seen,
            total_ports_seen: ports.len(),
        }));
    }

    Ok(candidates.remove(0))
}

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
/// short-circuits this any more (design.md §3 decisions 3, 9).
///
/// Blocking (`serialport::available_ports` is synchronous, and so is the
/// enrollment file read `Filter::resolve` does) — callers on an async
/// runtime should run this via `spawn_blocking`, same as `embarch-core`
/// already does for every other hardware-touching call.
pub fn detect() -> Result<DevBenchPort> {
    let filter = Filter::resolve()?;
    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    select(&ports, &filter)
}

/// What [`enumerate`] reports as a port's provenance: nothing narrowed it,
/// the OS simply listed it. Deliberately not one of
/// [`detected_by_for_vid`]'s answers — those name the rule that *selected* a
/// port, and an unfiltered listing applied no rule at all.
pub const ENUMERATED: &str = "enumerated";

/// Every serial port the OS currently enumerates that reports a USB
/// identity, with no VID gate and no narrowing — the list a human picks from
/// when declaring a [`Route::Direct`](super::signal::Route::Direct) signal's
/// carrier (`embarch-ui/design.md` §3 decision 10).
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
pub fn enumerate() -> Result<Vec<DetectedPort>> {
    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    Ok(enumerate_in(&ports))
}

/// [`enumerate`]'s pure half, split out for the same reason [`select`] is:
/// the shape of the answer is testable with no hardware attached.
pub fn enumerate_in(ports: &[SerialPortInfo]) -> Vec<DetectedPort> {
    let mut out: Vec<DetectedPort> = ports
        .iter()
        .filter_map(as_candidate)
        .map(|mut p| {
            p.detected_by = ENUMERATED;
            p
        })
        .collect();
    out.sort_by(|a, b| a.port_name.cmp(&b.port_name));
    out
}

#[cfg(test)]
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
}
