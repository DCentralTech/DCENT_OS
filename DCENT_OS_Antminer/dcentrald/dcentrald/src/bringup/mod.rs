//! Per-platform hardware bring-up sequences.
//!
//! W2.1 stage-1 carve-up (2026-05-07): the actual init code still lives in
//! `daemon.rs::Daemon::init()` (~3500 LOC, lines 4904..7649) and in the
//! per-mode `*Miner::run()` paths because each path owns a tightly coupled
//! single-I²C-owner service handle, PIC-heartbeat thread spawn, FPGA UIO
//! map, voltage-ramp gate, and 5-stable-tick deferred-voltage gate that
//! are easy to break with a mechanical extraction.
//!
//! What lives here today:
//!   - [`zynq_s9`]   — module-level documentation pointing to the canonical
//!                     S9 bring-up site in `daemon.rs::init()`.
//!   - [`zynq_am2_passthrough`] — same for the am2 passthrough branches.
//!
//! What lives here AFTER W2.1 stage-2 (next wave):
//!   - `zynq_s9::bringup(daemon: &mut Daemon, services: &MiningServices) -> Result<...>`
//!     that owns the S9 cold-boot 20-step sequence (PIC heartbeat init,
//!     FPGA UIO open, FIFO reset, address assignment, open-core, voltage
//!     ramp). This needs a `MiningServices` aggregator (W2.3) before it
//!     can be lifted out cleanly.
//!   - `zynq_am2_passthrough::bringup(...)` for the am2 PSU + dsPIC +
//!     BM1362 cold-boot path.
//!
//! Invariants that MUST be preserved across any future split (memory rules):
//!   -  — 7 RULES of PIC heartbeat.
//!     Voltage commands MUST gate behind 5 stable ticks. Parser flush MUST
//!     fire on every NACK.
//!   -  — MiscCtrl triple-write
//!     with 5 ms spacing. Both S9 (BM1387) and am2 (BM1362) require it.
//!   -  — AXI IIC THIGH/TLOW=1498, TBUF=499.
//!   -  — never RPM=0 when fans spin.
//!   -  — `0x50..=0x57` write-denied.
//!   - Single-I2C-owner architecture mandatory on am2.
//!   - `panic = "abort"` required for S9 release profile.
//!   - NEVER write 0 to FPGA `CTRL_REG`.

pub mod zynq_am2_passthrough;
pub mod zynq_s9;
