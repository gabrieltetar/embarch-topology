//! DUT signal links — design.md §3 decision 18.
//!
//! Until this existed, this crate modelled **boards and their probes/links,
//! but nothing about a signal that leaves a board.** It could say "this
//! hardware_id is enrolled as `dut`" and "dev-bench's link is this USB
//! serial." It could not say "the DUT has a UART, and here is where it
//! currently goes" — so a signal that skips a node had no representation at
//! all, and a topology diagram drawn from this crate's data could not show
//! one.
//!
//! A [`SignalLink`] is a **declared** fact, stored alongside
//! [`EnrolledBoard`](super::enrollment::EnrolledBoard) in this crate's own
//! enrollment storage, for the same reason enrollment itself is (decision
//! 14): no detection can produce it, because a wire between two headers is
//! invisible to software.
//!
//! **[`Route`] is the load-bearing field and the whole reason this exists.**
//! The intended topology for a DUT signal is DUT -> dev-bench -> Core:
//! dev-bench already owns a validated link to Core, and is the node that
//! could eventually correlate a DUT signal against its own BLE view. The
//! bench simply does not have the spare pins or the pass-through firmware
//! yet, so today's outpost UART goes to a standalone USB-UART bridge on the
//! Core machine instead. Recording that as a **declared route** rather than
//! as "the temporary way it happens to be wired" is what makes the eventual
//! move a one-field change plus dev-bench firmware — not a redesign of
//! anything that consumes it, and not a study re-authoring (a `Study` names
//! the signal, never the carrier: `embarch-study-designer/design.md` §3
//! decision 39).
//!
//! **Scope is deliberately narrow, matching decisions 10/11:** signals are
//! an extensible table, not a hardcoded list, but there is no logic here for
//! a signal fanning out to two destinations, for a signal between two DUTs,
//! or for driving a `HostToDut` stimulus line. None of those are real yet.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::enrollment;
use super::port::{self, DetectedPort, Filter};

/// A named signal originating at a board, with a declared route that may
/// deliberately bypass dev-bench (design.md §3 decision 18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalLink {
    /// What a `Study` names when it taps this signal
    /// (`embarch-study-designer/design.md` §4.8's
    /// `StreamSource::Signal { name }`). Unique within the table.
    pub name: String,
    /// The enrollment role the signal comes out of — `"dut"` for the
    /// outpost's UART. Not validated against
    /// [`enrollment`](super::enrollment) here: a wire can be declared before
    /// the board it leaves is enrolled, and refusing that would just move
    /// the ordering problem around.
    pub origin_role: String,
    pub direction: SignalDirection,
    pub route: Route,
}

/// Which way a signal travels. The outpost is [`DutToHost`](Self::DutToHost)
/// and TX-only (`embarch-outpost/design.md` §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalDirection {
    DutToHost,
    HostToDut,
    Bidirectional,
}

/// Where a signal currently goes (design.md §3 decision 18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum Route {
    /// Straight to a serial port on the Core machine, **bypassing dev-bench
    /// entirely** — what the outpost uses today, for a stated hardware
    /// reason rather than a design preference.
    ///
    /// `port_serial` is the bridge's own USB serial, resolved by the same
    /// [`Filter`] machinery decision 17 built for dev-bench's link. It is
    /// declared for exactly the reason `link_port_serial` is: a `Direct`
    /// route adds a **third** VID-matching serial device to a bench that
    /// already had two, and the resolution failure without a declared serial
    /// is decision 17's ambiguity with one candidate louder.
    Direct { port_serial: String },
    /// Terminates on declared dev-bench pins; dev-bench relays it over its
    /// existing Core link, **passing bytes through and interpreting
    /// nothing** (`embarch-outpost/design.md` §3 decision 11).
    ///
    /// Not resolvable to a port by this crate — the carrier is dev-bench's
    /// own already-resolved link. Nothing on this bench has the pins for it
    /// yet, which is why `Direct` is what the outpost uses today.
    ViaDevBench { rx_pin: String, tx_pin: String },
}

