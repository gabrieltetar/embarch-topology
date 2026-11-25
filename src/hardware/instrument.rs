//! **Bench instruments driven over a USB serial line**: an electronic load,
//! a lab supply, anything that speaks SCPI. Decision 49.
//!
//! **Declared, never detected**, for the same reason as a DUT's bootload ports
//! (decision 36): a USB-UART bridge's VID:PID is shared by every device built
//! on that chip, and some bridges carry no USB serial number at all. So an
//! engineer says which identity is the instrument, and this module only ever
//! matches live ports against that.
//!
//! **The line facts are the instrument's, so they live here, not in every
//! caller.** Which terminator it needs, what it answers, how long it wants
//! between commands and what a set acknowledges with differ by model and are
//! learned the hard way on the bench; declaring them once means a study step
//! and a one-off exchange cannot disagree about them.
//!
//! **Its own `instruments` table in `enrollment.toml`**, beside
//! `bootload_ports`: an instrument has no probe, no role and no enrolled row.
//! The table is skipped when empty, so a store that never declared one is
//! rewritten unchanged.

use serde::{Deserialize, Serialize};

use super::bootload::UsbPortId;
#[cfg(feature = "hardware")]
use super::bootload::find_in;

/// How an instrument is spoken to. One variant today; an enum so a second
/// protocol is a new variant rather than a reinterpretation of this one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstrumentProtocol {
    /// One text line per command; a query's reply is one line back.
    #[default]
    Scpi,
}

/// The longest name an instrument may have: it is a URL path segment.
pub const MAX_NAME_LEN: usize = 64;

/// The longest gap an instrument may ask for between commands. Ten seconds
/// is far past any instrument's need and short enough that a typo cannot
/// park a study step for minutes.
pub const MAX_MIN_INTERVAL_MS: u32 = 10_000;

/// The longest safe-off line, for an instrument or a bench board's signal.
/// Mirrors the write cap Core puts on an exchange's `line` (4096 bytes); this
/// crate cannot depend on Core, so the number is repeated here, assumed equal.
pub const MAX_SAFE_OFF_LINE_LEN: usize = 4096;

/// A safe-off line is checked as an exchange's `line` is: not empty, no CR or
/// LF (the terminator is added by Core), within the write cap. `owner` names
/// the declaration in the message.
pub fn check_safe_off_line(owner: &str, line: &str) -> Result<(), String> {
    if line.is_empty() {
        return Err(format!("{owner}'s safe_off holds an empty line; leave it out instead"));
    }
    if line.contains(['\r', '\n']) {
        return Err(format!("{owner}'s safe_off line holds a CR or LF: one command per line, the terminator is added"));
    }
    if line.len() > MAX_SAFE_OFF_LINE_LEN {
        return Err(format!(
            "{owner}'s safe_off line is {} bytes; at most {MAX_SAFE_OFF_LINE_LEN}",
            line.len()
        ));
    }
    Ok(())
}

fn lf() -> String {
    "\n".to_string()
}

fn crlf() -> String {
    "\r\n".to_string()
}

/// Where a metrology entry's numbers come from: the report prints it beside
/// every value it touches (decision 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MetrologyBasis {
    /// The instrument's datasheet.
    Datasheet,
    /// A calibration certificate, named by the instrument's `calibration`.
    Certificate,
    /// The engineer's own declaration, marked as such.
    Declared,
}

/// The span a metrology entry holds over, in its `unit`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MeasuringRange {
    pub min: f64,
    pub max: f64,
}

/// One quantity an instrument measures, and what one reading of it is worth
/// (decision 51). Nothing in this crate uses the numbers; Core stamps them on
/// the measurements the instrument makes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrology {
    /// What it measures, e.g. `current`.
    pub quantity: String,
    pub unit: String,
    /// The smallest step a reading resolves, in `unit`.
    pub resolution: f64,
    /// Expanded uncertainty `U` of one reading, in `unit`.
    #[serde(rename = "U")]
    pub u_expanded: f64,
    /// Coverage factor of `U`.
    pub k: f64,
    pub basis: MetrologyBasis,
    pub range: MeasuringRange,
}

/// An instrument's calibration, as the person declared it (decision 51).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    /// The serial the certificate names; checked against `*IDN?` by Core.
    pub serial: String,
    pub certificate_ref: String,
    /// The date the calibration runs out, `yyyy-mm-dd`.
    pub due: String,
    /// The certificate file's SHA-256, lower-case hex, when it is attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate_sha256: Option<String>,
}

