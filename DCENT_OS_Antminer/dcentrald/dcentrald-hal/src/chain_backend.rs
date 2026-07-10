//! `Bm1397PlusChainBackend` — chain transport abstraction for BM1362-family chips.
//!
//!  (2026-05-23) added a transport abstraction while investigating
//! `a lab unit` chain-enum-0. Later live evidence corrected the FIFO hypothesis:
//! `a lab unit` command/init traffic is on PL UARTs (`/dev/ttyS1` + `/dev/ttyS3`),
//! with OUT2-gated MCR on the active bitstream. The FPGA FIFO transport stays
//! in-tree as a default-off experimental/backend boundary, not the current
//! `a lab unit` default command path.
//!
//! This trait is the abstraction boundary. `init_asic_chain` in
//! `s19j_hybrid_mining.rs` dispatches between the two impls at runtime:
//!
//! - [`crate::serial_chain::SerialChainBackend`] — the proven `a lab unit`
//!   2026-05-15-first-shares baseline (PL UART path).
//! - [`crate::fpga_chain_backend::FpgaChainBackend`] — the default-off
//!   experimental FIFO backend for UIO/devmem FIFO work.
//!
//! Selection is env-gated by `DCENT_AM2_USE_FPGA_CHAIN` (default off →
//! `a lab unit` baseline preserved byte-identical when the flag is unset).
//!
//! Per IMPLEMENTATION-PLAN.md §4: 14-method surface covering baud/framing,
//! BM1397+ command issuance, response collection, work dispatch, and
//! transport identity. The trait deliberately mirrors
//! `SerialChainBackend`'s public method names so the Phase-1 impl for
//! `SerialChainBackend` is a zero-logic shim.

use crate::Result;

/// Chain transport for BM1397/BM1398/BM1362/BM1366/BM1368-family ASICs.
///
/// All command-issuing methods send the raw BM1397+ wire payload (header +
/// length + body). The implementation is responsible for adding preamble +
/// CRC5 (serial path) or LSB-first byte packing into a 32-bit FIFO word
/// (FPGA-FIFO path).
pub trait Bm1397PlusChainBackend: Send + Sync {
    // ---- baud / framing ---------------------------------------------------

    /// Set the chain UART baud rate. On the serial path this writes the
    /// 16550 divisor latch; on the FPGA path this writes `REG_BAUD` (the
    /// FPGA cmd UART's baud divider register).
    fn set_baud_rate(&self, baud: u32) -> Result<()>;

    /// Configure the expected BM1397+ response body length (bytes after
    /// the `[0xAA, 0x55]` preamble). 7 for the BM139x / BM1362 family.
    fn set_response_body_len(&self, body_len: usize) -> Result<()>;

    // ---- BM1397+ commands -------------------------------------------------

    /// Broadcast GetAddress (header 0x52). Causes every chip in the chain
    /// to reply with a 7-byte response carrying its `chip_id`.
    fn send_get_address_bm1397plus(&self) -> Result<()>;

    /// Broadcast ChainInactive (header 0x53). Prepares the chain to accept
    /// per-chip address assignment.
    fn send_chain_inactive_bm1397plus(&self) -> Result<()>;

    /// Single-chip SetChipAddress (header 0x40). `addr` is the address to
    /// assign — by convention `(chip_index * 4)` so addresses end up at
    /// 0x00 / 0x04 / 0x08 / ... up to 0xF8 for a 63-chip chain.
    fn send_set_address_bm1397plus(&self, addr: u8) -> Result<()>;

    /// Broadcast register write (header 0x51). Writes `value` to register
    /// `reg` on every chip on the chain.
    fn send_write_reg_broadcast_bm1397plus(&self, reg: u8, value: u32) -> Result<()>;

    /// Single-chip register write (header 0x41). Writes `value` to
    /// register `reg` on the chip currently at address `chip_addr`.
    fn send_write_reg_bm1397plus(&self, chip_addr: u8, reg: u8, value: u32) -> Result<()>;

    /// Single-chip register read (header 0x42). Causes the addressed chip
    /// to reply with the contents of register `reg`.
    fn send_read_reg_bm1397plus(&self, chip_addr: u8, reg: u8) -> Result<()>;

    // ---- response / nonce collection -------------------------------------

    /// Read a single response frame (timeout in ms). Returns the number of
    /// body bytes (after preamble) copied into `out`. 0 = no frame within
    /// the timeout.
    fn read_response_frame(&self, out: &mut [u8], timeout_ms: u64) -> Result<usize>;

    /// Collect every response frame that arrives within `max_wait_ms`.
    /// Used by GetAddress chip enumeration where the host expects up to
    /// `N` chip responses back-to-back.
    fn read_all_responses(&self, max_wait_ms: u64) -> Result<Vec<Vec<u8>>>;

    // ---- work dispatch + nonce path --------------------------------------

    /// Push a fully-framed work item to the chain (preamble + 86 bytes +
    /// CRC16 on the serial path; raw LSB-first 32-bit words to the FPGA
    /// work-tx FIFO on the FPGA path).
    fn send_work_frame(&self, frame: &[u8]) -> Result<()>;

    /// Poll for a single nonce frame (timeout in ms). Returns 0 if no
    /// frame within the timeout.
    fn poll_nonce_frame(&self, out: &mut [u8], timeout_ms: u64) -> Result<usize>;

    // ---- transport identity ----------------------------------------------

    /// Chain ID (0-indexed). Used in log lines + the FPGA UIO name
    /// lookup (`chain1-*` for chain 0, `chain2-*` for chain 1, etc.).
    fn chain_id(&self) -> u8;

    /// Short transport label for log lines + telemetry. Stable across
    /// versions — current values: `"serial-devmem-uart"`,
    /// `"serial-kernel-uart"`, `"fpga-fifo-uio"`, `"fpga-fifo-devmem"`.
    fn transport_label(&self) -> &'static str;
}
