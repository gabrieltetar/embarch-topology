//! Vendor-specific, per-chip-family readback of a target's factory-burned
//! unique ID — the live signal [`super::validate`]'s board-identity check
//! cross-checks against [`super::enrollment`], since it survives a probe
//! getting physically moved to a different board in a way a bare USB serial
//! number can't. Formerly `embarch-core`'s own `hardware_id.rs`, moved here
//! unchanged (decisions 2, 4).
//!
//! Only the chip families this suite's real hardware actually uses are
//! implemented — Nordic (classic and nRF54L), Espressif ESP32-C5, and
//! STM32G0. An unrecognized chip is a named error, never a guess.

use anyhow::{Context, Result};
use probe_rs::{Core, MemoryInterface};

/// Nordic classic nRF5x/nRF9x series: `FICR.DEVICEID[0..1]`.
const NRF5X_FICR_DEVICEID: [u64; 2] = [0x1000_0060, 0x1000_0064];

/// Nordic nRF54L series (nRF54L15/nRF54L10/nRF54L05, and the nRF54LM20A):
/// `FICR.INFO.DEVICEID[0..1]`. Sourced from a real user's report of a
/// working read at `0xFFC304` (Nordic DevZone), `0xFFC308` following the
/// same two-word stride the classic layout above uses.
const NRF54L_FICR_INFO_DEVICEID: [u64; 2] = [0x00FF_C304, 0x00FF_C308];

/// STM32G0: the 96-bit factory unique device ID, three consecutive words
/// from `UID_BASE`. **Deliberately G0-only, not `stm32`-wide** — `UID_BASE`
/// is per-family on STM32 (G0/L4 at `0x1FFF_7590`, F4 at `0x1FFF_7A10`,
/// H7 at `0x1FF1_E800`), so a `starts_with("stm32")` arm would read a
/// plausible-looking word out of whatever happens to live at this address
/// on another family. Evidence is the vendor HAL rather than a datasheet
/// reading: every `stm32g0*xx.h` in `hal_stm32`'s `stm32cube/stm32g0xx/soc/`
/// — `stm32g0b1xx.h` (the part this was added for) included — defines
/// `UID_BASE (0x1FFF7590UL)`, and Zephyr's `hwinfo_stm32.c` reads its device
/// ID as three `LL_GetUID_Word*` reads from that same base.
const STM32G0_UID: [u64; 3] = [0x1FFF_7590, 0x1FFF_7594, 0x1FFF_7598];

/// ESP32-C5: `EFUSE_RD_MAC_SYS0_REG`/`EFUSE_RD_MAC_SYS1_REG`, the
/// factory-programmed base MAC address.
const ESP32C5_EFUSE_MAC_SYS0: u64 = 0x600B_4844;
const ESP32C5_EFUSE_MAC_SYS1: u64 = 0x600B_4848;

/// A chip's family, as far as hardware-id readback cares: which register
/// pair [`read`] gets it from, and (via [`is_nordic_deviceid_chip`]) whether
/// a self-reported ID has a derivable relation to that pair at all. One
/// classifier decides both questions so they cannot disagree — see topology
/// decision 22 and task `topology/007`, which this type closes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChipFamily {
    /// nRF54L*: `FICR.INFO.DEVICEID[0..1]`. **nRF54H is deliberately not
    /// here** — see [`classify_chip`].
    Nrf54InfoDeviceId,
    /// Classic Nordic nRF5x/nRF9x: `FICR.DEVICEID[0..1]`.
    NrfClassicDeviceId,
    /// Espressif ESP32-C5.
    Esp32C5,
    /// STM32G0: the 96-bit `UID_BASE` triple. See [`STM32G0_UID`] for why
    /// this variant names one STM32 family rather than the vendor.
    Stm32G0Uid,
}

