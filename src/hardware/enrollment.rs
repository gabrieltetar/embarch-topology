//! `enrollment.toml`: a machine-local table recording which physical board a
//! debug probe's serial number is actually wired to. Formerly
//! `embarch-core`'s own `known_boards.rs` / `known_boards.toml`
//! (`embarch-core` decision 22) — this is "the one thing that's genuinely
//! persisted, since it's declared intent, not detectable" (spec.md):
//! nothing in a USB descriptor says "I'm wired to the DUT." A human's
//! one-time act of physically isolating a board and enrolling its probe
//! (`enroll`, [`super::validate`]) is the only source for this table; the
//! actual enforcement — the live hardware-ID readback-and-compare that makes
//! it worth trusting — is `validate.rs`.

#[cfg(feature = "hardware")]
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(feature = "hardware")]
use std::path::Path;

#[cfg(feature = "hardware")]
use super::paths;

/// The one role a DUT is enrolled under. Its dev-bench counterpart is
/// [`super::port::DEV_BENCH_ROLE`], which predates this constant and stays
/// where the port resolver reads it.
pub const DUT_ROLE: &str = "dut";

/// The suite's whole role vocabulary: **two fixed roles, and a board name is
/// a separate fact** (`embarch-ui` decision 44). Before that, a board's only
/// human-readable label *was* its role, so a bench with a third board got a
/// third role invented for it (`client-nucleo`) — a name wearing a
/// role's clothes, which made "which board is the DUT" unanswerable by
/// looking.
///
/// **Closed on the write path, tolerant on the load path.** [`upsert`] is
/// reached through `embarch-core`'s `POST /probes/enroll`, which rejects a
/// role outside this pair; nothing here re-checks a row already on disk,
/// because a store predating a later fact must keep loading (the same rule
/// `link_port_serial` and `signals` are under). A foreign row is therefore
/// still reachable through [`list`], and the surface that shows it is what
/// offers to remove it.
pub const CANONICAL_ROLES: [&str; 2] = [super::port::DEV_BENCH_ROLE, DUT_ROLE];

/// Whether `role` is one of [`CANONICAL_ROLES`]. The check `embarch-core`'s
/// enroll route applies before writing, and the check a renderer applies to
/// decide whether a row is a board holding a role or a leftover to clear.
pub fn is_canonical_role(role: &str) -> bool {
    CANONICAL_ROLES.contains(&role)
}

