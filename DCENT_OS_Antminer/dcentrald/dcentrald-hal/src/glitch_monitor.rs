//! `miner-glitch-monitor` UIO driver — Braiins-am2 bitstream only.
//!
//! ## Background (consolidated W13.B1, 2026-05-10 — supersedes )
//!
//! XXX: R4-CONFIRMED — `uart_relay_blocker3_5_analysis.md` §3 (5-source
//! consensus). The Braiins-am2 BraiinsOS bitstream exposes a single FPGA
//! IP at physical address `0x43D00000` named `miner-glitch-monitor` in the
//! device tree. **Stock CV1835 / AM335x BB / Amlogic / S9 hardware does
//! NOT populate this UIO** — that address is an address hole on stock
//! Antminer bitstreams. ONLY the Braiins-am2 custom bitstream populates
//! it. This is critical for platform routing: never gate any control
//! logic on this IP, and never assume it exists outside Braiins-am2.
//!
//! Bound to UIO uio18 on .139 (am2 BraiinsOS). The 4 KB register window
//! houses two distinct register groups:
//!
//! 1. **Glitch counters** at offsets +0x00..+0x0C (uart_rx, scl, sda, ...) —
//!    pure passive read-only telemetry. Zero on a healthy miner.
//!
//! 2. **UART relay status mirrors** at offsets +0x30 / +0x34 — read-only
//!    diagnostic mirrors of BM1362 UART_RELAY candidate state; see
//!    [`dcentrald_asic::bm1362::uart_relay`]. Phase 9 sweep (5 agents, 180
//!    registers) proved zero bits change here during pause. Phase 9A devmem
//!    write tests proved 7 distinct write values are silently rejected from
//!    userspace `devmem`. The mirror reflects what the chip set, not what
//!    the host wants.
//!
//! ## Reclassification (W13.B1, 2026-05-10)
//!
//! Per R4 RE pass, this IP is now classified as a Braiins-am2 diagnostic
//! mirror, NOT a control surface:
//!
//! - BM1362 UART_RELAY evidence points at ASIC reg `0x2C`
//!   (`dcentrald_asic::bm1362::uart_relay`), reachable via the per-chip
//!   serial protocol. R6-7 keeps 0x2C/0x34 candidate broadcasts lab-gated
//!   until live captures confirm exact control semantics.
//! - This module's `force_braiins_glitch_status_mirror_write` API is
//!   diagnostic-only and is expected to fail (silent NO-OP) on stock
//!   hardware. It is gated behind the `am2_force_braiins_glitch_mirror_write`
//!   config flag (lab-only, default `false`).
//!
//! for the full
//! reclassification + rename plan.

use std::fs;
use std::num::NonZeroUsize;

use nix::sys::mman::{MapFlags, ProtFlags};

use crate::uio::UioDevice;
use crate::{HalError, Result};

/// Braiins glitch monitor IP base (Braiins-am2 bitstream only).
///
/// XXX: R4-CONFIRMED — Braiins-am2 ONLY. Stock CV1835/AM335x/AML/S9 do
/// NOT populate this IP. Reads return `0` and writes are silently dropped
/// at the FPGA fabric.
pub const BRAIINS_GLITCH_MONITOR_BASE: u32 = 0x43D0_0000;

/// Backwards-compatible alias for the old constant name.
///
/// Kept so transitive consumers (e.g. lab tooling that imported
/// `GLITCH_MONITOR_BASE_ADDR`) compile after W13.B1.
#[deprecated(
    since = "0.13.0",
    note = "Use BRAIINS_GLITCH_MONITOR_BASE; this is the W13.B1 rename for clarity \
            (Braiins-am2 only, NOT a stock-hardware surface)"
)]
pub const GLITCH_MONITOR_BASE_ADDR: u32 = BRAIINS_GLITCH_MONITOR_BASE;

/// 4 KB UIO mmap window (one page).
pub const BRAIINS_GLITCH_MONITOR_MAP_SIZE: usize = 4096;

/// Backwards-compatible alias.
#[deprecated(since = "0.13.0", note = "Use BRAIINS_GLITCH_MONITOR_MAP_SIZE")]
pub const GLITCH_MONITOR_MAP_SIZE: usize = BRAIINS_GLITCH_MONITOR_MAP_SIZE;

/// Mirror-bit observation for `ro_relay_en` (bit 1 in bosminer Layout B).
///
/// XXX: R4-CONFIRMED — this is a MIRROR of BM1362 UART_RELAY candidate
/// state. Writes to FPGA address space silently fail per Phase 9A.
/// BM1362 0x2C/0x34 candidate broadcasts remain lab-gated pending R6-7.
pub const BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT: u32 = 0x0000_0002;

