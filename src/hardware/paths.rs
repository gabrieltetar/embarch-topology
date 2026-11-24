//! Where this crate's own persisted state lives — the machine-wide directory
//! `embarch-core`'s `token_store.rs` already established the convention for
//! (`%ProgramData%\embarch` / `/var/lib/embarch`), one level down in a
//! `topology` subdirectory this crate owns outright. Why the same directory
//! as Core's, rather than one of this crate's own: decision 23.
//!
//! This resolves the crate-internal storage schema/location question (spec.md's
//! "Storage and roles"): reuse the exact directory `known_boards.toml` already used
//! (continuity — this *is* that file's replacement, decision 3), scoped under
//! its own subdirectory so this crate's files (`enrollment.toml`,
//! `alerts.jsonl`) don't sit loose next to Core's `token`/`logs`.

use anyhow::{bail, Result};
use std::path::PathBuf;
use std::sync::Mutex;

/// The linking process's choice of folder, and whether anything has read the
/// folder yet. One value for the whole process: the store is a single file
/// set, and two folders in one process would be two stores.
struct Setting {
    dir: Option<PathBuf>,
    used: bool,
}

impl Setting {
    const fn new() -> Self {
        Setting { dir: None, used: false }
    }

    fn set(&mut self, dir: PathBuf) -> Result<()> {
        if let Some(current) = &self.dir {
            bail!("the identity store's folder is already set (to {}); it can be set once", current.display());
        }
        if self.used {
            bail!("the identity store's folder cannot be set: the store or the alert log was already accessed at its default path; set it once, at start, before first use");
        }
        self.dir = Some(dir);
        Ok(())
    }

    /// The set folder, or `None` for the default; either way marks the folder
    /// as read, so a later `set` is refused.
    fn read(&mut self) -> Option<PathBuf> {
        self.used = true;
        self.dir.clone()
    }
}

static SETTING: Mutex<Setting> = Mutex::new(Setting::new());

/// Sets the identity store's folder for this process, replacing the default
/// `<machine data dir>/topology`. Callable once, and only before the first
/// store or alert access; a second call, or a call after first use, is
/// refused. The CLI never calls it.
pub fn set_data_dir(dir: PathBuf) -> Result<()> {
    SETTING.lock().unwrap_or_else(|e| e.into_inner()).set(dir)
}

#[cfg(windows)]
fn machine_data_dir() -> Result<PathBuf> {
    use anyhow::Context;
    let program_data =
        std::env::var("ProgramData").context("ProgramData environment variable is not set")?;
    Ok(PathBuf::from(program_data).join("embarch"))
}

#[cfg(unix)]
fn machine_data_dir() -> Result<PathBuf> {
    Ok(PathBuf::from("/var/lib/embarch"))
}

/// The folder [`set_data_dir`] set, or by default
/// `<machine_data_dir>/topology` — created on first write if it doesn't
/// exist yet. Every path this module hands out is a child of this one.
/// Reading it ends the window in which [`set_data_dir`] is accepted.
pub fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = SETTING.lock().unwrap_or_else(|e| e.into_inner()).read() {
        return Ok(dir);
    }
    Ok(machine_data_dir()?.join("topology"))
}

pub fn enrollment_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("enrollment.toml"))
}

pub fn alert_log_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("alerts.jsonl"))
}

// `ui_marker_path()` (`<data_dir>/ui.addr`) lived here until 2026-08-25 —
// the running UI's own bound address, written by `bin/ui.rs` so a
// `validate()` call elsewhere on the machine could push it a live alert.
// `bin/ui.rs` was deleted 2026-08-24 and `embarch-ui` never wrote the
// marker, so nothing has produced this file since; retired with the rest of
// the live-push mechanism (decision 19). Named here rather
// than silently dropped because the file may still exist on a machine that
// ran the old UI, and nothing reads it any more.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::bootload::UsbPortId;
    use crate::hardware::enrollment;
    use crate::hardware::instrument::{self, Instrument, InstrumentProtocol};
    use crate::hardware::signal::{self, Route, SignalDirection, SignalLink};

    #[test]
    fn a_second_set_and_a_set_after_use_are_refused_saying_why() {
        let mut s = Setting::new();
        s.set(PathBuf::from("/a")).unwrap();
        let again = s.set(PathBuf::from("/b")).unwrap_err().to_string();
        assert!(again.contains("already set") && again.contains("/a"), "{again}");
        assert_eq!(s.read(), Some(PathBuf::from("/a")));

        let mut used = Setting::new();
        assert_eq!(used.read(), None);
        let late = used.set(PathBuf::from("/a")).unwrap_err().to_string();
        assert!(late.contains("already accessed"), "{late}");
    }

    /// The only test in this binary that touches the process-wide setting:
    /// every other test goes through the `_at` functions with its own path.
    #[test]
    fn a_set_folder_holds_the_store_the_alert_log_and_nothing_lands_at_the_default() {
        let dir = std::env::temp_dir().join(format!("embarch-topology-set-data-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_data_dir(dir.clone()).unwrap();

        enrollment::set_role_board("dut", "nrf54l15dk", "nRF54L15").unwrap();
        instrument::declare(Instrument {
            name: "dmm".into(),
            protocol: InstrumentProtocol::Scpi,
            port: "0403:6001:A1".parse::<UsbPortId>().unwrap(),
            baud: 9600,
            write_terminator: "\n".into(),
            read_terminator: "\n".into(),
            min_interval_ms: 0,
            ack_ok: None,
            ack_err: None,
            safe_off: Vec::new(),
        })
        .unwrap();
        signal::declare(SignalLink {
            name: "console".into(),
            origin_role: "dut".into(),
            origin_board: None,
            direction: SignalDirection::DutToHost,
            route: Route::Direct { port_serial: "FT9ABCDE".into(), interface: None },
            baud: None,
            safe_off: None,
        })
        .unwrap();

        // Every path is a child of the set folder, so none can be the default's.
        assert_eq!(data_dir().unwrap(), dir);
        assert_eq!(enrollment_path().unwrap(), dir.join("enrollment.toml"));
        assert_eq!(alert_log_path().unwrap(), dir.join("alerts.jsonl"));
        let text = std::fs::read_to_string(dir.join("enrollment.toml")).unwrap();
        assert!(text.contains("nrf54l15dk") && text.contains("dmm") && text.contains("console"), "{text}");

        let err = set_data_dir(PathBuf::from("/elsewhere")).unwrap_err().to_string();
        assert!(err.contains("already set"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unset_the_default_is_the_machine_dir_topology() {
        let mut s = Setting::new();
        assert_eq!(s.read(), None);
        assert!(machine_data_dir().unwrap().join("topology").ends_with("topology"));
    }
}