/// One enrolled probe↔board association. `hardware_id` is the target chip's
/// own factory-burned unique ID — independent of which probe or cable
/// answers, so it survives a probe getting physically moved to a different
/// board in a way a bare USB serial number can't.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnrolledBoard {
    /// **Optional since `embarch-ui` decision 45.** A role holds two
    /// independent bindings — which *probe* serves it, and which *board
    /// type* is in it — and either can be declared without the other. A row
    /// with no probe is a role whose board is known and whose silicon has
    /// never been read: a real state, and the one a bench is in while it is
    /// being set up.
    ///
    /// Deserializing an older store still works unchanged: a plain string
    /// reads as `Some`.
    #[serde(default)]
    pub probe_serial: Option<String>,
    pub role: String,
    /// The **board type** in this role — `nrf54l15dk`, `esp32c5_devkitc`,
    /// a board a firmware repo builds for. A *shape*, not a piece of
    /// hardware: two identical DKs are one board type, and what tells the
    /// two physical units apart is [`hardware_id`](Self::hardware_id),
    /// read through whatever probe is on one of them.
    ///
    /// **Opaque here**: nothing in this crate reads it, compares it or
    /// requires it to resolve to anything. It exists because `role` stopped
    /// being a name (`embarch-ui` decision 44) and because a run has to
    /// know what to build for (45).
    ///
    /// The catalog it names lives in a firmware repo
    /// (`embarch/boards.toml`), not on this machine, so a name written
    /// under one project and read under another may resolve to nothing —
    /// which is a fact for the renderer to state, never for this crate to
    /// repair.
    ///
    /// `#[serde(default)]` for the same reason every field below it has
    /// one: an `enrollment.toml` written before names existed still loads,
    /// with an empty name rather than a guessed one.
    #[serde(default)]
    pub name: String,
    pub chip: String,
    /// The probe-read (JTAG) hardware ID — not the bench's self-reported
    /// one. `embarch-core` decision 56. `None` exactly when
    /// [`probe_serial`](Self::probe_serial) is: an identity read needs a
    /// probe to read it through, so the two are one fact and are written
    /// together by [`super::validate::enroll`].
    #[serde(default)]
    pub hardware_id: Option<String>,
    /// UTC milliseconds since the epoch — when the probe half was last
    /// written. `None` while there is no probe half, never `0`: a row that
    /// has never been verified does not claim to have been verified at the
    /// epoch.
    #[serde(default)]
    pub confirmed_at_utc_ms: Option<u64>,
    /// A separately declared USB serial number for this role's *runtime
    /// serial link* — meaningful only for [`super::port::DEV_BENCH_ROLE`],
    /// `None` for every other role. Exists because `probe_serial` above is
    /// the JTAG debug probe's own serial, and on real dev-bench hardware
    /// whose runtime link moved to a dedicated UART bridge chip
    /// (`embarch-core` decision 27's port migration), that bridge
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
/// was written for, plus the declared-signal table decision 18
/// added alongside it. One file, because both are the same kind of thing —
/// a declared fact about what is physically wired to what, which no
/// detection can produce.
#[derive(Debug, Default, Serialize, Deserialize)]
#[cfg(feature = "hardware")]
pub struct Store {
    #[serde(default)]
    pub boards: Vec<EnrolledBoard>,
    /// `#[serde(default)]` so an `enrollment.toml` written before signals
    /// existed keeps loading, exactly as `link_port_serial` does.
    #[serde(default)]
    pub signals: Vec<super::signal::SignalLink>,
}