/// Classifies `chip` by name. The nRF54L check runs first and is
/// case-insensitive — `nRF54L47`, `nRF54LM10`, and a lowercase/suffixed
/// `nrf54l15_cpuapp` all land here rather than falling through to the
/// classic `starts_with("nRF5")` arm the way a name one character off used
/// to (that arm matches `"nRF54..."` too, since `"nRF54"` starts with
/// `"nRF5"`). `embarch-core`'s `flash_backend.rs` makes the same
/// case-insensitive `nrf54l` decision for the same reason, one repo over.
///
/// **It does not, however, stop where this function stops, and this comment
/// used to say it did.** `embarch-core`'s `requires_vendor_tool` matches
/// `nrf54h` as well (its decision 49), because an unmatched nRF54H name was
/// falling through to `false` and reaching probe-rs unannounced. Both repos
/// refuse nRF54H, in opposite directions: this function abstains (`None`,
/// "I cannot say which register pair holds its ID"), while
/// `requires_vendor_tool` positively asserts ("keep probe-rs away"). The
/// two matchers stay separate on purpose — see topology decision 25 and
/// `tasks/suite/024` — because they answer different questions and share no
/// return type that serves both.
///
/// **nRF54H returns `None` on purpose, and it is checked before the classic
/// prefix so it cannot reach either register pair by accident.** Decision
/// 21's evidence for `FICR.INFO.DEVICEID` at `0x00FF_C304`/`0x00FF_C308` is
/// entirely nRF54L — three nRF54L15s read over JTAG and one HAL cross-check
/// — and says nothing about the Haltium family; nothing in this repo or in
/// `embarch-core` establishes either address on an nRF54H part, and no such
/// silicon has ever been on this bench. Sending it to the nRF54L pair and
/// sending it to the classic pair are both guesses, and this file's rule is
/// that an unrecognized chip is a named error, never a guess. When an
/// nRF54H is enrolled, the way to add it is a register read on real
/// silicon, not a prefix.
///
/// An unrecognized chip returns `None` — a named error, never a guess.
fn classify_chip(chip: &str) -> Option<ChipFamily> {
    if chip == "esp32c5" {
        return Some(ChipFamily::Esp32C5);
    }
    let c = chip.to_ascii_lowercase();
    if c.starts_with("nrf54h") {
        // Unevidenced on both pairs; see this function's doc comment.
        None
    } else if c.starts_with("nrf54l") {
        Some(ChipFamily::Nrf54InfoDeviceId)
    } else if c.starts_with("nrf5") || c.starts_with("nrf9") {
        Some(ChipFamily::NrfClassicDeviceId)
    } else if c.starts_with("stm32g0") {
        // Narrow on purpose: the prefix stops at the family, not at `stm32`,
        // because `UID_BASE` moves between STM32 families. Every other STM32
        // still reaches the `None` arm below, which is the named error this
        // file's rule asks for rather than a read at a guessed address.
        Some(ChipFamily::Stm32G0Uid)
    } else {
        None
    }
}

/// Reads `chip`'s factory-unique hardware ID over an already-attached
/// `core`, formatted as a lowercase hex string.
pub fn read(core: &mut Core<'_>, chip: &str) -> Result<String> {
    match classify_chip(chip) {
        Some(ChipFamily::Nrf54InfoDeviceId) => {
            read_words(core, &NRF54L_FICR_INFO_DEVICEID, "FICR.INFO.DEVICEID")
        }
        Some(ChipFamily::NrfClassicDeviceId) => {
            read_words(core, &NRF5X_FICR_DEVICEID, "FICR.DEVICEID")
        }
        Some(ChipFamily::Stm32G0Uid) => read_words(core, &STM32G0_UID, "UID"),
        Some(ChipFamily::Esp32C5) => {
            let sys0 = core
                .read_word_32(ESP32C5_EFUSE_MAC_SYS0)
                .context("failed to read EFUSE_RD_MAC_SYS0_REG")?;
            let sys1 = core
                .read_word_32(ESP32C5_EFUSE_MAC_SYS1)
                .context("failed to read EFUSE_RD_MAC_SYS1_REG")?;
            Ok(format!("{sys0:08x}{sys1:08x}"))
        }
        None => anyhow::bail!(
            "no hardware-id readback implemented for chip '{chip}' — enrollment/gating only \
             covers Nordic nRF5x/nRF9x/nRF54L, Espressif esp32c5 and ST STM32G0 today"
        ),
    }
}

