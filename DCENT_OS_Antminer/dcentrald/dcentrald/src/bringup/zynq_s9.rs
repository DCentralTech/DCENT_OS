//! S9 (Zynq am1 / BM1387) hardware bring-up.
//!
//! W2.1 stage-1 placeholder (2026-05-07). The canonical bring-up site is
//! still `daemon.rs::Daemon::init()` (lines ~4904..7649). Lifting it out
//! cleanly requires:
//!
//!   1. A `MiningServices` aggregator type (W2.3 next wave) that bundles
//!      the I2C service handle, voltage command channel, heartbeat
//!      cancellation token, fan controller Arc, and watchdog handle.
//!   2. A test fixture for the PIC-heartbeat thread that doesn't require
//!      live AXI IIC. The current `daemon.rs` heartbeat spawn path uses
//!      `dcentrald_hal::i2c::I2cServiceHandle` directly, which has no
//!      mockable trait surface.
//!   3. Confirmation that the 5-stable-tick deferred-voltage gate
//! survives a
//!      module-boundary move — currently the gate is implemented as a
//!      `stable_heartbeat_ticks: AtomicU32` shared between the heartbeat
//!      thread and the voltage drain loop.
//!
//! The S9 cold-boot sequence (verified 2026-04-19 sustained mining) is:
//!
//! ```text
//!   Phase 0: Emergency PIC heartbeats (3× to each of 0x55..=0x57)
//!   Phase 1: I2C service spawn (single owner of /dev/i2c-0)
//!   Phase 2: FPGA UIO open (uio0=fan, uio1..=12=chains)
//!   Phase 3: PIC firmware detect (0xCC=bootloader, 0x60=app, then JUMP)
//!   Phase 4: Heartbeat thread spawn (1 Hz, 5-tick stability gate)
//!   Phase 5: Set voltage 9.4V (initial safe — gated on 5 stable ticks)
//!   Phase 6: FPGA UART baud = 115200 (divisor 0x6C)
//!   Phase 7: Reset hash board, wait 4 s
//!   Phase 8: Chip enumeration (GetAddress 0x04, BM1387 CMD)
//!   Phase 9: Address assignment (ChainInactive 0x05 ×3 + SetChipAddress)
//!   Phase 10: Set PLL frequency at 115200 baud BEFORE baud upgrade
//!   Phase 11: MiscCtrl triple-write (gate_block=1) + baud upgrade
//!   Phase 12: FPGA baud upgrade: divisor 0x07 → 1.5625 Mbaud
//!   Phase 13: Set TicketMask AFTER baud upgrade
//!   Phase 14: Open-core 114 dummy work items, 10 ms spacing
//!   Phase 15: MiscCtrl triple-write 0x00200180 (gate_block=0)
//!   Phase 16: Reduce voltage 9.4V → ~9.1V
//!   Phase 17: Start temperature watchdog
//!   Phase 18: Preheat to target temp (89°C)
//!   Phase 19: Frequency ramp in 5 MHz steps
//!   Phase 20: Enter autotuner
//! ```
//!
//! Until W2.3 lands, treat this module as the cross-reference doc that
//! tells future agents WHERE the S9 bring-up code lives, even though it
//! has not yet been physically relocated.

/// Future entry point. Placeholder until W2.3 ships the `MiningServices`
/// aggregator. Today, the S9 bring-up runs inline inside
/// [`crate::daemon::Daemon::run`] / `init`.
#[allow(dead_code)]
pub fn _placeholder_bringup_signature() {
    // intentionally empty — present only so this module isn't an empty
    // file and downstream agents see the planned signature.
}