#[cfg(feature = "hardware")]
pub fn now_utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(feature = "hardware")]
fn load_at(path: &Path) -> Result<Store> {
    if !path.exists() {
        return Ok(Store::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read enrollment file at {}", path.display()))?;
    toml::from_str(&contents)
        .with_context(|| format!("failed to parse enrollment file at {}", path.display()))
}

#[cfg(feature = "hardware")]
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
#[cfg(feature = "hardware")]
pub fn load_store() -> Result<Store> {
    load_at(&paths::enrollment_path()?)
}

/// Writes the whole store back. Pairs with [`load_store`]: a caller that
/// edits one table must round-trip the other untouched, which is why neither
/// side ever writes a `Store` it didn't just load.
#[cfg(feature = "hardware")]
pub fn save_store(store: &Store) -> Result<()> {
    save_at(&paths::enrollment_path()?, store)
}

/// Look up a probe's enrollment by serial number. `Ok(None)` is a normal
/// "not enrolled yet" outcome, not an error.
#[cfg(feature = "hardware")]
pub fn find(probe_serial: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store
        .boards
        .into_iter()
        .find(|b| b.probe_serial.as_deref() == Some(probe_serial)))
}

/// Look up an enrollment by `role` instead of `probe_serial` —
/// [`super::port::Filter::resolve`]'s fallback use. `role` is otherwise an
/// arbitrary, unvalidated label — this is the one place a specific value
/// (`"dev-bench"`, [`super::port::DEV_BENCH_ROLE`]) is treated as
/// conventional, and only as an opt-in convenience fallback, never enforced.
/// More than one entry sharing `role` returns the first (by file order)
/// rather than erroring — a soft, best-effort lookup, not `validate.rs`'s
/// fail-closed identity gate.
#[cfg(feature = "hardware")]
pub fn find_by_role(role: &str) -> Result<Option<EnrolledBoard>> {
    let store = load_at(&paths::enrollment_path()?)?;
    Ok(store.boards.into_iter().find(|b| b.role == role))
}

/// Every currently-enrolled board — the topology UI/CLI's own listing.
#[cfg(feature = "hardware")]
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
///
/// **That return covers exactly one row.** This function only ever
/// de-duplicates going forward — nothing on the load path re-checks the
/// invariant — so a store that already held more than one row for `role`
/// (reachable only via a hand-edited or pre-2026-08-31 `enrollment.toml`,
/// since this function itself never creates that state) reports the first
/// one found displaced and silently removes the rest: `find` below returns
/// one, `retain` removes every row sharing `role`.
#[cfg(feature = "hardware")]
pub fn upsert(board: EnrolledBoard) -> Result<Option<EnrolledBoard>> {
    upsert_at(&paths::enrollment_path()?, board)
}

/// [`upsert`]'s whole body, against an explicit path. Split out so the
/// role-uniqueness rule above is testable for real rather than
/// re-implemented in a test against a temp file — the shape three tests in
/// this module were already in, and precisely why nobody noticed the rule
/// was missing.
#[cfg(feature = "hardware")]
fn upsert_at(path: &Path, board: EnrolledBoard) -> Result<Option<EnrolledBoard>> {
    let mut store = load_at(path)?;

    let displaced = store
        .boards
        .iter()
        .find(|b| b.role == board.role && b.probe_serial != board.probe_serial)
        .cloned();

    // **A row with no probe is never treated as sharing one.** Matching on
    // `Option` equality alone would make two boardless roles collide on
    // `None == None` and silently delete each other, which is the state a
    // bench is in the moment both roles have a board type and neither has
    // been wired up yet.
    let same_probe = |b: &EnrolledBoard| {
        board.probe_serial.is_some() && b.probe_serial == board.probe_serial
    };
    store.boards.retain(|b| !same_probe(b) && b.role != board.role);
    store.boards.push(board);
    save_at(path, &store)?;
    Ok(displaced)
}

/// Declares **which board type is in `role`**, without opening anything
/// (`embarch-ui` decision 45). The write behind `embarch-core`'s
/// `PUT /probes/enrolled/{role}/board`.
///
/// **The half that carries no identity claim.** A board type is a shape a
/// repo builds for; saying which one is in a role is a statement about the
/// bench, not about silicon, so it needs no probe, no attach and no
/// hardware ID — and it must work with nothing plugged in, which is when a
/// bench is usually being described. The probe half
/// ([`super::validate::enroll`]) is what reads an identity, and this
/// function never touches it: an existing probe binding is carried across
/// verbatim.
///
/// **Changing the board type does not clear the recorded hardware ID**, and
/// deliberately: the ID is what the probe last read, a fact about a past
/// read rather than a claim about the present. A role whose board type
/// moved to different silicon now has a row whose halves disagree, and
/// `POST /validate` is what says so — loudly, by name — rather than this
/// function quietly erasing the evidence.
#[cfg(feature = "hardware")]
pub fn set_role_board(role: &str, name: &str, chip: &str) -> Result<EnrolledBoard> {
    set_role_board_at(&paths::enrollment_path()?, role, name, chip)
}

/// [`set_role_board`]'s body against an explicit path — the same split
/// [`upsert_at`] is under, and for the same reason.
#[cfg(feature = "hardware")]
fn set_role_board_at(path: &Path, role: &str, name: &str, chip: &str) -> Result<EnrolledBoard> {
    let mut store = load_at(path)?;
    if let Some(existing) = store.boards.iter_mut().find(|b| b.role == role) {
        existing.name = name.to_string();
        existing.chip = chip.to_string();
        let updated = existing.clone();
        save_at(path, &store)?;
        return Ok(updated);
    }
    let row = EnrolledBoard {
        probe_serial: None,
        role: role.to_string(),
        name: name.to_string(),
        chip: chip.to_string(),
        hardware_id: None,
        confirmed_at_utc_ms: None,
        link_port_serial: None,
        link_port_interface: None,
    };
    store.boards.push(row.clone());
    save_at(path, &store)?;
    Ok(row)
}

/// Removes whatever board holds `role`, returning it. `Ok(None)` means
/// nothing was enrolled under that role — an ordinary outcome for a caller
/// retracting a row it believed existed, and the one the HTTP layer answers
/// `404` to.
///
/// **The counterpart [`upsert`] never had.** Enrolling could displace a row
/// (by role, or by probe serial) but nothing could retract one, so a
/// mis-enrolled board — or, before roles closed to a fixed pair
/// ([`CANONICAL_ROLES`]), a board enrolled under an invented role — stayed
/// in `enrollment.toml` for good, short of hand-editing a file inside a
/// permission wall on the real deployment. Removing is a plain file write:
/// no probe is opened, and a board that is not attached is removed exactly
/// as one that is.
///
/// Every row sharing `role` goes, not just the first, for the same reason
/// [`upsert`] retains that way: a pre-2026-08-31 store can hold more than
/// one, and leaving the rest behind would make a retraction look like it had
/// silently failed.
#[cfg(feature = "hardware")]
pub fn remove_by_role(role: &str) -> Result<Option<EnrolledBoard>> {
    remove_by_role_at(&paths::enrollment_path()?, role)
}

/// [`remove_by_role`]'s body against an explicit path, testable for real —
/// the same split [`upsert_at`] is under, and for the same reason.
#[cfg(feature = "hardware")]
fn remove_by_role_at(path: &Path, role: &str) -> Result<Option<EnrolledBoard>> {
    let mut store = load_at(path)?;
    let removed = store.boards.iter().find(|b| b.role == role).cloned();
    if removed.is_none() {
        return Ok(None);
    }
    store.boards.retain(|b| b.role != role);
    save_at(path, &store)?;
    Ok(removed)
}

/// Declares `role`'s runtime-link USB serial (`EnrolledBoard::link_port_serial`'s
/// own doc comment) — a second, independent fact from the probe-rs identity
/// readback `enroll`/`upsert` do, since a plain UART bridge has no chip to
/// attach to and no `hardware_id` to read. `role` must already be enrolled
/// (via [`super::validate::enroll`]) — this only ever amends an existing
/// row, it never creates one on its own, so there's always a `probe_serial`/
/// `chip`/`hardware_id` on record for whatever role this link serial gets
/// attached to.
#[cfg(feature = "hardware")]
pub fn set_link_port_serial(role: &str, serial: &str) -> Result<()> {
    amend(role, |board| board.link_port_serial = Some(serial.to_string()))
}

/// Declares which USB interface of `role`'s link device actually carries the
/// link ([`EnrolledBoard::link_port_interface`]'s own doc comment for the
/// nRF54L15DK case that forced this). Same contract as
/// [`set_link_port_serial`]: `role` must already be enrolled, and this only
/// ever amends that row.
#[cfg(feature = "hardware")]
pub fn set_link_port_interface(role: &str, interface: u8) -> Result<()> {
    amend(role, |board| board.link_port_interface = Some(interface))
}

/// Unsets `role`'s declared link port serial, reverting
/// [`super::port::Filter::resolve`] to its JTAG-probe-serial fallback —
/// `embarch-topology` decision 27, closing the exact gap decision 20 named:
/// a stale declared serial hard-narrows detection to a port that no longer
/// exists, and there was previously no way to clear it short of hand-editing
/// `enrollment.toml`. `role` must already be enrolled, same contract as
/// [`set_link_port_serial`].
#[cfg(feature = "hardware")]
pub fn clear_link_port_serial(role: &str) -> Result<()> {
    amend(role, |board| board.link_port_serial = None)
}

/// Unsets `role`'s declared link port interface. Same contract and rationale
/// as [`clear_link_port_serial`], for [`EnrolledBoard::link_port_interface`].
#[cfg(feature = "hardware")]
pub fn clear_link_port_interface(role: &str) -> Result<()> {
    amend(role, |board| board.link_port_interface = None)
}

/// The shared body of the two `set_link_port_*` functions above.
#[cfg(feature = "hardware")]
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

#[cfg(all(test, feature = "hardware"))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("embarch-topology-enrollment-test-{name}-{}", std::process::id()))
    }

    fn sample(serial: &str) -> EnrolledBoard {
        EnrolledBoard {
            probe_serial: Some(serial.to_string()),
            role: DUT_ROLE.to_string(),
            name: "reference-dut-fw".to_string(),
            chip: "nRF54L15".to_string(),
            hardware_id: Some("deadbeefcafef00d".to_string()),
            confirmed_at_utc_ms: Some(1_755_000_000_000),
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
        assert_eq!(bench.probe_serial.as_deref(), Some("001057729826"));
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
        updated.hardware_id = Some("a-new-hardware-id".to_string());
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

    /// The clearing affordance `tasks/topology/004` adds: a previously
    /// declared link port serial can be unset again, without a hand edit of
    /// `enrollment.toml` — the fix for decision 20's own failure mode, where
    /// re-enrolling by role carries the stale serial right back over
    /// (`validate::enroll`'s own doc comment on why that's keyed on probe
    /// serial rather than role).
    #[test]
    fn clear_link_port_serial_unsets_a_declared_one_and_round_trips() {
        let dir = temp_path("clear-link-port-serial-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut dev_bench = sample("D0:CF:13:ED:F9:30");
        dev_bench.role = "dev-bench".to_string();
        dev_bench.chip = "esp32c5".to_string();
        dev_bench.link_port_serial = Some("D607104BD96EF0119D5C489B1045C30F".to_string());
        save_at(&path, &Store { boards: vec![dev_bench], signals: Vec::new() }).unwrap();

        // Reimplement clear_link_port_serial against the temp path directly
        // — the real fn goes through paths::enrollment_path(), not
        // overridable per-test (same idiom as the `set_*` test above).
        let mut store = load_at(&path).unwrap();
        store.boards.iter_mut().find(|b| b.role == "dev-bench").unwrap().link_port_serial = None;
        save_at(&path, &store).unwrap();

        let reloaded = load_at(&path).unwrap();
        assert_eq!(reloaded.boards[0].link_port_serial, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Same, for `link_port_interface`.
    #[test]
    fn clear_link_port_interface_unsets_a_declared_one_and_round_trips() {
        let dir = temp_path("clear-link-port-interface-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut dev_bench = sample("001057729826");
        dev_bench.role = "dev-bench".to_string();
        dev_bench.link_port_interface = Some(2);
        save_at(&path, &Store { boards: vec![dev_bench], signals: Vec::new() }).unwrap();

        let mut store = load_at(&path).unwrap();
        store.boards.iter_mut().find(|b| b.role == "dev-bench").unwrap().link_port_interface = None;
        save_at(&path, &store).unwrap();

        let reloaded = load_at(&path).unwrap();
        assert_eq!(reloaded.boards[0].link_port_interface, None);

        std::fs::remove_dir_all(&dir).ok();
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

    /// Removing is keyed on role and takes every row holding it — the
    /// pre-2026-08-31 duplicate case `upsert` already retains against.
    #[test]
    fn remove_by_role_takes_every_row_holding_it_and_returns_one() {
        let dir = temp_path("remove-role-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        // A store as a hand-edited one can be: two rows sharing a role,
        // plus an unrelated board that must survive.
        let mut first = sample("D0:CF:13:ED:F9:30");
        first.role = "client-nucleo".to_string();
        let mut second = sample("001057729826");
        second.role = "client-nucleo".to_string();
        let keeper = sample("000852006107");
        save_at(
            &path,
            &Store { boards: vec![first.clone(), second, keeper.clone()], signals: Vec::new() },
        )
        .unwrap();

        let removed = remove_by_role_at(&path, "client-nucleo").unwrap();
        assert_eq!(removed, Some(first));

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards, vec![keeper], "every row holding the role must go, and only those");
    }

    /// Retracting a role nothing holds is an ordinary answer, not an error,
    /// and it must not rewrite the file.
    #[test]
    fn remove_by_role_reports_nothing_removed_without_touching_the_store() {
        let dir = temp_path("remove-missing-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        save_at(&path, &Store { boards: vec![sample("000852006107")], signals: Vec::new() })
            .unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        assert_eq!(remove_by_role_at(&path, "dev-bench").unwrap(), None);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A store written before names existed still loads, with an empty name
    /// rather than a failure or a guess.
    #[test]
    fn a_store_without_names_still_loads() {
        let toml = r#"
[[boards]]
probe_serial = "000852006107"
role = "dut"
chip = "nRF54L15"
hardware_id = "deadbeefcafef00d"
confirmed_at_utc_ms = 1755000000000
"#;
        let store: Store = toml::from_str(toml).expect("a pre-name store must still load");
        assert_eq!(store.boards[0].name, "");
        assert_eq!(store.boards[0].role, DUT_ROLE);
    }

    /// The role vocabulary is closed, and a board name is not a role.
    #[test]
    fn only_the_two_canonical_roles_are_roles() {
        assert!(is_canonical_role("dut"));
        assert!(is_canonical_role("dev-bench"));
        assert!(!is_canonical_role("client-nucleo"));
        assert!(!is_canonical_role("DUT"), "the wire spelling is lowercase; the label is not");
        assert!(!is_canonical_role(""));
    }

    /// The board half writes with no probe, and the probe half is not
    /// invented to make room for it.
    #[test]
    fn a_board_type_can_be_declared_for_a_role_with_no_probe() {
        let dir = temp_path("set-board-empty-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let row = set_role_board_at(&path, DUT_ROLE, "nrf54l15dk", "nRF54L15").unwrap();
        assert_eq!(row.name, "nrf54l15dk");
        assert_eq!(row.probe_serial, None);
        assert_eq!(row.hardware_id, None);
        assert_eq!(row.confirmed_at_utc_ms, None, "never verified is not verified at the epoch");

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].role, DUT_ROLE);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Changing the board type leaves the probe binding and the recorded
    /// identity alone — the halves are independent, and a stale identity is
    /// evidence for `validate` rather than something to erase.
    #[test]
    fn setting_a_board_type_keeps_the_probe_half_untouched() {
        let dir = temp_path("set-board-keep-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        let mut enrolled = sample("000852006107");
        enrolled.name = "nrf54l15dk".to_string();
        save_at(&path, &Store { boards: vec![enrolled.clone()], signals: Vec::new() }).unwrap();

        let row = set_role_board_at(&path, DUT_ROLE, "client-nucleo", "STM32G0B1VE").unwrap();
        assert_eq!(row.name, "client-nucleo");
        assert_eq!(row.chip, "STM32G0B1VE");
        assert_eq!(row.probe_serial, enrolled.probe_serial, "the probe binding is a separate fact");
        assert_eq!(
            row.hardware_id, enrolled.hardware_id,
            "the recorded identity stays for validate to disagree with"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two roles that each hold a board type and no probe must both
    /// survive: matching on `Option` equality alone would make them
    /// collide on `None == None`.
    #[test]
    fn two_boardless_roles_do_not_displace_each_other() {
        let dir = temp_path("boardless-roles-dir");
        let path = dir.join("enrollment.toml");
        let _ = std::fs::remove_dir_all(&dir);

        set_role_board_at(&path, DUT_ROLE, "nrf54l15dk", "nRF54L15").unwrap();
        set_role_board_at(&path, super::super::port::DEV_BENCH_ROLE, "esp32c5_devkitc", "esp32c5")
            .unwrap();

        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards.len(), 2, "{boards:?}");

        // And an enrol into one of them, with a probe, still leaves the other.
        let mut enrolled = sample("000852006107");
        enrolled.name = "nrf54l15dk".to_string();
        upsert_at(&path, enrolled).unwrap();
        let boards = load_at(&path).unwrap().boards;
        assert_eq!(boards.len(), 2, "{boards:?}");
        assert_eq!(
            boards.iter().filter(|b| b.probe_serial.is_none()).count(),
            1,
            "the bench role keeps its boardless row"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A store written before the probe half became optional still loads,
    /// with both halves present.
    #[test]
    fn a_pre_optional_probe_store_still_loads() {
        let toml = r#"
[[boards]]
probe_serial = "000852006107"
role = "dut"
chip = "nRF54L15"
hardware_id = "deadbeefcafef00d"
confirmed_at_utc_ms = 1755000000000
"#;
        let store: Store = toml::from_str(toml).expect("an older store must still load");
        assert_eq!(store.boards[0].probe_serial.as_deref(), Some("000852006107"));
        assert_eq!(store.boards[0].hardware_id.as_deref(), Some("deadbeefcafef00d"));
        assert_eq!(store.boards[0].confirmed_at_utc_ms, Some(1_755_000_000_000));
    }
}