/// Backwards-compatible alias for the legacy `RELAY_ENABLE_VALUE`.
///
/// Old name was misleading: this value is what the mirror MIRRORS, not
/// what enables anything. Use `BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT`.
#[deprecated(
    since = "0.13.0",
    note = "Renamed to BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT — old name implied \
            this value enables something, but it's just the mirror bit observation"
)]
pub const RELAY_ENABLE_VALUE: u32 = BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT;

/// Speculative offsets — verified by string-table order, not by live probe.
const SPEC_OFFSET_UART_RX: u32 = 0x00;
const SPEC_OFFSET_SCL: u32 = 0x04;
const SPEC_OFFSET_SDA: u32 = 0x08;

/// Map a physical chain index to its Braiins glitch-status mirror offset.
///
/// XXX: R4-CONFIRMED — Braiins-am2 ONLY. These offsets only exist in the
/// Braiins-am2 custom bitstream's miner-glitch-monitor IP. Stock hardware
/// returns `0` for all reads.
///
/// Only physical indices 2 and 3 are populated on .139 (Phase 4A) — those
/// correspond to logical hash chains 1 and 4 respectively. Other indices
/// either don't exist on this bitstream variant or are tied off; returning
/// `None` signals "don't touch."
pub const fn chain_glitch_status_offset(chain_phys_idx: u8) -> Option<u32> {
    match chain_phys_idx {
        // physical idx 2 = logical chain 1 (FPGA window 0x43C0_0000)
        2 => Some(0x30),
        // physical idx 3 = logical chain 4 (FPGA window 0x43C3_0000)
        3 => Some(0x34),
        _ => None,
    }
}

/// Backwards-compatible alias.
#[deprecated(
    since = "0.13.0",
    note = "Renamed to chain_glitch_status_offset — old name implied control, \
            but it's a status-mirror offset (Braiins-am2 only)"
)]
pub const fn chain_relay_offset(chain_phys_idx: u8) -> Option<u32> {
    chain_glitch_status_offset(chain_phys_idx)
}

/// Result of an explicit Braiins glitch-mirror write attempt.
///
/// "Attempt" because the Braiins-am2 mirror is read-only on stock hw —
/// 7 devmem write attempts during sustained mining were silently rejected
/// per Phase 9A. This struct surfaces the post-read for diagnostic logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BraiinsGlitchMirrorWriteAttempt {
    pub offset: u32,
    pub pre_read: u32,
    pub wrote: u32,
    pub post_read: u32,
}

/// Glitch counter snapshot. All counters are 32-bit unsigned. Field names
/// mirror bosminer's telemetry strings so dashboards parsing the BraiinsOS
/// JSON can reuse the same keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlitchCounters {
    pub uart_rx: u32,
    pub scl: u32,
    pub sda: u32,
}

/// `miner-glitch-monitor` UIO driver — Braiins-am2 only.
///
/// W13.B1 (2026-05-10): renamed from `GlitchMonitor`. Combined with the
/// old `dcentrald_hal::uart_relay::UartRelay` (which mmapped the same
/// physical page via `/dev/mem`); the merged struct exposes both the
/// counter snapshot API and the diagnostic UART relay mirror API.
///
/// Wraps a single mmap of `/dev/uioN` map0. Uses a raw `*mut u8` plus a
/// length so we can do volatile read/write at any offset within the page.
pub struct BraiinsGlitchMonitor {
    /// UIO device for counter reads (uses [`UioDevice`]'s 4-byte API).
    regs: UioDevice,
    /// Raw map0 base for direct writes to non-counter offsets.
    raw_ptr: *mut u8,
    /// Map size (typically 4096).
    raw_len: usize,
    /// UIO numeric for diagnostics (also keeps the source of `_file` traceable).
    uio_n: u8,
    /// **CRITICAL**: hold the `/dev/uioN` file descriptor open for the lifetime
    /// of the mmap. The Xilinx `uio_pdrv_genirq` kernel driver gates AXI-Lite
    /// **write** permission on the per-fd map.
    _file: std::fs::File,
}

/// Backwards-compatible type alias.
#[deprecated(
    since = "0.13.0",
    note = "Renamed to BraiinsGlitchMonitor per W13.B1 — Braiins-am2 only, NOT a \
            generic glitch monitor surface"
)]
pub type GlitchMonitor = BraiinsGlitchMonitor;

// SAFETY: the mmap is a device register page; volatile reads/writes on it
// have no thread-local state.
unsafe impl Send for BraiinsGlitchMonitor {}
unsafe impl Sync for BraiinsGlitchMonitor {}