/// How a chip's **self-reported** identity — what firmware running on the
/// board says about itself — relates to the identity [`read`] gets over
/// JTAG. See [`compare_self_reported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfReportedIdentity {
    /// The two name the same chip.
    Match,
    /// The two name different chips — the board on the runtime link is not
    /// the board the probe is attached to.
    Mismatch,
    /// The board reported nothing at all (its build has no way to answer).
    NotReported,
    /// No relation is declared for this chip yet, so nothing can be
    /// concluded either way. **Not a match**, and callers must not treat it
    /// as one.
    Undeclared,
}

/// Relates a chip ID a board reported about itself to the one [`read`] just
/// read over JTAG (`embarch-core` decision 35).
///
/// **Why this needs a relation at all, rather than string equality.** The two
/// come from different mechanisms and are not obliged to agree byte for byte:
/// [`read`] reads specific vendor registers at addresses this module
/// declares, while a board self-reports through Zephyr's `hwinfo_get_device_id`,
/// whose bytes and their order are a per-SoC *driver* decision. They describe
/// the same silicon; they need not spell it the same way.
///
/// **Two chip families have a declared relation: `esp32c5`, and the Nordic
/// families [`is_nordic_deviceid_chip`] recognizes.** An arm here is only
/// writable when the transform is *derivable*, not guessed.
///
/// For `esp32c5` it is, because both sides turn out to read the identical two
/// registers. Zephyr's `hwinfo_esp32.c` ESP32-C5 branch reads
/// `EFUSE_RD_MAC_SYS0_REG`/`EFUSE_RD_MAC_SYS1_REG`, which resolve to
/// `0x600B4800 + 0x44`/`+ 0x48` — [`ESP32C5_EFUSE_MAC_SYS0`] and
/// [`ESP32C5_EFUSE_MAC_SYS1`] exactly. It then emits the six base-MAC bytes
/// in a fixed order, dropping `sys1`'s top 16 bits (a checksum). That is a
/// deterministic, lossy projection of the JTAG-read pair, and
/// [`esp32c5_expected_self_report`] is it.
///
/// For the Nordic classic and nRF54L families it is derivable the same way —
/// both sides read the same `DEVICEID` pair, one directly and one through
/// Zephyr's `hwinfo_nrf.c` — and [`nordic_expected_self_report`]'s own doc
/// comment states that derivation in full (topology decision 21). **That
/// relation is declared and derived, not verified across the family it
/// covers**: decision 21's silicon evidence is nRF54L15 only, and
/// `open.md` records that `nRF54L10`/`nRF54L05`/`nRF54LM20A` take this same
/// arm with no silicon ever attached, and that the DUT's own readback has no
/// independent corroboration. A declared relation and a family-wide
/// confirmation are different claims; only the first is made here.
///
/// Every other chip returns [`SelfReportedIdentity::Undeclared`], which is
/// **not** a pass: a comparison that could not be made is not a comparison
/// that succeeded. Writing an arm for one requires the same thing these two
/// had — both implementations' actual register reads, in view at once.
pub fn compare_self_reported(chip: &str, jtag_read: &str, self_reported: &str) -> SelfReportedIdentity {
    if self_reported.is_empty() {
        return SelfReportedIdentity::NotReported;
    }
    // Equality is conclusive for any chip: two mechanisms agreeing on a
    // factory-unique value is not a coincidence a wrong board can produce.
    if self_reported.eq_ignore_ascii_case(jtag_read) {
        return SelfReportedIdentity::Match;
    }
    let expected = match chip {
        "esp32c5" => esp32c5_expected_self_report(jtag_read),
        c if is_nordic_deviceid_chip(c) => nordic_expected_self_report(jtag_read),
        _ => return SelfReportedIdentity::Undeclared,
    };
    match expected {
        Some(expected) if self_reported.eq_ignore_ascii_case(&expected) => {
            SelfReportedIdentity::Match
        }
        Some(_) => SelfReportedIdentity::Mismatch,
        // The JTAG string isn't the shape `read` produces, so there is
        // nothing to project. Undeclared rather than Mismatch: the fault
        // is on this side, and refusing a board over it would be wrong.
        None => SelfReportedIdentity::Undeclared,
    }
}

