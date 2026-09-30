//! **A DUT's two USB serial identities for bootloading**: which port its
//! application's shell enumerates as, and which one its bootloader does.
//! Decision 36; the cross-repo design is `embarch-doc/bootload-proposal.md`.
//!
//! **Both are declared, never detected.** Nothing observable over USB says
//! that a `2fe3:000c` CDC ACM device is this DUT's MCUboot rather than any
//! other Zephyr board's, and a DUT built from Zephyr's defaults enumerates
//! under a VID:PID thousands of unrelated boards share. So an engineer says
//! what their firmware enumerates as, the same posture as dev-bench's
//! `link_port_serial` (decision 20), and this module only ever matches live
//! ports against that.
//!
//! **Its own table in `enrollment.toml`, not two more fields on
//! [`EnrolledBoard`](super::EnrolledBoard).** A bootloadable DUT may have no
//! probe and no enrolled row at all, and every struct literal of that type
//! across three other repos would have had to change for a fact none of them
//! read.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::port::DetectedPort;

/// One USB serial port, by identity. `serial` and `interface` narrow it when
/// VID:PID alone matches more than one port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsbPortId {
    pub vid: u16,
    pub pid: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<u8>,
}

impl UsbPortId {
    pub fn matches(&self, port: &DetectedPort) -> bool {
        port.vendor_id == Some(self.vid)
            && port.product_id == Some(self.pid)
            && self.serial.as_ref().is_none_or(|s| port.serial_number.as_deref() == Some(s.as_str()))
            && self.interface.is_none_or(|i| port.interface == Some(i))
    }

    /// Whether one physical port could match both — the case in which "which
    /// of the two is enumerated" cannot be answered by looking.
    pub fn overlaps(&self, other: &UsbPortId) -> bool {
        fn compatible<T: PartialEq>(a: &Option<T>, b: &Option<T>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
        }
        self.vid == other.vid
            && self.pid == other.pid
            && compatible(&self.serial, &other.serial)
            && compatible(&self.interface, &other.interface)
    }
}

/// `VID:PID` or `VID:PID:SERIAL`, VID and PID in hex with or without `0x`.
/// The interface is never part of the string; it is its own field wherever
/// one is taken.
impl FromStr for UsbPortId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().splitn(3, ':');
        let hex = |part: Option<&str>, what: &str| -> Result<u16, String> {
            let part = part.filter(|p| !p.is_empty()).ok_or_else(|| format!("`{s}` has no {what}; expected VID:PID[:SERIAL]"))?;
            let digits = part.trim_start_matches("0x").trim_start_matches("0X");
            u16::from_str_radix(digits, 16).map_err(|_| format!("{what} `{part}` in `{s}` is not a 16-bit hex number"))
        };
        let vid = hex(parts.next(), "VID")?;
        let pid = hex(parts.next(), "PID")?;
        let serial = parts.next().filter(|p| !p.is_empty()).map(str::to_string);
        Ok(UsbPortId { vid, pid, serial, interface: None })
    }
}

impl fmt::Display for UsbPortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vid, self.pid)?;
        if let Some(serial) = &self.serial {
            write!(f, ":{serial}")?;
        }
        if let Some(interface) = self.interface {
            write!(f, " (interface {interface})")?;
        }
        Ok(())
    }
}

/// A role's declared bootload ports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootloadPorts {
    pub role: String,
    /// Where a declared entry command goes. `None` means no command can be
    /// sent, so only a DUT already sitting in its bootloader can be
    /// bootloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<UsbPortId>,
    pub bootloader: UsbPortId,
}

impl BootloadPorts {
    /// Refuses a pair whose two identities one port could match both of
    /// (the proposal's first open question, closed here). Inferring which is
    /// running from disappear-and-reappear timing would be a guess about the
    /// DUT, and the fix — a distinct bootloader PID — is the firmware's.
    pub fn check(&self) -> Result<(), String> {
        if let Some(app) = &self.app {
            if app.overlaps(&self.bootloader) {
                return Err(format!(
                    "the application ({app}) and the bootloader ({}) could be the same port, so nothing \
                     could tell which one is running; give the bootloader its own USB PID, or declare \
                     serial numbers or interfaces that differ",
                    self.bootloader
                ));
            }
        }
        Ok(())
    }
}

/// More than one live port matched a declared identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguousUsbPort {
    pub id: UsbPortId,
    pub matches: Vec<DetectedPort>,
}

impl fmt::Display for AmbiguousUsbPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.matches.iter().map(|p| p.port_name.as_str()).collect();
        write!(
            f,
            "{} matches {} ports ({}); declare its USB serial number or interface to say which",
            self.id,
            names.len(),
            names.join(", ")
        )
    }
}

impl std::error::Error for AmbiguousUsbPort {}

/// The one port among `ports` matching `id`. `Ok(None)` when it is not
/// enumerated — a normal answer while a device is resetting, which is when
/// a caller polls this.
pub fn find_in(ports: &[DetectedPort], id: &UsbPortId) -> Result<Option<DetectedPort>, AmbiguousUsbPort> {
    let matches: Vec<DetectedPort> = ports.iter().filter(|p| id.matches(p)).cloned().collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(AmbiguousUsbPort { id: id.clone(), matches }),
    }
}

#[cfg(feature = "hardware")]
mod store {
    use anyhow::{bail, Result};

    use super::super::enrollment::{self, DUT_ROLE};
    use super::{find_in, BootloadPorts, UsbPortId};
    use crate::hardware::port::{self, DetectedPort};