/// Whether `s` is a real calendar date written `yyyy-mm-dd`.
fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let num = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[r];
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let (Some(y), Some(m), Some(d)) = (num(0..4), num(5..7), num(8..10)) else {
        return false;
    };
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

impl Metrology {
    /// Refuses what no reading could be judged with, naming the field.
    fn check(&self, owner: &str) -> Result<(), String> {
        let at = format!("{owner}'s metrology for `{}`", self.quantity);
        if self.quantity.is_empty() {
            return Err(format!("{owner} has a metrology entry with an empty quantity"));
        }
        if self.unit.is_empty() {
            return Err(format!("{at} has an empty unit"));
        }
        for (field, v) in [("resolution", self.resolution), ("U", self.u_expanded)] {
            if !v.is_finite() || v < 0.0 {
                return Err(format!("{at}: {field} is {v}; it must be a number, zero or more"));
            }
        }
        if !self.k.is_finite() || self.k <= 0.0 {
            return Err(format!("{at}: k is {}; it must be a number above zero", self.k));
        }
        let r = self.range;
        if !r.min.is_finite() || !r.max.is_finite() || r.min >= r.max {
            return Err(format!("{at}: range {} to {} is inverted or empty; min must be below max", r.min, r.max));
        }
        Ok(())
    }
}

impl Calibration {
    fn check(&self, owner: &str) -> Result<(), String> {
        for (field, v) in [("serial", &self.serial), ("certificate_ref", &self.certificate_ref)] {
            if v.is_empty() {
                return Err(format!("{owner}'s calibration has an empty {field}"));
            }
        }
        if !is_iso_date(&self.due) {
            return Err(format!("{owner}'s calibration due `{}` is not a date written yyyy-mm-dd", self.due));
        }
        if let Some(h) = &self.certificate_sha256 {
            if h.len() != 64 || !h.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')) {
                return Err(format!("{owner}'s calibration certificate_sha256 is not 64 lower-case hex digits"));
            }
        }
        Ok(())
    }
}

/// One declared instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    /// What a study step and the exchange route call it.
    pub name: String,
    #[serde(default)]
    pub protocol: InstrumentProtocol,
    /// The USB serial identity it enumerates as.
    pub port: UsbPortId,
    pub baud: u32,
    /// Appended to every line written. An instrument that ignores a line
    /// ended by CR alone needs LF here.
    #[serde(default = "lf")]
    pub write_terminator: String,
    /// What ends one reply line.
    #[serde(default = "crlf")]
    pub read_terminator: String,
    /// The least time between two exchanges with this instrument, measured by
    /// Core from the end of the previous exchange (its reply read, or its
    /// write flushed when nothing is read) to the start of the next write,
    /// process-wide. The flush line sent after opening is an exchange too. So
    /// the gap is never shorter than this and is usually longer by the
    /// instrument's reply time; do not tune it down to allow for that.
    #[serde(default)]
    pub min_interval_ms: u32,
    /// Text a set answers with when it was carried out. With it declared, a
    /// set's reply is read and a set without it is not acknowledged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_ok: Option<String>,
    /// Text a set answers with when it was refused. Recorded as such; any
    /// reply without `ack_ok` is unacknowledged whether or not it holds this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_err: Option<String>,
    /// Lines sent in order, with this instrument's terminators and pacing, to
    /// switch its output off (Core's safe-off at a graceful stop and at start).
    /// Empty when none is declared; Core then lists it as "no safe-off
    /// declared". Absent in every row declared before it existed and omitted
    /// when empty, so a store is rewritten unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub safe_off: Vec<String>,
    /// One entry per quantity it measures (decision 51). Empty when none is
    /// declared and omitted when empty, so a store is rewritten unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrology: Vec<Metrology>,
    /// Its calibration, when declared (decision 51). Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calibration: Option<Calibration>,
}

