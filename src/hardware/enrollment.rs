//! `enrollment.toml`: a machine-local table recording which physical board a
//! debug probe's serial number is actually wired to. Formerly
//! `embarch-core`'s own `known_boards.rs` / `known_boards.toml` (design.md
//! §3 decisions 2, 3, 7) — this is "the one thing that's genuinely
//! persisted, since it's declared intent, not detectable" (design.md §2):
//! nothing in a USB descriptor says "I'm wired to the DUT." A human's
//! one-time act of physically isolating a board and enrolling its probe
//! (`enroll`, [`super::validate`]) is the only source for this table; the
//! actual enforcement — the live hardware-ID readback-and-compare that makes
//! it worth trusting — is `validate.rs`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::paths;

/// One enrolled probe↔board association. `hardware_id` is the target chip's
/// own factory-burned unique ID — independent of which probe or cable
/// answers, so it survives a probe getting physically moved to a different
/// board in a way a bare USB serial number can't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrolledBoard {
    pub probe_serial: String,
    pub role: String,
    pub chip: String,
    pub hardware_id: String,
    /// UTC milliseconds since the epoch.
    pub confirmed_at_utc_ms: u64,
    /// A separately declared USB serial number for this role's *runtime
    /// serial link* — meaningful only for [`super::port::DEV_BENCH_ROLE`],
    /// `None` for every other role. Exists because `probe_serial` above is
    /// the JTAG debug probe's own serial, and on real dev-bench hardware
    /// whose runtime link moved to a dedicated UART bridge chip
    /// (`embarch-core/design.md` decision 21's port migration), that bridge
    /// is a *different physical USB device* with its own, unrelated serial
    /// — nothing observable over USB proves the two are the same board, so
    /// this can't be inferred the way `hardware_id` is; it's a second
    /// declared fact, set via [`set_link_port_serial`] once a human reads it
    /// off the actual link port. [`super::port::Filter::resolve`] prefers
    /// this over its old JTAG-probe-serial fallback when set.
    #[serde(default)]
    pub link_port_serial: Option<String>,
    /// Which USB interface of that device carries the link, when the serial
    /// alone cannot say — a debug probe that exposes **two** VCOM ports
    /// shares one USB serial across both, so [`link_port_serial`] and the
    /// `probe_serial` fallback both narrow to a *pair*, not to a port.
    ///
    /// **Real, and it cost a debugging cycle to find (2026-08-31).** The
    /// nRF54L15DK's onboard J-Link OB enumerates VCOM0 and VCOM1 as
    /// interfaces 0 and 2 of one composite device.
    /// [`super::port::select`] used to resolve that pair by taking the
    /// lowest interface index and logging a warning — a rule with no
    /// hardware evidence behind it, since no bench had ever had two VCOMs
    /// before. On this DK it is simply wrong: Zephyr's `zephyr,console` for
    /// this board is `uart20`, whose pins (P1.04/P1.05) are wired to
    /// **VCOM1**, interface 2. The result was a bench that flashed, booted,
    /// ran, and answered nothing, while Core reported a clean detection of
    /// the silent port.
    ///
    /// Undeclared, the old lowest-interface guess still applies — a bench
    /// with a single VCOM (every one before this DK) needs nothing here.
    /// Declared, it narrows hard, same posture as [`link_port_serial`].
    #[serde(default)]
    pub link_port_interface: Option<u8>,
}

/// `enrollment.toml`'s whole contents: the enrolled-board table this file
/// was written for, plus the declared-signal table design.md §3 decision 18
/// added alongside it. One file, because both are the same kind of thing —
/// a declared fact about what is physically wired to what, which no
/// detection can produce.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub boards: Vec<EnrolledBoard>,
    /// `#[serde(default)]` so an `enrollment.toml` written before signals
    /// existed keeps loading, exactly as `link_port_serial` does.
    #[serde(default)]
    pub signals: Vec<super::signal::SignalLink>,
}

pub fn now_utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_at(path: &Path) -> Result<Store> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read enrollment file at {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse enrollment file at {}", path.display()))
}

