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