    /// Enumerates live ports and matches `id` against them. Blocking.
    pub fn find(id: &UsbPortId) -> Result<Option<DetectedPort>> {
        Ok(find_in(&port::enumerate()?, id)?)
    }

    pub fn get(role: &str) -> Result<Option<BootloadPorts>> {
        Ok(enrollment::load_store()?.bootload_ports.into_iter().find(|p| p.role == role))
    }

    /// Insert or replace `role`'s declaration. **Only the DUT is
    /// bootloaded**; dev-bench firmware goes on by probe, and a declaration
    /// under any other name would be one nothing reads. No enrolled row is
    /// required: a USB-only DUT may have none.
    pub fn declare(ports: BootloadPorts) -> Result<()> {
        if ports.role != DUT_ROLE {
            bail!("only the `{DUT_ROLE}` role is bootloaded; `{}` is not", ports.role);
        }
        ports.check().map_err(anyhow::Error::msg)?;
        let mut store = enrollment::load_store()?;
        store.bootload_ports.retain(|p| p.role != ports.role);
        store.bootload_ports.push(ports);
        enrollment::save_store(&store)
    }

    /// Removes `role`'s declaration. `Ok(false)` if there was none.
    pub fn clear(role: &str) -> Result<bool> {
        let mut store = enrollment::load_store()?;
        let before = store.bootload_ports.len();
        store.bootload_ports.retain(|p| p.role != role);
        if store.bootload_ports.len() == before {
            return Ok(false);
        }
        enrollment::save_store(&store)?;
        Ok(true)
    }
}

#[cfg(feature = "hardware")]
pub use store::{clear, declare, find, get};

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, vid: u16, pid: u16, serial: Option<&str>, interface: Option<u8>) -> DetectedPort {
        DetectedPort {
            port_name: name.to_string(),
            detected_by: "enumerated".to_string(),
            vendor_id: Some(vid),
            product_id: Some(pid),
            serial_number: serial.map(str::to_string),
            product: None,
            interface,
            guessed_among: None,
        }
    }

    fn id(s: &str) -> UsbPortId {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_prints() {
        assert_eq!(id("2fe3:000c"), UsbPortId { vid: 0x2fe3, pid: 0x000c, serial: None, interface: None });
        assert_eq!(id("0x2FE3:0x4:ABC:def").serial.as_deref(), Some("ABC:def"));
        let mut with_iface = id("2fe3:4:SN1");
        with_iface.interface = Some(2);
        assert_eq!(with_iface.to_string(), "2fe3:0004:SN1 (interface 2)");
        for bad in ["", "2fe3", "2fe3:", "zz:1", "12345:1"] {
            assert!(bad.parse::<UsbPortId>().is_err(), "{bad}");
        }
    }

    #[test]
    fn finds_one_none_or_names_the_ambiguity() {
        let ports = [
            port("COM3", 0x2fe3, 0x0004, None, Some(0)),
            port("COM4", 0x2fe3, 0x000c, None, Some(0)),
            port("COM5", 0x2fe3, 0x0004, Some("B"), Some(0)),
        ];
        assert_eq!(find_in(&ports, &id("2fe3:000c")).unwrap().unwrap().port_name, "COM4");
        assert_eq!(find_in(&ports, &id("1915:000c")).unwrap(), None);
        let err = find_in(&ports, &id("2fe3:0004")).unwrap_err();
        assert_eq!(err.matches.len(), 2);
        assert!(err.to_string().contains("COM3, COM5"), "{err}");
        assert_eq!(find_in(&ports, &id("2fe3:0004:B")).unwrap().unwrap().port_name, "COM5");
    }

    #[test]
    fn a_pair_one_port_could_match_both_of_is_refused() {
        let pair = |app: &str, boot: &str| BootloadPorts { role: "dut".into(), app: Some(id(app)), bootloader: id(boot) };
        assert!(pair("2fe3:0004", "2fe3:000c").check().is_ok());
        assert!(pair("2fe3:0004", "2fe3:0004").check().is_err());
        // No serial on one side still overlaps: a port with serial A matches both.
        assert!(pair("2fe3:0004", "2fe3:0004:A").check().is_err());
        assert!(pair("2fe3:0004:A", "2fe3:0004:B").check().is_ok());
        let mut split = pair("2fe3:0004", "2fe3:0004");
        split.app.as_mut().unwrap().interface = Some(0);
        split.bootloader.interface = Some(2);
        assert!(split.check().is_ok());
        let no_app = BootloadPorts { role: "dut".into(), app: None, bootloader: id("2fe3:0004") };
        assert!(no_app.check().is_ok());
    }

    #[cfg(feature = "hardware")]
    #[test]
    fn the_table_round_trips_and_an_older_store_still_loads() {
        use crate::hardware::enrollment::Store;
        let mut store = Store::default();
        store.bootload_ports.push(BootloadPorts { role: "dut".into(), app: Some(id("2fe3:0004")), bootloader: id("2fe3:000c:SN") });
        let text = toml::to_string_pretty(&store).unwrap();
        let back: Store = toml::from_str(&text).unwrap();
        assert_eq!(back.bootload_ports, store.bootload_ports);

        let older: Store = toml::from_str("boards = []\nsignals = []\n").unwrap();
        assert!(older.bootload_ports.is_empty());
        assert!(!toml::to_string_pretty(&older).unwrap().contains("bootload"));
    }

    #[test]
    fn serializes_without_the_absent_narrowing_fields() {
        let json = serde_json::to_string(&id("2fe3:000c")).unwrap();
        assert_eq!(json, r#"{"vid":12259,"pid":12}"#);
        assert_eq!(serde_json::from_str::<UsbPortId>(&json).unwrap(), id("2fe3:000c"));
    }
}