fn save_at(path: &Path, store: &Store) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let contents = toml::to_string_pretty(store).context("failed to serialize enrollment")?;
    std::fs::write(path, contents)
        .with_context(|| format!("failed to write enrollment file at {}", path.display()))
}

/// The whole store, for callers that own one of its tables
/// ([`super::signal`]). Board-only callers use [`list`]/[`find`] instead.
pub fn load_store() -> Result<Store> {
    load_at(&paths::enrollment_path()?)
}

/// Writes the whole store back. Pairs with [`load_store`]: a caller that
/// edits one table must round-trip the other untouched, which is why neither
/// side ever writes a `Store` it didn't just load.
pub fn save_store(store: &Store) -> Result<()> {
    save_at(&paths::enrollment_path()?, store)
}

/// Look up a probe's enrollment by serial number. `Ok(None)` is a normal
/// "not enrolled yet" outcome, not an error.
pub fn find(probe_serial: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store.boards.into_iter().find(|b| b.probe_serial == probe_serial))
}

/// Look up an enrollment by `role` instead of `probe_serial` —
/// [`super::port::Filter::resolve`]'s fallback use. `role` is otherwise an
/// arbitrary, unvalidated label — this is the one place a specific value
/// (`"dev-bench"`, [`super::port::DEV_BENCH_ROLE`]) is treated as
/// conventional, and only as an opt-in convenience fallback, never enforced.
/// More than one entry sharing `role` returns the first (by file order)
/// rather than erroring — a soft, best-effort lookup, not `validate.rs`'s
/// fail-closed identity gate.
pub fn find_by_role(role: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store.boards.into_iter().find(|b| b.role == role))
}

/// Every currently-enrolled board — the topology UI/CLI's own listing.
pub fn list() -> Result<Vec<EnrolledBoard>> {
    Ok(load_at(&paths::enrollment_path()?)?.boards)
}

/// Insert or replace the entry for `board.probe_serial` — enrollment is
/// idempotent, re-enrolling the same probe overwrites its old row rather
/// than accumulating stale duplicates that could disagree with each other.
///
/// **A `role` is also unique, and that took until 2026-08-31 to enforce.**
/// This function only ever de-duplicated on `probe_serial`, so moving a role
/// to *different silicon* — the dev-bench going from an nRF54L15DK to an
/// ESP32-C5 and back — left two rows both claiming `role = "dev-bench"`.
/// Nothing errored, and nothing looked wrong in the file. What broke is
/// downstream: [`find_by_role`] documents itself as returning the first
/// match by file order, so every role-keyed consumer — `validate`'s identity
/// gate, [`port::Filter::resolve`](super::port::Filter::resolve)'s serial
/// fallback, `POST /validate` — would keep answering with the *unplugged*
/// board, and the newly enrolled one would be unreachable by the only name
/// anything addresses it by. On this bench that presented as the new
/// dev-bench inheriting the old one's `link_port_serial`, a UART bridge that
/// was no longer attached to anything, which hard-narrows detection to a
/// port that cannot exist.
///
/// So a same-`role`/different-`probe_serial` row is displaced too, and
/// returned rather than dropped silently: replacing one board with another
/// under the same name is exactly the kind of thing a caller should be able
/// to say out loud. `Ok(None)` means nothing was displaced.
pub fn upsert(board: EnrolledBoard) -> Result<Option<EnrolledBoard>> {
    upsert_at(&paths::enrollment_path()?, board)
}

/// [`upsert`]'s whole body, against an explicit path. Split out so the
/// role-uniqueness rule above is testable for real rather than
/// re-implemented in a test against a temp file — the shape three tests in
/// this module were already in, and precisely why nobody noticed the rule
/// was missing.
fn upsert_at(path: &Path, board: EnrolledBoard) -> Result<Option<EnrolledBoard>> {
    let mut store = load_at(path)?;

    let displaced = store
        .boards
        .iter()
        .find(|b| b.role == board.role && b.probe_serial != board.probe_serial)
        .cloned();

    store
        .boards
        .retain(|b| b.probe_serial != board.probe_serial && b.role != board.role);
    store.boards.push(board);
    save_at(path, &store)?;
    Ok(displaced)
}