impl Instrument {
    /// What a declaration must satisfy before it is stored. The port's
    /// identity is already well-formed by type; a name is a path segment.
    pub fn check(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > MAX_NAME_LEN {
            return Err(format!("an instrument name is 1 to {MAX_NAME_LEN} characters; `{}` is not", self.name));
        }
        if !self.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return Err(format!(
                "instrument name `{}` may hold only ASCII letters, digits, `-`, `_` and `.`: it is a URL path segment",
                self.name
            ));
        }
        if self.baud == 0 {
            return Err(format!("instrument `{}` has baud 0", self.name));
        }
        for (what, t) in [("write_terminator", &self.write_terminator), ("read_terminator", &self.read_terminator)] {
            if t.is_empty() || t.len() > 4 {
                return Err(format!("instrument `{}`'s {what} is {} bytes; it must be 1 to 4", self.name, t.len()));
            }
        }
        if self.min_interval_ms > MAX_MIN_INTERVAL_MS {
            return Err(format!(
                "instrument `{}`'s min_interval_ms {} is above {MAX_MIN_INTERVAL_MS}",
                self.name, self.min_interval_ms
            ));
        }
        for (what, a) in [("ack_ok", &self.ack_ok), ("ack_err", &self.ack_err)] {
            if a.as_deref() == Some("") {
                return Err(format!("instrument `{}`'s {what} is empty; leave it out instead", self.name));
            }
        }
        if self.ack_err.is_some() && self.ack_ok.is_none() {
            return Err(format!(
                "instrument `{}` declares ack_err without ack_ok: a set is judged by ack_ok, so ack_err alone decides nothing",
                self.name
            ));
        }
        if self.ack_ok.is_some() && self.ack_ok == self.ack_err {
            return Err(format!("instrument `{}`'s ack_ok and ack_err are the same text", self.name));
        }
        let owner = format!("instrument `{}`", self.name);
        for line in &self.safe_off {
            check_safe_off_line(&owner, line)?;
        }
        for m in &self.metrology {
            m.check(&owner)?;
        }
        if let Some(c) = &self.calibration {
            c.check(&owner)?;
        }
        Ok(())
    }

    /// Whether a set's reply is read and judged.
    pub fn acknowledges(&self) -> bool {
        self.ack_ok.is_some()
    }
}

/// Two instruments one live port could match both of cannot be told apart:
/// refused at declaration, as decision 36 refuses a bootload pair.
pub fn check_against(new: &Instrument, others: &[Instrument]) -> Result<(), String> {
    match others.iter().find(|o| o.name != new.name && o.port.overlaps(&new.port)) {
        Some(o) => Err(format!(
            "instrument `{}` ({}) and instrument `{}` ({}) could be the same port, so nothing could tell which \
             is which; declare USB serial numbers or interfaces that differ",
            new.name, new.port, o.name, o.port
        )),
        None => Ok(()),
    }
}

/// One command to an instrument: `POST /instruments/{name}/exchange`'s body,
/// and what a host study's `Instrument` step sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentExchange {
    /// The command, without its terminator: the instrument's own is added.
    pub line: String,
    /// A query: its reply is read and returned. Otherwise a set, whose reply
    /// is read only when the instrument declares `ack_ok`.
    #[serde(default)]
    pub query: bool,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// How long to wait for the port to be enumerable. A bridge powered by
    /// the instrument appears only once the instrument is on.
    #[serde(default)]
    pub port_wait_ms: u64,
}

/// `timeout_ms` when none is given.
pub const DEFAULT_TIMEOUT_MS: u64 = 2_000;

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

/// How a set's reply judged against the instrument's declared acknowledgements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ack {
    /// The reply holds `ack_ok`: the set was carried out.
    Ok,
    /// The reply holds `ack_err`: the instrument refused it.
    Refused,
    /// A reply holding neither (an unknown command's own answer, a stray
    /// line), or nothing within the timeout. The set is not known to have
    /// been carried out.
    Unacknowledged,
}

/// What one command did. `reply` is the raw line, terminator removed and
/// nothing else: a value is never parsed here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentReply {
    pub instrument: String,
    pub port: String,
    pub baud: u32,
    pub line: String,
    pub query: bool,
    /// The reply line, or empty when nothing was read (a set on an
    /// instrument that declares no acknowledgement is never read).
    pub reply: String,
    /// Whether the read terminator was seen: a whole reply line arrived.
    pub terminated: bool,
    /// A set's acknowledgement; `None` for a query and for an instrument
    /// declaring no `ack_ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack: Option<Ack>,
    /// What the line sent right after opening (decision 49) answered, so a
    /// parser left poisoned by a partial line is visible rather than silent.
    pub flush_reply: String,
    /// How long this command waited for the instrument's `min_interval_ms`.
    pub paced_ms: u64,
    pub elapsed_ms: u64,
    pub started_utc_ms: u64,
}

impl InstrumentReply {
    /// Whether the command did what it was for: a query got a whole reply
    /// line; a set was acknowledged, or the instrument declares no
    /// acknowledgement to check.
    pub fn succeeded(&self) -> bool {
        if self.query {
            self.terminated
        } else {
            self.ack.is_none_or(|a| a == Ack::Ok)
        }
    }
}