/// The Nordic chips whose [`read`] arm goes to a `DEVICEID` pair — the exact
/// set for which [`nordic_expected_self_report`]'s derivation holds. Derived
/// from [`classify_chip`] rather than re-matching, so it stays the *same*
/// set `read` handles: a chip [`classify_chip`] adds without a `read` arm
/// (there isn't one) degrades to `Undeclared`, which is safe, and the
/// reverse would silently compare against a projection of a string `read`
/// never produced.
fn is_nordic_deviceid_chip(chip: &str) -> bool {
    matches!(
        classify_chip(chip),
        Some(ChipFamily::Nrf54InfoDeviceId) | Some(ChipFamily::NrfClassicDeviceId)
    )
}

/// Projects a JTAG-read Nordic ID (`{deviceid0:08x}{deviceid1:08x}`, as
/// [`read`] produces for both the classic `FICR.DEVICEID` and the nRF54L
/// `FICR.INFO.DEVICEID` arms) into the string a board running Zephyr reports
/// for itself: **the same 16 hex digits with their two halves swapped.**
///
/// Derived from `zephyr/drivers/hwinfo/hwinfo_nrf.c`, not from observation —
/// the standard [`compare_self_reported`] sets. That driver reads the pair in
/// index order and then deliberately emits it reversed:
///
/// ```text
/// buf[0] = nrf_ficr_deviceid_get(NRF_FICR, 0);
/// buf[1] = nrf_ficr_deviceid_get(NRF_FICR, 1);
/// dev_id.id[0] = sys_cpu_to_be32(buf[1]);   /* DEVICEID[1] first  */
/// dev_id.id[1] = sys_cpu_to_be32(buf[0]);   /* DEVICEID[0] second */
/// ```
///
/// `sys_cpu_to_be32` then `memcpy` is a big-endian byte emission, and
/// `{:08x}` is big-endian nibble order, so each half hex-encodes identically
/// on both sides and only their *order* differs. One `#if` branch covers
/// both `NRF_FICR_HAS_DEVICE_ID` and `NRF_FICR_HAS_INFO_DEVICE_ID`, which is
/// why one relation serves the classic parts and the nRF54L series alike.
///
/// **The limit, stated rather than left implicit:** that file has `#elif`
/// branches falling back to `DEVICEADDR` or the NFC tag header on parts where
/// `DEVICEID` is inaccessible, and those produce something this projection
/// does not describe at all. Every chip [`read`] currently handles takes the
/// `DEVICEID` branch — but a part that did not would come back `Mismatch`
/// here, not `Undeclared`, which is the one way this arm can be wrong. It is
/// the same exposure the `esp32c5` arm carries and is accepted on the same
/// terms: the alternative is refusing to check anything.
///
/// Confirmed live 2026-08-31 on the nRF54L15DK dev bench — JTAG
/// `6fcddc36cb781b71`, self-reported `cb781b716fcddc36`.
fn nordic_expected_self_report(jtag_read: &str) -> Option<String> {
    if jtag_read.len() != 16 || !jtag_read.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("{}{}", &jtag_read[8..], &jtag_read[..8]))
}