/// A declared signal isn't where it says it is — the same structured,
/// downcastable idiom [`TopologyMismatch`](super::validate::TopologyMismatch)
/// and [`NotFound`](super::port::NotFound) already establish for this
/// crate's "no guessing" errors (design.md §3 decision 12).
///
/// **Not written to `alerts.jsonl`**, unlike a board mismatch, and that is a
/// deliberate gap rather than an oversight: [`Alert`](super::alert::Alert)'s
/// shape is board-specific (`chip`, `recorded_hardware_id`,
/// `live_hardware_id`), and a wire has none of those — filling three fields
/// with empty strings that a UI would render as facts is precisely the
/// silent-mislabelling this crate exists to prevent. Recording signal
/// mismatches durably wants the alert record to grow a subject
/// discriminator first; see design.md §5.
#[derive(Debug)]
pub struct SignalMismatch {
    pub name: String,
    pub origin_role: String,
    /// The USB serial the `Direct` route declares, `None` for a
    /// `ViaDevBench` route.
    pub declared_port_serial: Option<String>,
    pub reason: String,
}

impl std::fmt::Display for SignalMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "signal-mismatch: signal '{}' (from role '{}') — {}",
            self.name, self.origin_role, self.reason
        )
    }
}

impl std::error::Error for SignalMismatch {}

/// No signal is declared under this name — a normal, expected state (nothing
/// has been wired yet), not a bug. Downcastable so a caller can answer `404`
/// rather than `500`, same as
/// [`NotEnrolled`](super::validate::NotEnrolled).
#[derive(Debug)]
pub struct SignalNotDeclared {
    pub name: String,
}

impl std::fmt::Display for SignalNotDeclared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no signal declared under the name '{}' — declare it first via \
             embarch_topology::hardware::declare_signal, since a wire between two headers \
             is invisible to software and can only ever be stated",
            self.name
        )
    }
}

impl std::error::Error for SignalNotDeclared {}

/// Every declared signal.
pub fn list() -> Result<Vec<SignalLink>> {
    Ok(enrollment::load_store()?.signals)
}

/// Look up one declared signal by name. `Ok(None)` is a normal "not declared
/// yet" outcome, not an error.
pub fn find(name: &str) -> Result<Option<SignalLink>> {
    Ok(enrollment::load_store()?.signals.into_iter().find(|s| s.name == name))
}

/// Insert or replace the signal named `link.name` — declaring is idempotent,
/// re-declaring the same signal overwrites its row rather than accumulating
/// stale duplicates that could disagree about where the wire goes. That
/// overwrite *is* the migration path decision 18 promises: moving the
/// outpost onto dev-bench pins is one call here, not a redesign.
pub fn declare(link: SignalLink) -> Result<()> {
    if link.name.trim().is_empty() {
        anyhow::bail!("a signal needs a name; it is what a Study taps it by");
    }
    let mut store = enrollment::load_store()?;
    store.signals.retain(|s| s.name != link.name);
    store.signals.push(link);
    enrollment::save_store(&store)
}

/// Removes a declared signal. `Ok(false)` if nothing was declared under that
/// name — removing something that isn't there isn't a failure.
pub fn remove(name: &str) -> Result<bool> {
    let mut store = enrollment::load_store()?;
    let before = store.signals.len();
    store.signals.retain(|s| s.name != name);
    if store.signals.len() == before {
        return Ok(false);
    }
    enrollment::save_store(&store)?;
    Ok(true)
}

/// Resolves a `Direct` signal to the serial port currently carrying it, live,
/// on every call — no cached answer, same construction as
/// [`port::detect`](super::port::detect) (design.md §3 decisions 3, 9).
///
/// Blocking (`serialport::available_ports` is synchronous, as is the
/// enrollment-file read) — callers on an async runtime should run this via
/// `spawn_blocking`, same as every other hardware-touching call here.
///
/// A `ViaDevBench` route has no port of its own to resolve: its carrier is
/// dev-bench's own link, already resolved by
/// [`port::detect`](super::port::detect). Asking for one returns a
/// [`SignalMismatch`] naming that rather than guessing at dev-bench's port
/// on the caller's behalf.
pub fn resolve_port(name: &str) -> Result<DetectedPort> {
    let link = find(name)?
        .ok_or_else(|| anyhow::Error::new(SignalNotDeclared { name: name.to_string() }))?;
    resolve_link_port(&link)
}