/// Judges a set's reply against `inst`'s acknowledgements. `None` when it
/// declares none.
pub fn judge_ack(inst: &Instrument, reply: &str) -> Option<Ack> {
    let ok = inst.ack_ok.as_deref()?;
    Some(if reply.contains(ok) {
        Ack::Ok
    } else if inst.ack_err.as_deref().is_some_and(|e| reply.contains(e)) {
        Ack::Refused
    } else {
        Ack::Unacknowledged
    })
}

#[cfg(feature = "hardware")]
mod store {
    use anyhow::{anyhow, Result};

    use super::super::enrollment;
    use super::{check_against, find_in, Instrument};
    use crate::hardware::port::{self, DetectedPort};

    pub fn list() -> Result<Vec<Instrument>> {
        Ok(enrollment::load_store()?.instruments)
    }

    pub fn get(name: &str) -> Result<Option<Instrument>> {
        Ok(enrollment::load_store()?.instruments.into_iter().find(|i| i.name == name))
    }

    /// Insert, or replace the one of the same name. Refused when the
    /// declaration fails [`Instrument::check`] or one port could match it and
    /// another instrument both.
    pub fn declare(inst: Instrument) -> Result<()> {
        inst.check().map_err(anyhow::Error::msg)?;
        let mut store = enrollment::load_store()?;
        check_against(&inst, &store.instruments).map_err(anyhow::Error::msg)?;
        match store.instruments.iter_mut().find(|i| i.name == inst.name) {
            Some(slot) => *slot = inst,
            None => store.instruments.push(inst),
        }
        enrollment::save_store(&store)
    }

    /// Removes `name`. `Ok(false)` if nothing was declared under it.
    pub fn remove(name: &str) -> Result<bool> {
        let mut store = enrollment::load_store()?;
        let before = store.instruments.len();
        store.instruments.retain(|i| i.name != name);
        if store.instruments.len() == before {
            return Ok(false);
        }
        enrollment::save_store(&store)?;
        Ok(true)
    }

    /// The one live port `inst` matches. `Ok(None)` when it is not
    /// enumerated; an identity matching several ports is an error naming
    /// every one. Blocking.
    pub fn find_port(inst: &Instrument) -> Result<Option<DetectedPort>> {
        find_in(&port::enumerate()?, &inst.port).map_err(|e| anyhow!("instrument `{}`: {e}", inst.name))
    }
}

#[cfg(feature = "hardware")]
pub use store::{declare, find_port, get, list, remove};

#[cfg(test)]
mod tests {
    use super::*;

    fn load(name: &str, port: &str) -> Instrument {
        Instrument {
            name: name.into(),
            protocol: InstrumentProtocol::Scpi,
            port: port.parse().unwrap(),
            baud: 9600,
            write_terminator: "\n".into(),
            read_terminator: "\r\n".into(),
            min_interval_ms: 200,
            ack_ok: Some("Rexecu success".into()),
            ack_err: Some("Rexecu err".into()),
            safe_off: Vec::new(),
            metrology: Vec::new(),
            calibration: None,
        }
    }

    fn current() -> Metrology {
        Metrology {
            quantity: "current".into(),
            unit: "A".into(),
            resolution: 0.001,
            u_expanded: 0.005,
            k: 2.0,
            basis: MetrologyBasis::Datasheet,
            range: MeasuringRange { min: 0.0, max: 40.0 },
        }
    }

    fn calibrated() -> Instrument {
        calibrated_named("eload")
    }

    fn calibrated_named(name: &str) -> Instrument {
        let mut i = load(name, "0403:6015");
        i.metrology = vec![current()];
        i.calibration = Some(Calibration {
            serial: "SN0001".into(),
            certificate_ref: "CERT-2026-17".into(),
            due: "2027-02-28".into(),
            certificate_sha256: Some("ab".repeat(32)),
        });
        i
    }