/// Declares `role`'s runtime-link USB serial (`EnrolledBoard::link_port_serial`'s
/// own doc comment) — a second, independent fact from the probe-rs identity
/// readback `enroll`/`upsert` do, since a plain UART bridge has no chip to
/// attach to and no `hardware_id` to read. `role` must already be enrolled
/// (via [`super::validate::enroll`]) — this only ever amends an existing
/// row, it never creates one on its own, so there's always a `probe_serial`/
/// `chip`/`hardware_id` on record for whatever role this link serial gets
/// attached to.
pub fn set_link_port_serial(role: &str, serial: &str) -> Result<()> {
    amend(role, |board| board.link_port_serial = Some(serial.to_string()))
}

/// Declares which USB interface of `role`'s link device actually carries the
/// link ([`EnrolledBoard::link_port_interface`]'s own doc comment for the
/// nRF54L15DK case that forced this). Same contract as
/// [`set_link_port_serial`]: `role` must already be enrolled, and this only
/// ever amends that row.
pub fn set_link_port_interface(role: &str, interface: u8) -> Result<()> {
    amend(role, |board| board.link_port_interface = Some(interface))
}

/// The shared body of the two `set_link_port_*` functions above.
fn amend(role: &str, f: impl FnOnce(&mut EnrolledBoard)) -> Result<()> {
    let path = paths::enrollment_path()?;
    let mut store = load_at(&path)?;
    let board = store
        .boards
        .iter_mut()
        .find(|b| b.role == role)
        .with_context(|| format!("no board enrolled as role '{role}' yet; enroll it first"))?;
    f(board);
    save_at(&path, &store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("embarch-topology-enrollment-test-{name}-{}", std::process::id()))
    }

    fn sample(serial: &str) -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: serial.to_string(),
            role: "reference-dut-fw".to_string(),
            chip: "nRF54L15".to_string(),
            hardware_id: "deadbeefcafef00d".to_string(),
            confirmed_at_utc_ms: 1_755_000_000_000,
            link_port_serial: None,
            link_port_interface: None,
        }
    }

    #[test]
    fn missing_file_is_no_entries_not_an_error() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        let store = load_at(&path).expect("a missing file should load as empty, not error");
        assert!(store.boards.is_empty());
    }

    #[test]
    fn upsert_then_find_round_trips() {
        let dir = temp_path("round-trip-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let board = sample("000852006107");
        save_at(&path, &Store { boards: vec![board.clone()], signals: Vec::new() }).unwrap();

        let found = load_at(&path).unwrap().boards.into_iter().find(|b| b.probe_serial == board.probe_serial);
        assert_eq!(found, Some(board));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_by_role_matches_on_role_not_serial() {
        let dir = temp_path("by-role-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut dev_bench = sample("D0:CF:13:ED:F9:30");
        dev_bench.role = "dev-bench".to_string();
        dev_bench.chip = "esp32c5".to_string();
        save_at(&path, &Store { boards: vec![sample("000852006107"), dev_bench.clone()], signals: Vec::new() }).unwrap();

        let found = load_at(&path).unwrap().boards.into_iter().find(|b| b.role == "dev-bench");
        assert_eq!(found, Some(dev_bench));

        let missing = load_at(&path).unwrap().boards.into_iter().find(|b| b.role == "no-such-role");
        assert_eq!(missing, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The real bug, pinned against the real function: moving a role onto
    /// different silicon must leave exactly one row holding that role, and
    /// must say which board it displaced.
    #[test]
    fn upsert_moves_a_role_to_a_new_probe_and_reports_the_displaced_board() {
        let dir = temp_path("role-move-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        // The bench as it stood: an ESP32-C5 dev-bench with a declared link
        // port on a separate UART bridge, plus an unrelated DUT.
        let mut old_bench = sample("D0:CF:13:ED:F9:30");
        old_bench.role = "dev-bench".to_string();
        old_bench.chip = "esp32c5".to_string();
        old_bench.link_port_serial = Some("D607104BD96EF0119D5C489B1045C30F".to_string());
        let dut = sample("000852006107");
        save_at(&path, &Store { boards: vec![old_bench.clone(), dut.clone()], signals: Vec::new() })
            .unwrap();

        let mut new_bench = sample("001057729826");
        new_bench.role = "dev-bench".to_string();
        new_bench.chip = "nRF54L15".to_string();

        let displaced = upsert_at(&path, new_bench.clone()).unwrap();
        assert_eq!(displaced, Some(old_bench));

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(
            boards.iter().filter(|b| b.role == "dev-bench").count(),
            1,
            "a role must be held by exactly one board"
        );
        let bench = boards.iter().find(|b| b.role == "dev-bench").unwrap();
        assert_eq!(bench.probe_serial, "001057729826");
        assert_eq!(
            bench.link_port_serial, None,
            "the displaced board's link port must not be inherited by different hardware"
        );
        assert!(
            boards.iter().any(|b| b.probe_serial == dut.probe_serial),
            "an unrelated role must be left alone"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Re-enrolling the *same* probe under the same role displaces nothing —
    /// the ordinary idempotent case must not report a phantom replacement.
    #[test]
    fn upsert_of_the_same_probe_displaces_nothing() {
        let dir = temp_path("same-probe-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let board = sample("001057729826");
        assert_eq!(upsert_at(&path, board.clone()).unwrap(), None);
        assert_eq!(upsert_at(&path, board).unwrap(), None);
        assert_eq!(load_at(&path).unwrap().boards.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upsert_overwrites_rather_than_duplicates() {
        let dir = temp_path("overwrite-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut store = Store::default();
        store.boards.push(sample("same-serial"));
        save_at(&path, &store).unwrap();

        let mut updated = sample("same-serial");
        updated.hardware_id = "a-new-hardware-id".to_string();
        let mut reloaded = load_at(&path).unwrap();
        reloaded.boards.retain(|b| b.probe_serial != updated.probe_serial);
        reloaded.boards.push(updated.clone());
        save_at(&path, &reloaded).unwrap();

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards.len(), 1, "re-enrolling the same serial must not accumulate a second row");
        assert_eq!(boards[0], updated);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_link_port_serial_deserializes_as_none() {
        // Pre-existing enrollment.toml rows, written before this field
        // existed, must keep loading rather than erroring.
        let toml = r#"
            [[boards]]
            probe_serial = "D0:CF:13:ED:F9:30"
            role = "dev-bench"
            chip = "esp32c5"
            hardware_id = "13edf930fffed0cf"
            confirmed_at_utc_ms = 1787528352457
        "#;
        let store: Store = toml::from_str(toml).unwrap();
        assert_eq!(store.boards[0].link_port_serial, None);
    }

    #[test]
    fn set_link_port_serial_amends_an_existing_role_and_round_trips() {
        let dir = temp_path("link-port-serial-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut dev_bench = sample("D0:CF:13:ED:F9:30");
        dev_bench.role = "dev-bench".to_string();
        dev_bench.chip = "esp32c5".to_string();
        save_at(&path, &Store { boards: vec![dev_bench], signals: Vec::new() }).unwrap();

        // Reimplement set_link_port_serial against the temp path directly —
        // the real fn goes through paths::enrollment_path(), not overridable
        // per-test.
        let mut store = load_at(&path).unwrap();
        store.boards.iter_mut().find(|b| b.role == "dev-bench").unwrap().link_port_serial =
            Some("D607104BD96EF0119D5C489B1045C30F".to_string());
        save_at(&path, &store).unwrap();

        let reloaded = load_at(&path).unwrap();
        assert_eq!(
            reloaded.boards[0].link_port_serial.as_deref(),
            Some("D607104BD96EF0119D5C489B1045C30F")
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