fn resolve_link_port(link: &SignalLink) -> Result<DetectedPort> {
    let port_serial = match &link.route {
        Route::Direct { port_serial } => port_serial,
        Route::ViaDevBench { .. } => {
            return Err(anyhow::Error::new(SignalMismatch {
                name: link.name.clone(),
                origin_role: link.origin_role.clone(),
                declared_port_serial: None,
                reason: "its route is via-dev-bench, so it has no port of its own — it arrives \
                         relayed over dev-bench's existing link, resolved by \
                         resolve_dev_bench_port()"
                    .to_string(),
            }))
        }
    };

    let ports = serialport::available_ports().context("failed to enumerate serial ports")?;
    port::select(&ports, &Filter::for_declared_serial(port_serial)).map_err(|e| {
        anyhow::Error::new(SignalMismatch {
            name: link.name.clone(),
            origin_role: link.origin_role.clone(),
            declared_port_serial: Some(port_serial.clone()),
            reason: format!(
                "its declared bridge (USB serial '{port_serial}') is not currently enumerable: \
                 {e}"
            ),
        })
    })
}

/// Confirms a declared signal is where it says it is, **before an operation
/// that needs it** — the signal-side counterpart of
/// [`validate_role`](super::validate_role) (design.md §3 decision 18).
///
/// **What this can honestly assert, stated rather than implied.** For a
/// `Direct` route it confirms the declared `port_serial` is currently
/// enumerable. It **cannot** confirm that the wire from the DUT's TX pin
/// actually lands on that bridge — the same structural limit dev-bench's own
/// link already has, now in a second place. A signal that is declared,
/// resolved, and enumerable can still be physically unplugged at the DUT
/// end, and nothing observable over USB says so.
///
/// A `ViaDevBench` route validates to `Ok` on the strength of being
/// declared: its carrier is dev-bench's link, whose own liveness is
/// [`validate_role`](super::validate_role)'s job, not this one's. Asserting
/// anything more would be re-checking dev-bench through a second, weaker
/// path.
pub fn validate(name: &str) -> Result<SignalLink> {
    let link = find(name)?
        .ok_or_else(|| anyhow::Error::new(SignalNotDeclared { name: name.to_string() }))?;
    match link.route {
        Route::Direct { .. } => {
            resolve_link_port(&link)?;
            Ok(link)
        }
        Route::ViaDevBench { .. } => Ok(link),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::{SerialPortInfo, SerialPortType, UsbPortInfo};

    fn usb(port_name: &str, vid: u16, product: Option<&str>, serial: Option<&str>) -> SerialPortInfo {
        SerialPortInfo {
            port_name: port_name.to_string(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 0x6001,
                serial_number: serial.map(str::to_string),
                manufacturer: None,
                product: product.map(str::to_string),
                interface: None,
            }),
        }
    }

    fn outpost_link() -> SignalLink {
        SignalLink {
            name: "outpost".to_string(),
            origin_role: "dut".to_string(),
            direction: SignalDirection::DutToHost,
            route: Route::Direct { port_serial: "FT9ABCDE".to_string() },
        }
    }

    #[test]
    fn a_declared_serial_resolves_a_bridge_no_vid_rule_would_have_kept() {
        // The concrete case decision 18 flags: a DUT signal may land on any
        // USB-UART bridge, including one whose VID this module has never
        // heard of. The declared serial is the fact; gating it on VID could
        // only ever exclude the right answer.
        let ports = vec![
            usb("COM13", port::SILABS_VID, Some("CP210x"), Some("D607104B")),
            usb("COM5", port::SEGGER_VID, Some("JLink CDC UART Port"), Some("000852006107")),
            usb("COM21", 0x0403, Some("FT232R USB UART"), Some("FT9ABCDE")),
        ];
        let found = port::select(&ports, &Filter::for_declared_serial("FT9ABCDE")).unwrap();
        assert_eq!(found.port_name, "COM21");
    }

    #[test]
    fn a_declared_serial_narrows_past_the_third_device_the_bypass_adds() {
        // Decision 17's real ambiguity, one candidate louder: three
        // candidates on the bench, and the declared serial picks exactly one.
        let ports = vec![
            usb("COM13", port::SILABS_VID, Some("CP210x"), Some("D607104B")),
            usb("COM5", port::SEGGER_VID, Some("JLink CDC UART Port"), Some("000852006107")),
            usb("COM21", port::SILABS_VID, Some("CP210x"), Some("BRIDGE-2")),
        ];
        assert_eq!(
            port::select(&ports, &Filter::for_declared_serial("BRIDGE-2")).unwrap().port_name,
            "COM21"
        );
        // And an undeclared serial resolves to nothing rather than to
        // whichever candidate happened to sort first.
        assert!(port::select(&ports, &Filter::for_declared_serial("NOT-HERE")).is_err());
    }

    #[test]
    fn dev_benchs_own_resolution_still_gates_on_vid() {
        // `no_vid_gate` must not leak into the dev-bench path: with nothing
        // declared, the VID filter is the only thing standing between
        // "dev-bench's link" and every USB serial device on the machine.
        assert!(!Filter::default().no_vid_gate);
        let ports = vec![usb("COM21", 0x0403, Some("FT232R USB UART"), Some("FT9ABCDE"))];
        let err = port::select(&ports, &Filter::default()).unwrap_err();
        assert!(err.downcast_ref::<port::NotFound>().is_some());
    }

    #[test]
    fn a_via_dev_bench_route_reports_that_it_has_no_port_of_its_own() {
        let link = SignalLink {
            route: Route::ViaDevBench { rx_pin: "P0.04".to_string(), tx_pin: "P0.05".to_string() },
            ..outpost_link()
        };
        let err = resolve_link_port(&link).unwrap_err();
        let mismatch = err.downcast_ref::<SignalMismatch>().expect("SignalMismatch");
        assert_eq!(mismatch.declared_port_serial, None);
        assert!(mismatch.reason.contains("via-dev-bench"));
    }

    #[test]
    fn a_signal_link_round_trips_through_the_enrollment_files_toml() {
        // Signals share `enrollment.toml` with enrolled boards, so the two
        // tables have to coexist in one file without either erasing the
        // other.
        let link = outpost_link();
        let toml = toml::to_string_pretty(&Store { signals: vec![link.clone()] }).unwrap();
        let back: Store = toml::from_str(&toml).unwrap();
        assert_eq!(back.signals, vec![link]);

        let relayed = SignalLink {
            route: Route::ViaDevBench { rx_pin: "P0.04".to_string(), tx_pin: "P0.05".to_string() },
            ..outpost_link()
        };
        let toml = toml::to_string_pretty(&Store { signals: vec![relayed.clone()] }).unwrap();
        let back: Store = toml::from_str(&toml).unwrap();
        assert_eq!(back.signals, vec![relayed]);
    }

    #[test]
    fn an_enrollment_file_predating_signals_still_loads() {
        let toml = r#"
            [[boards]]
            probe_serial = "D0:CF:13:ED:F9:30"
            role = "dev-bench"
            chip = "esp32c5"
            hardware_id = "13edf930fffed0cf"
            confirmed_at_utc_ms = 1787528352457
        "#;
        let store: super::super::enrollment::Store = toml::from_str(toml).unwrap();
        assert!(store.signals.is_empty());
        assert_eq!(store.boards.len(), 1);
    }

    /// Signals-only view of the real store, for the round-trip above.
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Store {
        signals: Vec<SignalLink>,
    }
}
