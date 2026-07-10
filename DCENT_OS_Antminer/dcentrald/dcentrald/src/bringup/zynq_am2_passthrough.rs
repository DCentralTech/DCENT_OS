//! S19 Pro / S19j Pro (am2 Zynq + BM1398/BM1362) passthrough bring-up.
//!
//! W2.1 stage-1 placeholder (2026-05-07). The actual am2 passthrough
//! init lives today inside `daemon.rs::Daemon::init()` (am2 branches)
//! and `s19j_hybrid_mining.rs::S19jHybridMiner::run()`. Both will
//! collapse into a single `bringup::zynq_am2_passthrough::bringup(...)`
//! once the W2.3 `MiningServices` aggregator + dsPIC driver split land.
//!
//! ## Why am2 is its own module
//!
//! - **PSU controller is APW121215a** (not S9's PIC16F1704). Voltage
//!   feedback comes from PMBus or live serial probes, not from the
//!   chain-PIC heartbeat.
//! - **dsPIC voltage controllers at I²C 0x20..=0x22** (not S9's 0x55..=0x57).
//!   Framed protocol with SUM checksum.
//! - **EEPROM range 0x50..=0x57 is WRITE-DENIED at the HAL** by per-bus
//!   denylist. S9 leaves
//!   the denylist empty because S9's 0x55..=0x57 are PICs, not EEPROMs.
//! - **Single-I2C-owner architecture mandatory** — one process holds
//!   `/dev/i2c-0`. Daemon, hybrid, and serial paths all route through
//!   `dcentrald_hal::i2c::I2cServiceHandle`. Direct `I2cBus::open(0)` is
//!   forbidden on am2.
//! - **Chain UART is `/dev/ttyS{1,2,3,4}` at 3.125 Mbaud post-MiscCtrl**,
//!   not FPGA WORK_TX_FIFO. Work dispatch is serial, not register-pump.
//! - **fw=0x86 dsPICs are refused for voltage commands by default**
//!. Lab override is the env var
//!   `DCENT_AM2_TRUST_DEGRADED_FW=1`.
//!
//! ## Open-core is not used on BM1362
//!
//! Unlike BM1387 (S9, requires 114 dummy-work packets with `gate_block=1`),
//! BM1362 activates cores via per-chip register writes during init. ESP-Miner
//! BM1366/1368/1370 drivers confirm — zero `open_core`/`gate_block` symbols.
//! See `dcentrald-asic::drivers::bm1362` module docs for the canonical
//! activation sequence and `dcentrald-asic::bm1362::cold_boot_step` for
//! the byte-pinned wire constants and frame builders shipped in W2.5.
//!
//! The actual orchestration `cold_boot()` async function is deferred —
//! both `serial_mining.rs::init_asic_chain` and
//! `s19j_hybrid_mining.rs::init_asic_chain` carry env-flag-laden
//! diagnostic scaffolding (`DCENT_BM1362_*` overrides, am2 UART relay
//! mirroring, post-power reset toggles) that needs hardware-co-located
//! testing before unification. The byte sequences themselves are
//! already locked by the `dcentrald-asic::bm1362` test suite, so any
//! future hoist can land without silently changing wire bytes.

/// Future entry point. Placeholder until W2.3 ships the `MiningServices`
/// aggregator and the dsPIC driver split. Today, the am2 bring-up runs
/// inline across `daemon.rs` (am2 passthrough branches) and
/// `s19j_hybrid_mining.rs`.
#[allow(dead_code)]
pub fn _placeholder_bringup_signature() {
    // intentionally empty — see module docs.
}