    #[test]
    fn metrology_and_calibration_are_checked_naming_the_field() {
        assert!(calibrated().check().is_ok());
        type Spoil = fn(&mut Instrument);
        let cases: [(&str, Spoil); 9] = [
            ("U", |i| i.metrology[0].u_expanded = -0.1),
            ("resolution", |i| i.metrology[0].resolution = -1.0),
            ("resolution", |i| i.metrology[0].resolution = f64::NAN),
            ("k", |i| i.metrology[0].k = 0.0),
            ("range", |i| i.metrology[0].range = MeasuringRange { min: 5.0, max: 1.0 }),
            ("unit", |i| i.metrology[0].unit.clear()),
            ("due", |i| i.calibration.as_mut().unwrap().due = "2027-02-30".into()),
            ("due", |i| i.calibration.as_mut().unwrap().due = "28/02/2027".into()),
            ("certificate_sha256", |i| i.calibration.as_mut().unwrap().certificate_sha256 = Some("AB".repeat(32))),
        ];
        for (field, spoil) in cases {
            let mut i = calibrated();
            spoil(&mut i);
            let err = i.check().unwrap_err();
            assert!(err.contains(field), "{field}: {err}");
        }
        let mut i = calibrated();
        i.calibration.as_mut().unwrap().serial.clear();
        assert!(i.check().unwrap_err().contains("serial"));
        i.calibration.as_mut().unwrap().serial = "SN0001".into();
        i.calibration.as_mut().unwrap().due = "2028-02-29".into();
        assert!(i.check().is_ok(), "a leap day is a date");
        i.calibration.as_mut().unwrap().certificate_sha256 = None;
        assert!(i.check().is_ok(), "the certificate's hash is optional");
    }