/// Projects a JTAG-read `esp32c5` ID (`{sys0:08x}{sys1:08x}`, as [`read`]
/// produces) into the string a board running Zephyr reports for itself.
///
/// Zephyr's `hwinfo_esp32.c` reads the same two registers and assembles the
/// base MAC as `[sys1 >> 8, sys1, sys0 >> 24, sys0 >> 16, sys0 >> 8, sys0]`
/// — so hex-encoded that is `sys1`'s low 16 bits followed by all of `sys0`.
/// `sys1`'s upper 16 bits are a checksum the driver drops, which is why this
/// is a projection rather than a bijection: it maps one way only, and this
/// direction is the one that has the JTAG read to start from.
///
/// `None` when `jtag_read` isn't 16 hex digits — [`read`] always produces
/// exactly that, so this is a malformed-input guard, not a real case.
fn esp32c5_expected_self_report(jtag_read: &str) -> Option<String> {
    if jtag_read.len() != 16 {
        return None;
    }
    let sys0 = u32::from_str_radix(&jtag_read[..8], 16).ok()?;
    let sys1 = u32::from_str_radix(&jtag_read[8..], 16).ok()?;
    Some(format!("{:02x}{:02x}{sys0:08x}", (sys1 >> 8) as u8, sys1 as u8))
}