impl BraiinsGlitchMonitor {
    /// Open `/dev/uio<n>` and mmap map0 (4 KB).
    pub fn open(uio_number: u8) -> Result<Self> {
        let regs = UioDevice::open(uio_number)?;

        let dev_path = format!("/dev/uio{}", uio_number);
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&dev_path)
            .map_err(|e| HalError::DeviceOpen {
                path: dev_path.clone(),
                source: e,
            })?;

        let len = NonZeroUsize::new(BRAIINS_GLITCH_MONITOR_MAP_SIZE)
            .expect("BRAIINS_GLITCH_MONITOR_MAP_SIZE is a non-zero compile-time constant");

        // SAFETY: standard UIO mmap.
        let mapped = unsafe {
            nix::sys::mman::mmap(
                None,
                len,
                ProtFlags::PROT_READ | ProtFlags::PROT_WRITE,
                MapFlags::MAP_SHARED,
                &file,
                0,
            )
        }
        .map_err(|e| HalError::MmapFailed {
            device: dev_path.clone(),
            source: e,
        })?;

        tracing::info!(
            uio = uio_number,
            base = format_args!("0x{:08X}", BRAIINS_GLITCH_MONITOR_BASE),
            size = BRAIINS_GLITCH_MONITOR_MAP_SIZE,
            "Opened miner-glitch-monitor UIO (Braiins-am2 only — fd retained for AXI-Lite write permission)"
        );