    #[test]
    fn metrology_and_calibration_round_trip_in_json() {
        let i = calibrated();
        let json = serde_json::to_string(&i).unwrap();
        assert!(json.contains(r#""U":0.005"#) && json.contains(r#""basis":"datasheet""#), "{json}");
        let back: Instrument = serde_json::from_str(&json).unwrap();
        assert_eq!(back, i);
        let bare = serde_json::to_string(&load("eload", "1a86:7523")).unwrap();
        assert!(!bare.contains("metrology") && !bare.contains("calibration"), "{bare}");
    }

    #[test]
    fn a_declaration_is_checked() {
        assert!(load("eload", "1a86:7523").check().is_ok());
        for bad in ["", "e load", "a/b", &"x".repeat(MAX_NAME_LEN + 1)] {
            assert!(load(bad, "1a86:7523").check().is_err(), "{bad:?}");
        }
        let mut i = load("eload", "1a86:7523");
        i.baud = 0;
        assert!(i.check().is_err());
        let mut i = load("eload", "1a86:7523");
        i.write_terminator.clear();
        assert!(i.check().unwrap_err().contains("write_terminator"));
        let mut i = load("eload", "1a86:7523");
        i.min_interval_ms = MAX_MIN_INTERVAL_MS + 1;
        assert!(i.check().is_err());
        let mut i = load("eload", "1a86:7523");
        i.ack_ok = None;
        assert!(i.check().unwrap_err().contains("ack_err without ack_ok"));
        i.ack_err = None;
        assert!(i.check().is_ok() && !i.acknowledges());
        let mut i = load("eload", "1a86:7523");
        i.ack_err = i.ack_ok.clone();
        assert!(i.check().is_err());
    }

    #[test]
    fn a_safe_off_line_is_checked() {
        let mut i = load("eload", "1a86:7523");
        i.safe_off = vec!["INP OFF".into(), "CURR 0".into()];
        assert!(i.check().is_ok());
        for bad in [String::new(), "A\nB".into(), "A\rB".into(), "x".repeat(MAX_SAFE_OFF_LINE_LEN + 1)] {
            i.safe_off = vec!["INP OFF".into(), bad.clone()];
            assert!(i.check().unwrap_err().contains("safe_off"), "{bad:?}");
        }
        i.safe_off = vec!["x".repeat(MAX_SAFE_OFF_LINE_LEN)];
        assert!(i.check().is_ok());
    }

    #[test]
    fn two_instruments_one_port_could_match_are_refused() {
        let a = load("load-a", "1a86:7523");
        assert!(check_against(&load("load-b", "1a86:7523"), std::slice::from_ref(&a)).is_err());
        // Re-declaring the same name is a replacement, not a clash.
        assert!(check_against(&load("load-a", "1a86:7523"), std::slice::from_ref(&a)).is_ok());
        let mut b = load("load-b", "1a86:7523");
        b.port.interface = Some(1);
        let mut a1 = a.clone();
        a1.port.interface = Some(0);
        assert!(check_against(&b, &[a1]).is_ok());
        assert!(check_against(&load("psu", "0483:5740"), &[a]).is_ok());
    }

    #[test]
    fn a_sets_reply_is_judged_by_its_declared_acks() {
        let i = load("eload", "1a86:7523");
        assert_eq!(judge_ack(&i, "Rexecu success"), Some(Ack::Ok));
        assert_eq!(judge_ack(&i, "Rexecu err"), Some(Ack::Refused));
        assert_eq!(judge_ack(&i, "Rcmd err"), Some(Ack::Unacknowledged));
        assert_eq!(judge_ack(&i, ""), Some(Ack::Unacknowledged));
        let mut quiet = i.clone();
        quiet.ack_ok = None;
        quiet.ack_err = None;
        assert_eq!(judge_ack(&quiet, "anything"), None);
    }

    #[test]
    fn success_is_a_whole_reply_for_a_query_and_an_ack_for_a_set() {
        let mut r = InstrumentReply {
            instrument: "eload".into(),
            port: "COM9".into(),
            baud: 9600,
            line: "*IDN?".into(),
            query: true,
            reply: "R ET5420A+".into(),
            terminated: true,
            ack: None,
            flush_reply: String::new(),
            paced_ms: 0,
            elapsed_ms: 10,
            started_utc_ms: 0,
        };
        assert!(r.succeeded());
        r.terminated = false;
        assert!(!r.succeeded());
        r.query = false;
        r.ack = Some(Ack::Refused);
        assert!(!r.succeeded());
        r.ack = Some(Ack::Ok);
        assert!(r.succeeded());
        r.ack = None;
        assert!(r.succeeded());
    }

    #[test]
    fn defaults_fill_an_older_or_shorter_declaration() {
        let json = r#"{"name":"eload","port":{"vid":6790,"pid":29987},"baud":9600}"#;
        let i: Instrument = serde_json::from_str(json).unwrap();
        assert_eq!((i.protocol, i.write_terminator.as_str(), i.read_terminator.as_str()), (InstrumentProtocol::Scpi, "\n", "\r\n"));
        assert_eq!((i.min_interval_ms, i.ack_ok, i.ack_err), (0, None, None));
        assert!(i.safe_off.is_empty());
        let x: InstrumentExchange = serde_json::from_str(r#"{"line":"*IDN?","query":true}"#).unwrap();
        assert_eq!((x.timeout_ms, x.port_wait_ms), (DEFAULT_TIMEOUT_MS, 0));
    }

    #[cfg(feature = "hardware")]
    #[test]
    fn the_table_round_trips_and_an_older_store_still_loads() {
        use crate::hardware::enrollment::Store;
        let mut store = Store::default();
        store.instruments.push(load("eload", "1a86:7523"));
        let text = toml::to_string_pretty(&store).unwrap();
        let back: Store = toml::from_str(&text).unwrap();
        assert_eq!(back.instruments, store.instruments);

        let mut with = load("psu", "0403:6001");
        with.safe_off = vec!["OUTP OFF".into(), "VOLT 0".into()];
        store.instruments.push(with);
        let back: Store = toml::from_str(&toml::to_string_pretty(&store).unwrap()).unwrap();
        assert_eq!(back.instruments, store.instruments);

        store.instruments.push(calibrated_named("dmm"));
        let back: Store = toml::from_str(&toml::to_string_pretty(&store).unwrap()).unwrap();
        assert_eq!(back.instruments, store.instruments);

        // A store written before the fields loads and is rewritten byte for byte.
        let fixture = "boards = []\nsignals = []\n\n[[instruments]]\nname = \"eload\"\nprotocol = \"scpi\"\nbaud = 9600\nwrite_terminator = \"\\n\"\nread_terminator = \"\\r\\n\"\nmin_interval_ms = 200\n\n[instruments.port]\nvid = 6790\npid = 29987\n";
        let old: Store = toml::from_str(fixture).unwrap();
        assert!(old.instruments[0].safe_off.is_empty());
        assert!(old.instruments[0].metrology.is_empty() && old.instruments[0].calibration.is_none());
        let rewritten = toml::to_string_pretty(&old).unwrap();
        assert!(!rewritten.contains("safe_off"));
        assert!(!rewritten.contains("metrology") && !rewritten.contains("calibration"));
        let again: Store = toml::from_str(&rewritten).unwrap();
        assert_eq!(toml::to_string_pretty(&again).unwrap(), rewritten);

        let older: Store = toml::from_str("boards = []\nsignals = []\n").unwrap();
        assert!(older.instruments.is_empty());
        assert!(!toml::to_string_pretty(&older).unwrap().contains("instruments"));
    }
}
