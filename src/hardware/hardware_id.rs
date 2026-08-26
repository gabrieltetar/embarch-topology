//! Vendor-specific, per-chip-family readback of a target's factory-burned
//! unique ID — the live signal [`super::validate`]'s board-identity check
//! cross-checks against [`super::enrollment`], since it survives a probe
//! getting physically moved to a different board in a way a bare USB serial
//! number can't. Formerly `embarch-core`'s own `hardware_id.rs`, moved here
//! unchanged (design.md §3 decisions 2, 4).
//!
//! Only the two chip families this suite's real hardware actually uses are
//! implemented. An unrecognized chip is a named error, never a guess.

use anyhow::{Context, Result};
use probe_rs::{Core, MemoryInterface};

/// Nordic classic nRF5x/nRF9x series: `FICR.DEVICEID[0..1]`.
const NRF5X_FICR_DEVICEID: [u64; 2] = [0x1000_0060, 0x1000_0064];

/// Nordic nRF54L series (nRF54L15/nRF54L10/nRF54L05, and the nRF54LM20A):
/// `FICR.INFO.DEVICEID[0..1]`. Sourced from a real user's report of a
/// working read at `0xFFC304` (Nordic DevZone), `0xFFC308` following the
/// same two-word stride the classic layout above uses.
const NRF54L_FICR_INFO_DEVICEID: [u64; 2] = [0x00FF_C304, 0x00FF_C308];

/// ESP32-C5: `EFUSE_RD_MAC_SYS0_REG`/`EFUSE_RD_MAC_SYS1_REG`, the
/// factory-programmed base MAC address.
const ESP32C5_EFUSE_MAC_SYS0: u64 = 0x600B_4844;
const ESP32C5_EFUSE_MAC_SYS1: u64 = 0x600B_4848;

/// Reads `chip`'s factory-unique hardware ID over an already-attached
/// `core`, formatted as a lowercase hex string.
pub fn read(core: &mut Core<'_>, chip: &str) -> Result<String> {
    match chip {
        "nRF54L15" | "nRF54L10" | "nRF54L05" | "nRF54LM20A" => {
            read_two_words(core, NRF54L_FICR_INFO_DEVICEID, "FICR.INFO.DEVICEID")
        }
        c if c.starts_with("nRF5") || c.starts_with("nRF9") => {
            read_two_words(core, NRF5X_FICR_DEVICEID, "FICR.DEVICEID")
        }
        "esp32c5" => {
            let sys0 = core
                .read_word_32(ESP32C5_EFUSE_MAC_SYS0)
                .context("failed to read EFUSE_RD_MAC_SYS0_REG")?;
            let sys1 = core
                .read_word_32(ESP32C5_EFUSE_MAC_SYS1)
                .context("failed to read EFUSE_RD_MAC_SYS1_REG")?;
            Ok(format!("{sys0:08x}{sys1:08x}"))
        }
        other => anyhow::bail!(
            "no hardware-id readback implemented for chip '{other}' — enrollment/gating only \
             covers Nordic nRF5x/nRF9x/nRF54L and Espressif esp32c5 today"
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
/// read over JTAG (`embarch-core/design.md` §3 decision 35).
///
/// **Why this needs a relation at all, rather than string equality.** The two
/// come from different mechanisms and are not obliged to agree byte for byte:
/// [`read`] reads specific vendor registers at addresses this module
/// declares, while a board self-reports through Zephyr's `hwinfo_get_device_id`,
/// whose bytes and their order are a per-SoC *driver* decision. They describe
/// the same silicon; they need not spell it the same way.
///
/// **`esp32c5` has a declared relation; nothing else does.** An arm here is
/// only writable when the transform is *derivable*, not guessed — and for
/// this part it is, because both sides turn out to read the identical two
/// registers. Zephyr's `hwinfo_esp32.c` ESP32-C5 branch reads
/// `EFUSE_RD_MAC_SYS0_REG`/`EFUSE_RD_MAC_SYS1_REG`, which resolve to
/// `0x600B4800 + 0x44`/`+ 0x48` — [`ESP32C5_EFUSE_MAC_SYS0`] and
/// [`ESP32C5_EFUSE_MAC_SYS1`] exactly. It then emits the six base-MAC bytes
/// in a fixed order, dropping `sys1`'s top 16 bits (a checksum). That is a
/// deterministic, lossy projection of the JTAG-read pair, and
/// [`esp32c5_expected_self_report`] is it.
///
/// Every other chip returns [`SelfReportedIdentity::Undeclared`], which is
/// **not** a pass: a comparison that could not be made is not a comparison
/// that succeeded. Writing an arm for one requires the same thing this one
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
    match chip {
        "esp32c5" => match esp32c5_expected_self_report(jtag_read) {
            Some(expected) if self_reported.eq_ignore_ascii_case(&expected) => {
                SelfReportedIdentity::Match
            }
            Some(_) => SelfReportedIdentity::Mismatch,
            // The JTAG string isn't the shape `read` produces, so there is
            // nothing to project. Undeclared rather than Mismatch: the fault
            // is on this side, and refusing a board over it would be wrong.
            None => SelfReportedIdentity::Undeclared,
        },
        _ => SelfReportedIdentity::Undeclared,
    }
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

fn read_two_words(core: &mut Core<'_>, addresses: [u64; 2], name: &str) -> Result<String> {
    let a = core
        .read_word_32(addresses[0])
        .with_context(|| format!("failed to read {name}[0] at {:#x}", addresses[0]))?;
    let b = core
        .read_word_32(addresses[1])
        .with_context(|| format!("failed to read {name}[1] at {:#x}", addresses[1]))?;
    Ok(format!("{a:08x}{b:08x}"))
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
        // The whole point of decision 35: the runtime serial link and the
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

    #[test]
    fn unrecognized_chip_is_a_named_error_not_a_guess() {
        let chip = "STM32F407VG";
        let matched = matches!(chip, "nRF54L15" | "nRF54L10" | "nRF54L05" | "nRF54LM20A")
            || chip.starts_with("nRF5")
            || chip.starts_with("nRF9")
            || chip == "esp32c5";
        assert!(!matched, "STM32F407VG should fall through to the unrecognized-chip error arm");
    }
}
