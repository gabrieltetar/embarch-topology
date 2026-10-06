//! Attaching a probe without rewriting a running target's clock enables (decision 39).
//!
//! probe-rs's sequence for ARMv6 STM32s (F0, L0, G0) read-modify-writes the RCC register holding
//! the DBGMCU clock enable, and DBGMCU's control register, both when it attaches and when the
//! session ends, whatever the core is doing. On an STM32G0 idling in WFI with no DMA clock on, the
//! bus matrix clock is off during Sleep and a debug read of any system-bus address returns 0 or
//! the previous word with an OK acknowledge (the G0 reference manual, debug chapter), so that read-modify-write writes
//! garbage back: on 2026-10-06 it wrote `0x00000E80` into a running DUT's `RCC_APBENR1` and
//! switched off its USB, PWR and I2C clocks until a reset.
//!
//! [`attach`] swaps that sequence for [`GuardedStm32Armv6`], which does the same writes only while
//! the core is halted (when a debug read is always good) and leaves a running core's RCC and
//! DBGMCU alone. What is given up: probe-rs no longer turns on debug in Stop/Standby on a running
//! board, which no caller here relies on — they read an ID, flash, or reset.

use std::sync::Arc;

use probe_rs::architecture::arm::memory::ArmMemoryInterface;
use probe_rs::architecture::arm::sequences::ArmDebugSequence;
use probe_rs::architecture::arm::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};
use probe_rs::config::{DebugSequence, Registry};
use probe_rs::probe::Probe;
use probe_rs::{CoreType, Permissions, Session};

/// DHCSR, readable over the debug port even while the core sleeps (it is not on the system bus).
const DHCSR: u64 = 0xE000_EDF0;
const DHCSR_S_HALT: u32 = 1 << 17;
const RCC: u64 = 0x4002_1000;
const DBGMCU_CR: u64 = 0x4001_5804;
const DBG_STOP: u32 = 1 << 1;
const DBG_STANDBY: u32 = 1 << 2;

/// The ARMv6 STM32 families probe-rs gives its read-modify-write sequence to, by the chip-name
/// prefixes it uses (`probe-rs` 0.31, `vendor/st/mod.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    F0,
    L0,
    G0,
}

impl Family {
    pub(crate) fn of(chip: &str) -> Option<Self> {
        let up = chip.to_ascii_uppercase();
        if up.starts_with("STM32F0") {
            Some(Family::F0)
        } else if up.starts_with("STM32L0") {
            Some(Family::L0)
        } else if up.starts_with("STM32G0") {
            Some(Family::G0)
        } else {
            None
        }
    }

    /// The RCC register holding the DBGMCU clock enable, and that bit: APB2ENR bit 22 on F0
    /// (offset 0x18) and L0 (0x34), APBENR1 bit 27 on G0 (0x3C).
    pub(crate) fn dbg_clock_enable(self) -> (u64, u32) {
        match self {
            Family::F0 => (RCC + 0x18, 1 << 22),
            Family::L0 => (RCC + 0x34, 1 << 22),
            Family::G0 => (RCC + 0x3C, 1 << 27),
        }
    }
}

pub(crate) fn with_bits(value: u32, bits: u32, on: bool) -> u32 {
    if on {
        value | bits
    } else {
        value & !bits
    }
}

/// probe-rs's ARMv6 STM32 sequence, doing its RCC and DBGMCU writes only on a halted core.
#[derive(Debug)]
pub(crate) struct GuardedStm32Armv6 {
    family: Family,
}

impl GuardedStm32Armv6 {
    fn set_debug_in_low_power(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        on: bool,
        when: &str,
    ) -> Result<(), ArmError> {
        if memory.read_word_32(DHCSR)? & DHCSR_S_HALT == 0 {
            tracing::info!(
                "{when}: core running, leaving RCC and DBGMCU as they are (a debug read of them may \
                 not be good while it sleeps; embarch-topology decision 39)"
            );
            return Ok(());
        }
        let (enr, bit) = self.family.dbg_clock_enable();
        let value = memory.read_word_32(enr)?;
        memory.write_word_32(enr, with_bits(value, bit, on))?;
        let cr = memory.read_word_32(DBGMCU_CR)?;
        memory.write_word_32(DBGMCU_CR, with_bits(cr, DBG_STOP | DBG_STANDBY, on))
    }
}

impl ArmDebugSequence for GuardedStm32Armv6 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        self.set_debug_in_low_power(&mut *memory, true, "debug_device_unlock")
    }

    fn debug_core_stop(&self, memory: &mut dyn ArmMemoryInterface, _core_type: CoreType) -> Result<(), ArmError> {
        self.set_debug_in_low_power(memory, false, "debug_core_stop")
    }
}

/// `probe.attach(chip, permissions)`, with an ARMv6 STM32's debug sequence swapped for
/// [`GuardedStm32Armv6`]. Every other chip attaches exactly as probe-rs would.
pub fn attach(probe: Probe, chip: &str, permissions: Permissions) -> Result<Session, probe_rs::Error> {
    let Some(family) = Family::of(chip) else {
        return probe.attach(chip, permissions);
    };
    let mut target = Registry::from_builtin_families().get_target_by_name(chip)?;
    target.debug_sequence = DebugSequence::Arm(Arc::new(GuardedStm32Armv6 { family }));
    probe.attach(target, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_families_probe_rs_gives_the_read_modify_write_sequence_are_guarded() {
        assert_eq!(Family::of("STM32G0B1VE"), Some(Family::G0));
        assert_eq!(Family::of("stm32g0b1retx"), Some(Family::G0));
        assert_eq!(Family::of("STM32F030C8"), Some(Family::F0));
        assert_eq!(Family::of("STM32L073RZ"), Some(Family::L0));
        assert_eq!(Family::of("STM32G474RE"), None);
        assert_eq!(Family::of("nRF54L15"), None);
    }

    #[test]
    fn the_clock_enable_is_the_same_register_and_bit_probe_rs_uses() {
        assert_eq!(Family::G0.dbg_clock_enable(), (0x4002_103C, 1 << 27));
        assert_eq!(Family::F0.dbg_clock_enable(), (0x4002_1018, 1 << 22));
        assert_eq!(Family::L0.dbg_clock_enable(), (0x4002_1034, 1 << 22));
    }

    #[test]
    fn bits_are_set_and_cleared_without_touching_the_rest() {
        // The value a running G0's APBENR1 really held on 2026-10-06, and the
        // garbage probe-rs wrote over it.
        assert_eq!(with_bits(0x166F_2000, 1 << 27, true), 0x1E6F_2000);
        assert_eq!(with_bits(0x1E6F_2000, 1 << 27, false), 0x166F_2000);
        assert_eq!(with_bits(0, DBG_STOP | DBG_STANDBY, true), 0x6);
    }
}