        Ok(Self {
            regs,
            raw_ptr: mapped.as_ptr() as *mut u8,
            raw_len: BRAIINS_GLITCH_MONITOR_MAP_SIZE,
            uio_n: uio_number,
            _file: file,
        })
    }

    /// UIO device number this instance is bound to. Diagnostic only.
    pub fn uio_number(&self) -> u8 {
        self.uio_n
    }

    /// Read a 32-bit register at `offset` from the raw mapping.
    pub fn read_word(&self, offset: u32) -> Result<u32> {
        if !offset.is_multiple_of(4) {
            return Err(HalError::Other(format!(
                "BraiinsGlitchMonitor::read_word offset 0x{:X} not 4-byte aligned",
                offset
            )));
        }
        if (offset as usize) + 4 > self.raw_len {
            return Err(HalError::RegisterOutOfBounds {
                device: "miner-glitch-monitor".into(),
                offset,
                size: self.raw_len,
            });
        }
        // SAFETY: bounds + alignment checked above; ptr is a 4 KB MMIO mmap.
        let v = unsafe { std::ptr::read_volatile(self.raw_ptr.add(offset as usize) as *const u32) };
        Ok(v)
    }

    /// Write a 32-bit value at `offset` through the UIO mapping.
    ///
    /// Per Phase 9A: writes are silently rejected on stock hardware. This
    /// API is diagnostic-only and is gated by
    /// `am2_force_braiins_glitch_mirror_write` (lab-only, default `false`).
    pub fn write_glitch_mirror_word(&self, offset: u32, value: u32) -> Result<()> {
        if !offset.is_multiple_of(4) {
            return Err(HalError::Other(format!(
                "BraiinsGlitchMonitor::write_glitch_mirror_word offset 0x{:X} not 4-byte aligned",
                offset
            )));
        }
        if (offset as usize) + 4 > self.raw_len {
            return Err(HalError::RegisterOutOfBounds {
                device: "miner-glitch-monitor".into(),
                offset,
                size: self.raw_len,
            });
        }
        // SAFETY: bounds + alignment checked above; ptr is a 4 KB RW MMIO mmap.
        unsafe {
            std::ptr::write_volatile(self.raw_ptr.add(offset as usize) as *mut u32, value);
        }
        Ok(())
    }

    /// Read a snapshot of the 3 speculative glitch counters. Passive — never
    /// writes to the IP.
    pub fn read_counters(&self) -> GlitchCounters {
        GlitchCounters {
            uart_rx: self.regs.read_reg(SPEC_OFFSET_UART_RX),
            scl: self.regs.read_reg(SPEC_OFFSET_SCL),
            sda: self.regs.read_reg(SPEC_OFFSET_SDA),
        }
    }

    /// Returns true if all known counters are currently zero (nominal).
    pub fn is_quiescent(&self) -> bool {
        self.read_counters() == GlitchCounters::default()
    }

    /// Read the per-chain UART relay STATUS MIRROR through the UIO mapping.
    ///
    /// Read-only — this is the canonical operation. The mirror reflects
    /// the BM1362 ASIC reg `0x2C` value the chip currently has set.
    /// Mirror writes do NOT control the chip. BM1362 0x2C/0x34 candidate
    /// broadcasts remain lab-gated pending R6-7.
    pub fn read_chain_uart_relay_mirror(&self, chain_phys_idx: u8) -> Result<u32> {
        let offset = chain_glitch_status_offset(chain_phys_idx).ok_or_else(|| {
            HalError::Other(format!(
                "BraiinsGlitchMonitor: no UART relay status mirror offset for chain_phys_idx={} \
                 (only 2 and 3 are populated on .139 Braiins-am2)",
                chain_phys_idx
            ))
        })?;
        let v = self.read_word(offset)?;
        tracing::info!(
            chain_phys_idx,
            offset = format_args!("0x{:02X}", offset),
            value = format_args!("0x{:08X}", v),
            "BraiinsGlitchMonitor: UART relay status mirror read (Braiins-am2 diagnostic)"
        );
        Ok(v)
    }

    /// Backwards-compatible alias.
    #[deprecated(
        since = "0.13.0",
        note = "Renamed to read_chain_uart_relay_mirror — old `observe_relay` name was \
                ambiguous about the read-only mirror semantics"
    )]
    pub fn observe_relay(&self, chain_phys_idx: u8) -> Result<u32> {
        self.read_chain_uart_relay_mirror(chain_phys_idx)
    }

    /// Diagnostic-only Braiins glitch-monitor mirror write attempt.
    ///
    /// This is a Braiins-am2-bitstream-only write attempt that is expected
    /// to fail on stock hardware. Phase 9A proved 7 distinct write values
    /// are silently rejected from userspace. Use only for telemetry parity
    /// against bosminer; never as a load-bearing control path.
    ///
    /// Returns the pre/post readback so callers can log the silent NO-OP.
    pub fn force_braiins_glitch_status_mirror_write(
        &self,
        chain_phys_idx: u8,
    ) -> Result<BraiinsGlitchMirrorWriteAttempt> {
        let offset = chain_glitch_status_offset(chain_phys_idx).ok_or_else(|| {
            HalError::Other(format!(
                "BraiinsGlitchMonitor: no glitch-status mirror offset for chain_phys_idx={}",
                chain_phys_idx
            ))
        })?;

        let pre_read = self.read_word(offset)?;
        self.write_glitch_mirror_word(offset, BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT)?;
        let post_read = self.read_word(offset)?;

        tracing::info!(
            chain_phys_idx,
            offset = format_args!("0x{:02X}", offset),
            pre = format_args!("0x{:08X}", pre_read),
            wrote = format_args!("0x{:08X}", BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT),
            post = format_args!("0x{:08X}", post_read),
            "BraiinsGlitchMonitor: forced glitch-status mirror write attempt \
             (diagnostic-only; expected NO-OP on stock hardware)"
        );

        if post_read != BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT {
            tracing::warn!(
                chain_phys_idx,
                offset = format_args!("0x{:02X}", offset),
                wrote = format_args!("0x{:08X}", BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT),
                post = format_args!("0x{:08X}", post_read),
                "BraiinsGlitchMonitor: mirror write was NO-OP (post != wrote). \
                 This is expected on stock hardware. Real control lives at \
                 BM1362 ASIC reg 0x2C — see dcentrald_asic::bm1362::uart_relay."
            );
        }

        Ok(BraiinsGlitchMirrorWriteAttempt {
            offset,
            pre_read,
            wrote: BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT,
            post_read,
        })
    }

    /// Backwards-compatible alias for the legacy "lock" name. The W12-era
    /// "lock" semantic was bosminer's process-local ownership wrapper, not
    /// a HW lock. Per W13.B1 this is now diagnostic-only.
    #[deprecated(
        since = "0.13.0",
        note = "Renamed to force_braiins_glitch_status_mirror_write — old `lock_uart_relay` \
                implied control which Phase 9A proved this is not"
    )]
    pub fn lock_uart_relay(&self, chain_phys_idx: u8) -> Result<u32> {
        let attempt = self.force_braiins_glitch_status_mirror_write(chain_phys_idx)?;
        Ok(attempt.post_read)
    }

    /// Backwards-compatible "unlock" alias — writes 0 to the mirror. Use
    /// only on graceful shutdown.
    #[deprecated(
        since = "0.13.0",
        note = "The unlock semantic was bosminer-style cleanup; per W13.B1 the mirror \
                is read-only on stock hw, so this write is also diagnostic-only"
    )]
    pub fn unlock_uart_relay(&self, chain_phys_idx: u8) -> Result<()> {
        let offset = chain_glitch_status_offset(chain_phys_idx).ok_or_else(|| {
            HalError::Other(format!(
                "BraiinsGlitchMonitor: no glitch-status mirror offset for chain_phys_idx={}",
                chain_phys_idx
            ))
        })?;
        self.write_glitch_mirror_word(offset, 0)?;
        tracing::info!(
            chain_phys_idx,
            offset = format_args!("0x{:02X}", offset),
            "BraiinsGlitchMonitor: unlock attempt (write 0; diagnostic-only)"
        );
        Ok(())
    }
}