/// Reads consecutive ID words and concatenates them as lowercase hex, eight
/// digits per word, in the order given.
///
/// **Took a `[u64; 2]` until 2026-09-11**, when STM32G0's 96-bit `UID_BASE`
/// triple needed a third word. Widening it rather than adding a parallel
/// three-word function keeps one definition of what an ID string *is*: for a
/// two-word slice this emits byte-identical output to the old function, so
/// every hardware ID already recorded in `enrollment.toml` still compares
/// equal and no enrolled board needs re-enrolling.
fn read_words(core: &mut Core<'_>, addresses: &[u64], name: &str) -> Result<String> {
    let mut out = String::with_capacity(addresses.len() * 8);
    for (i, &address) in addresses.iter().enumerate() {
        let word = core
            .read_word_32(address)
            .with_context(|| format!("failed to read {name}[{i}] at {address:#x}"))?;
        out.push_str(&format!("{word:08x}"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_ids_match_for_any_chip_without_needing_a_declared_relation() {
        assert_eq!(
            compare_self_reported("esp32c5", "aaaaaaaabbbbbbbb", "aaaaaaaabbbbbbbb"),
            SelfReportedIdentity::Match
        );
        // Case is not part of the identity — one side hex-encodes lowercase
        // by convention, and a driver that upper-cased would still be
        // naming the same silicon.
        assert_eq!(
            compare_self_reported("esp32c5", "aaaaaaaabbbbbbbb", "AAAAAAAABBBBBBBB"),
            SelfReportedIdentity::Match
        );
    }

    /// The real pair, read off the nRF54L15DK dev bench on 2026-08-31 — the
    /// first time this comparison ever ran against Nordic silicon, and the
    /// run that reported `undeclared` because no arm existed.
    #[test]
    fn the_real_nrf54l15_dev_bench_pair_matches() {
        assert_eq!(
            compare_self_reported("nRF54L15", "6fcddc36cb781b71", "cb781b716fcddc36"),
            SelfReportedIdentity::Match
        );
        // The DUT on the same bench is also an nRF54L15, so the arm has to
        // still be able to tell two boards of the same chip apart — which is
        // the entire point of the gate.
        assert_eq!(
            compare_self_reported("nRF54L15", "834f2559f10a6cdf", "cb781b716fcddc36"),
            SelfReportedIdentity::Mismatch
        );
    }

    /// The relation is the same one for the classic parts, because
    /// `hwinfo_nrf.c` serves both families from a single `#if` branch.
    #[test]
    fn the_nordic_relation_covers_the_classic_families_too() {
        for chip in ["nRF52840", "nRF9160", "nRF54L10", "nRF54LM20A"] {
            assert_eq!(
                compare_self_reported(chip, "aabbccdd11223344", "11223344aabbccdd"),
                SelfReportedIdentity::Match,
                "{chip}"
            );
        }
    }

    /// A chip with no declared relation still gets `Undeclared`, and that is
    /// not a pass — the property the new arm must not have weakened.
    #[test]
    fn an_unrelated_chip_is_still_undeclared_not_a_pass() {
        assert_eq!(
            compare_self_reported("stm32f446xx", "aabbccdd11223344", "11223344aabbccdd"),
            SelfReportedIdentity::Undeclared
        );
    }

    #[test]
    fn a_board_that_reported_nothing_is_distinguishable_from_one_that_disagreed() {
        // These must not collapse into one answer: an empty ID means the
        // build cannot answer the question, while a different ID means it
        // answered and the answer was wrong. Only the second is evidence of
        // a wrong board.
        assert_eq!(
            compare_self_reported("esp32c5", "aaaaaaaabbbbbbbb", ""),
            SelfReportedIdentity::NotReported
        );
        assert_eq!(
            compare_self_reported("esp32c5", "aaaaaaaabbbbbbbb", "ccccccccdddddddd"),
            SelfReportedIdentity::Mismatch
        );
    }

    /// Builds the two strings the way the two real implementations build
    /// them, from one pair of register values — so this pins the *relation*
    /// rather than restating `esp32c5_expected_self_report`'s own arithmetic.
    fn esp32c5_pair(sys0: u32, sys1: u32) -> (String, String) {
        // `read`'s own formatting, above.
        let jtag = format!("{sys0:08x}{sys1:08x}");
        // Zephyr `hwinfo_esp32.c`'s own byte assembly, hex-encoded the way
        // dev-bench firmware's `read_hardware_id` encodes it.
        let mac: [u8; 6] = [
            (sys1 >> 8) as u8,
            sys1 as u8,
            (sys0 >> 24) as u8,
            (sys0 >> 16) as u8,
            (sys0 >> 8) as u8,
            sys0 as u8,
        ];
        let self_reported = mac.iter().map(|b| format!("{b:02x}")).collect::<String>();
        (jtag, self_reported)
    }

    #[test]
    fn an_esp32c5_reporting_its_own_base_mac_matches_the_jtag_read_pair() {
        // Both sides read EFUSE_RD_MAC_SYS0/SYS1 — the same two registers at
        // the same two addresses — so this relation is derived, not guessed.
        let (jtag, self_reported) = esp32c5_pair(0x1234_5678, 0xa5a5_9abc);
        assert_eq!(self_reported, "9abc12345678");
        assert_eq!(
            compare_self_reported("esp32c5", &jtag, &self_reported),
            SelfReportedIdentity::Match
        );
    }

    #[test]
    fn a_different_esp32c5_board_on_the_link_is_a_mismatch() {
        // The whole point of `embarch-core` decision 35: the runtime serial link and the
        // JTAG connection are physically separate USB devices, so this is
        // what "two different boards" looks like from Core.
        let (jtag, _) = esp32c5_pair(0x1234_5678, 0xa5a5_9abc);
        let (_, other_board) = esp32c5_pair(0x8765_4321, 0xa5a5_1111);
        assert_eq!(
            compare_self_reported("esp32c5", &jtag, &other_board),
            SelfReportedIdentity::Mismatch
        );
    }

    #[test]
    fn the_checksum_half_of_sys1_is_not_part_of_the_comparison() {
        // hwinfo drops sys1's upper 16 bits, so two JTAG reads differing
        // only there project to the same self-report. Asserted so nobody
        // "fixes" the projection into a bijection it cannot be.
        let (_, a) = esp32c5_pair(0x1234_5678, 0x0000_9abc);
        let (_, b) = esp32c5_pair(0x1234_5678, 0xffff_9abc);
        assert_eq!(a, b);
    }

    #[test]
    fn a_malformed_jtag_read_is_undeclared_rather_than_a_refusal() {
        // The fault would be on Core's side, and refusing a board over it
        // would be blaming the wrong party.
        assert_eq!(
            compare_self_reported("esp32c5", "not-hex", "9abc12345678"),
            SelfReportedIdentity::Undeclared
        );
    }

    #[test]
    fn an_undeclared_relation_is_never_reported_as_a_match() {
        // The whole failure mode this guards: a gate that answers "fine"
        // because it does not know how to answer at all.
        for reported in ["ccccccccdddddddd", "84f703aabbcc", "0"] {
            assert_ne!(
                compare_self_reported("some-future-chip", "aaaaaaaabbbbbbbb", reported),
                SelfReportedIdentity::Match
            );
        }
    }

    /// Doubles as the guard on the STM32G0 arm's narrowness: `UID_BASE` is
    /// per-family on STM32, so an F4 part must still reach the named error
    /// rather than inheriting G0's address.
    #[test]
    fn unrecognized_chip_is_a_named_error_not_a_guess() {
        assert_eq!(
            classify_chip("STM32F407VG"),
            None,
            "STM32F407VG should fall through to the unrecognized-chip error arm"
        );
        assert_eq!(classify_chip("STM32H743ZI"), None);
        assert_eq!(classify_chip("STM32L476RG"), None);
    }

    #[test]
    fn stm32g0_parts_get_the_uid_triple_whatever_the_package() {
        // The two this repo's own boards resolve to (chargerito_core and
        // nucleo_g0b1re both map through Zephyr's `stm32g0b1xx`), plus a
        // lowercase spelling, since callers pass probe-rs's casing and
        // Zephyr's interchangeably.
        assert_eq!(classify_chip("STM32G0B1VE"), Some(ChipFamily::Stm32G0Uid));
        assert_eq!(classify_chip("STM32G0B1RE"), Some(ChipFamily::Stm32G0Uid));
        assert_eq!(classify_chip("stm32g031k8"), Some(ChipFamily::Stm32G0Uid));
    }

    /// The compatibility promise in `read_words`'s doc comment: a two-word
    /// read still produces exactly the 16-hex-digit string already sitting in
    /// `enrollment.toml` for every enrolled Nordic board, so widening the
    /// helper cannot silently invalidate an enrollment.
    #[test]
    fn two_word_ids_keep_their_existing_sixteen_digit_shape() {
        let rendered: String =
            [0x2f77_b9c3u32, 0xf85b_29e9].iter().map(|w| format!("{w:08x}")).collect();
        assert_eq!(rendered, "2f77b9c3f85b29e9");
        assert_eq!(rendered.len(), 16);
    }

    /// A three-word ID is 24 digits — checked so the STM32 arm's output
    /// width is a stated fact rather than an accident of the loop.
    #[test]
    fn three_word_ids_render_twenty_four_digits() {
        let rendered: String = [0x0000_0001u32, 0x0000_0002, 0x0000_0003]
            .iter()
            .map(|w| format!("{w:08x}"))
            .collect();
        assert_eq!(rendered, "000000010000000200000003");
    }

    #[test]
    fn an_unlisted_nrf54l_name_still_gets_the_info_deviceid_pair() {
        // Not one of the four exact names read()/is_nordic_deviceid_chip()
        // used to list, but unmistakably an nRF54L part — it must not fall
        // through to the classic FICR.DEVICEID address.
        assert_eq!(classify_chip("nRF54L47"), Some(ChipFamily::Nrf54InfoDeviceId));
        assert_eq!(classify_chip("nRF54LM10"), Some(ChipFamily::Nrf54InfoDeviceId));
    }

    #[test]
    fn a_lowercase_suffixed_nrf54l_spelling_still_gets_the_info_deviceid_pair() {
        // The Zephyr board-target spelling (`_cpuapp` suffix, all-lowercase)
        // — flash_backend.rs accepts exactly this shape on the flash path.
        assert_eq!(
            classify_chip("nrf54l15_cpuapp"),
            Some(ChipFamily::Nrf54InfoDeviceId)
        );
    }

    #[test]
    fn an_nrf54h_name_is_a_named_error_rather_than_either_guess() {
        // Decision 21's evidence for the INFO.DEVICEID pair is entirely
        // nRF54L; nothing establishes either register pair on a Haltium
        // part. The classic arm's `nrf5` prefix would otherwise swallow it,
        // which is why nRF54H is checked first and refused explicitly
        // rather than merely left off the nRF54L arm.
        assert_eq!(classify_chip("nRF54H20"), None);
        assert_eq!(classify_chip("nrf54h20_cpuapp"), None);
    }

    #[test]
    fn a_classic_nrf5_part_still_gets_the_classic_deviceid_pair() {
        assert_eq!(classify_chip("nRF52840"), Some(ChipFamily::NrfClassicDeviceId));
    }
}