impl Drop for BraiinsGlitchMonitor {
    fn drop(&mut self) {
        // Same rationale as the other device-backed mappings: rely on
        // process-exit cleanup rather than risky late-shutdown munmap.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_address_is_braiins_am2_only() {
        // The Braiins glitch monitor is at 0x43D0_0000 — Braiins-am2 only.
        assert_eq!(BRAIINS_GLITCH_MONITOR_BASE, 0x43D0_0000);
    }

    #[test]
    fn map_size_is_one_page() {
        assert_eq!(BRAIINS_GLITCH_MONITOR_MAP_SIZE, 4096);
    }

    #[test]
    fn default_counters_are_zero() {
        let g = GlitchCounters::default();
        assert_eq!(g.uart_rx, 0);
        assert_eq!(g.scl, 0);
        assert_eq!(g.sda, 0);
    }

    #[test]
    fn chain_glitch_status_offset_only_knows_phys_2_and_3() {
        assert_eq!(chain_glitch_status_offset(2), Some(0x30));
        assert_eq!(chain_glitch_status_offset(3), Some(0x34));
        assert_eq!(chain_glitch_status_offset(0), None);
        assert_eq!(chain_glitch_status_offset(1), None);
        assert_eq!(chain_glitch_status_offset(4), None);
        assert_eq!(chain_glitch_status_offset(255), None);
    }

    #[test]
    fn mirror_bit_value_is_0x2() {
        // ro_relay_en bit observed on healthy bosminer (Phase 5A).
        assert_eq!(BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT, 0x0000_0002);
    }

    #[test]
    fn glitch_monitor_addr_is_braiins_only_not_stock() {
        // W13.B1 REGRESSION: pin module-doc text declaring this IP is
        // Braiins-am2 ONLY and stock hardware does NOT populate it.
        // Future agents must not silently re-promote the mirror to a
        // control surface.
        let src = include_str!("glitch_monitor.rs");
        assert!(
            src.contains("Braiins-am2 bitstream only"),
            "glitch_monitor.rs MUST keep the `Braiins-am2 bitstream only` \
             scope marker. Stock CV1835/AM335x/AML/S9 do NOT populate the IP."
        );
        assert!(
            src.contains("Stock CV1835 / AM335x BB / Amlogic / S9"),
            "glitch_monitor.rs MUST keep the explicit stock-platform exclusion \
             list. R4-CONFIRMED scope marker required."
        );
    }

    #[test]
    fn force_braiins_glitch_status_mirror_write_is_diagnostic_only() {
        // W13.B1 REGRESSION: pin doc-text on the diagnostic-only API.
        // Future agents must not promote this to a load-bearing control
        // path. Phase 9A proved the writes are silent NO-OPs on stock hw.
        let src = include_str!("glitch_monitor.rs");
        assert!(
            src.contains("diagnostic-only"),
            "force_braiins_glitch_status_mirror_write MUST stay marked \
             `diagnostic-only`. Phase 9A proved writes are silent NO-OPs."
        );
        assert!(
            src.contains("BM1362 ASIC reg 0x2C") || src.contains("BM1362 ASIC reg `0x2C`"),
            "doc must point at the canonical control reg in dcentrald-asic"
        );
    }

    #[allow(deprecated)]
    #[test]
    fn back_compat_aliases_resolve() {
        // Deprecated aliases must still resolve so the W13.B1 cut-over
        // doesn't break transitive consumers we missed.
        assert_eq!(GLITCH_MONITOR_BASE_ADDR, BRAIINS_GLITCH_MONITOR_BASE);
        assert_eq!(GLITCH_MONITOR_MAP_SIZE, BRAIINS_GLITCH_MONITOR_MAP_SIZE);
        assert_eq!(RELAY_ENABLE_VALUE, BRAIINS_GLITCH_MIRROR_RO_RELAY_BIT);
        assert_eq!(chain_relay_offset(2), Some(0x30));
        assert_eq!(chain_relay_offset(3), Some(0x34));
    }
}
