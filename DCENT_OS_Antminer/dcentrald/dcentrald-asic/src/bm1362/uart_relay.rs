//! BM1362 ASIC register `0x2C` — UART_RELAY candidate/control evidence.
//!
//! ## Relocation history (W13.B1, 2026-05-10)
//!
//! This module was relocated from `dcentrald_hal::uart_relay::UartRelayReg`
//! after R4 RE pass + 5-source consensus established that:
//!
//! 1. **`UART_RELAY` evidence points at BM1362 ASIC register `0x2C`**,
//!    reachable via the per-chip serial protocol
//!    ([`crate::chain::SerialChainBackend::send_write_reg_broadcast_bm1397plus`]).
//!    R6-7 keeps production writes disabled until live captures confirm
//!    exact control semantics. The full chip-prefixed address is `0xC1002C`
//!    (informational; the serial protocol uses the 8-bit reg only).
//! 2. **`0x43D00030` / `0x43D00034` are Braiins-am2 diagnostic mirrors**, NOT
//!    a control surface. Stock CV1835 / AM335x / Amlogic Antminer bitstreams
//!    have ZERO response there (address hole). Phase 9A live tests proved
//!    the mirrors silently reject `devmem` writes. See the renamed
//!    `dcentrald_hal::glitch_monitor::BraiinsGlitchMonitor` for the mirror
//!    surface.
//!
//! ## Two bitfield layouts coexist on BM1362
//!
//! BM1362 silicon hosts a single 32-bit register at `0x2C`, but two distinct
//! decode layouts have been observed in the wild. Both are documented here
//! for posterity; tests pin the stock-layout evidence per R4-2 carry-fwd.
//!
//! ### Layout A — RE3 §7 stock-BM1362 (canonical for BM1362)
//!
//! ```text
//! value = chip_address      // bits [7:0]
//!       | (gap_cnt << 8)    // bits [11:8]   (4 bits, max 15)
//!       | (nonce_gap_en << 12)  // bit [12]
//!       | (ro_relay_en  << 13)  // bit [13]
//!       | (co_relay_en  << 14)  // bit [14]
//! // bits [31:15] reserved — must preserve on RMW
//! ```
//!
//! Default at cold boot: `0x000F_0000` (gap_cnt=15, all relays disabled).
//!
//! XXX: R4-CONFIRMED — `uart_relay_blocker3_5_analysis.md` §3 (5-source
//! consensus). Pinned by [`tests::stock_bitfield_layout_documented`].
//!
//! ### Layout B — bosminer/BraiinsOS BM1387 overload (CVCtrl/Braiins-am2 init)
//!
//! ```text
//! value = co_relay_en          // bit [0]
//!       | (ro_relay_en << 1)   // bit [1]
//!       | (GAP_CNT << 16)      // bits [31:16]
//! ```
//!
//! Bosminer writes `0x007C_0003` (broadcast) and `0x000F_0003` (alt) on
//! BM1362 via this layout. dcentrald retains these values as lab-gated
//! candidates behind `DCENT_BM1362_ENABLE_UART_RELAY_LAB`. Matches the
//! `bosminer.bin` `UartRelayReg` string-table layout cited in
//! `phase9a_A1_binary_re.md:26-30`.
//!
//! XXX: R4-INFERRED scope-narrow to BraiinsOS — NOT stock BM1362 truth.
//! W12.7 was wrong about this being the stock layout. Pinned by
//! [`tests::bosminer_overload_layout_documented`].
//!
//! ## Cross-references
//!
//! -  — full rename plan.
//! -  (R4-2)
//!   — open question on exact write values for cold-boot enable.
//! - `dcentrald_hal::glitch_monitor::BraiinsGlitchMonitor` — the
//!   diagnostic mirror surface (Braiins-am2 only, NOT control).

/// BM1362 UART_RELAY ASIC register address (8-bit, used by the per-chip
/// serial protocol).
pub const UART_RELAY_REG_ADDR: u8 = 0x2C;

/// Informational full address (chip-base | reg). The serial protocol does
/// NOT use this prefix; it uses the 8-bit reg only via
/// [`crate::chain::SerialChainBackend::send_write_reg_broadcast_bm1397plus`].
pub const BM1362_UART_RELAY_ASIC_REG: u32 = 0x00C1_002C;

/// Stock BM1362 UART_RELAY default value at cold boot per RE3 §7
/// (Layout A: gap_cnt=15, all relay enables off).
pub const BM1362_UART_RELAY_DEFAULT: u32 = 0x000F_0000;

/// Bosminer-overload broadcast write value (Layout B). Retained as a
/// lab-gated BM1362 relay candidate.
pub const UART_RELAY_BOSMINER_ENABLE: u32 = 0x007C_0003;

/// Companion BM1362 UART_RELAY candidate register written by bosminer.
pub const UART_RELAY_ALT_REG_ADDR: u8 = 0x34;

/// Bosminer alt broadcast write value (Layout B alt — written to companion
/// register `0x34` on BM1362, NOT a different value for `0x2C`).
pub const UART_RELAY_BOSMINER_ENABLE_ALT: u32 = 0x000F_0003;

/// Typed bitfield for BM1362 UART_RELAY register `0x2C`, Layout A
/// (RE3 §7 stock canonical).
///
/// XXX: R4-CONFIRMED — uart_relay_blocker3_5_analysis.md §3 (5-source
/// consensus). The bit positions match RE3's inferred layout but exact
/// write values for cold-boot enable are not yet confirmed against live
/// silicon. Future agents must not wire this bitfield into any platform's
/// cold-boot path until R4-2 returns a live-captured bit pattern.
///
/// ## Field bit windows (RE3 §7)
///
/// | Bits   | Field         | Width | Notes                              |
/// |--------|---------------|-------|------------------------------------|
/// | [7:0]  | chip_address  | 8     | ASIC address on the UART chain     |
/// | [11:8] | gap_cnt       | 4     | Nonce gap count (NOT 5 bits)       |
/// | [12]   | nonce_gap_en  | 1     | Enable nonce gap filtering         |
/// | [13]   | ro_relay_en   | 1     | Rollover relay enable              |
/// | [14]   | co_relay_en   | 1     | Carry-over relay enable            |
/// | [31:15]| Reserved      | 17    | Must preserve on read-modify-write |
///
// W14.B cross-ref: W4 handoff `bm1362_nonce_format_update_summary.md` §9
// confirms this exact bitfield layout. Handoff also notes "in stock
// firmware, the 'no need to set uart relay' string means the condition
// check ALWAYS skips this write" — consistent with the W13.B1 design
// decision to keep this write OFF by default.
//
// PR-024 (2026-05-16): `cold_boot_enable_nonce_path()` now EXISTS (added
// below) so a future bench CV1835 unit has an opt-in path, but it is
// STRICTLY default-off — it composes a register frame ONLY when the
// pre-existing `DCENT_CV1835_ACCEPT_INFERRED_SOC_REGS` env-gate is `=1`
// (no new gate invented). Gate unset (production default) is a clean,
// logged no-op that composes NO frame — byte-identical to this W12.7
// "data only, NOT wired" state. The exact on-wire enable VALUE stays
// R11-6 hardware-gated (0/4 Tier-1 bench CV1835 asks landed across
// R3→R11). R4-2 / R11-6 carry-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UartRelayReg(pub u32);

impl UartRelayReg {
    // XXX: INFERRED — requires R4 live capture. Bit positions match RE3 §7
    // but exact write values for cold-boot are unconfirmed.
    const CHIP_ADDRESS_SHIFT: u32 = 0;
    const CHIP_ADDRESS_MASK: u32 = 0x0000_00FF;

    const GAP_CNT_SHIFT: u32 = 8;
    /// gap_cnt is **4 bits** [11:8] per RE3 §7 — NOT 5 bits.
    pub const GAP_CNT_WIDTH: u32 = 4;
    pub const GAP_CNT_MAX: u8 = (1u8 << Self::GAP_CNT_WIDTH) - 1; // 15
    pub const GAP_CNT_MASK: u32 = (Self::GAP_CNT_MAX as u32) << Self::GAP_CNT_SHIFT;

    const NONCE_GAP_EN_BIT: u32 = 12;
    const NONCE_GAP_EN_MASK: u32 = 1u32 << Self::NONCE_GAP_EN_BIT;

    const RO_RELAY_EN_BIT: u32 = 13;
    const RO_RELAY_EN_MASK: u32 = 1u32 << Self::RO_RELAY_EN_BIT;

    const CO_RELAY_EN_BIT: u32 = 14;
    const CO_RELAY_EN_MASK: u32 = 1u32 << Self::CO_RELAY_EN_BIT;

    /// All fields cleared. `nonce_gap_en` / `ro_relay_en` / `co_relay_en`
    /// all FALSE. `chip_address = 0`, `gap_cnt = 0`.
    pub const fn zero() -> Self {
        Self(0)
    }

    /// Construct from raw fields. `gap_cnt` is masked to 4 bits — values
    /// above 15 silently truncate. Reserved bits [31:15] are zeroed; use
    /// [`Self::from_raw`] if you need to preserve foreign bits.
    pub const fn new(
        chip_address: u8,
        gap_cnt: u8,
        nonce_gap_en: bool,
        ro_relay_en: bool,
        co_relay_en: bool,
    ) -> Self {
        let mut v: u32 = 0;
        v |= (chip_address as u32) << Self::CHIP_ADDRESS_SHIFT;
        v |= ((gap_cnt as u32) & (Self::GAP_CNT_MAX as u32)) << Self::GAP_CNT_SHIFT;
        if nonce_gap_en {
            v |= Self::NONCE_GAP_EN_MASK;
        }
        if ro_relay_en {
            v |= Self::RO_RELAY_EN_MASK;
        }
        if co_relay_en {
            v |= Self::CO_RELAY_EN_MASK;
        }
        Self(v)
    }

    /// Wrap a raw u32 (preserves reserved bits [31:15]).
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Decode the underlying u32.
    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn chip_address(self) -> u8 {
        ((self.0 & Self::CHIP_ADDRESS_MASK) >> Self::CHIP_ADDRESS_SHIFT) as u8
    }

    pub const fn gap_cnt(self) -> u8 {
        ((self.0 & Self::GAP_CNT_MASK) >> Self::GAP_CNT_SHIFT) as u8
    }

    pub const fn nonce_gap_en(self) -> bool {
        (self.0 & Self::NONCE_GAP_EN_MASK) != 0
    }

    pub const fn ro_relay_en(self) -> bool {
        (self.0 & Self::RO_RELAY_EN_MASK) != 0
    }

    pub const fn co_relay_en(self) -> bool {
        (self.0 & Self::CO_RELAY_EN_MASK) != 0
    }

    pub const fn with_chip_address(self, chip_address: u8) -> Self {
        let cleared = self.0 & !Self::CHIP_ADDRESS_MASK;
        Self(cleared | ((chip_address as u32) << Self::CHIP_ADDRESS_SHIFT))
    }

    /// `gap_cnt` is masked to 4 bits — values above 15 truncate.
    pub const fn with_gap_cnt(self, gap_cnt: u8) -> Self {
        let cleared = self.0 & !Self::GAP_CNT_MASK;
        let v = ((gap_cnt as u32) & (Self::GAP_CNT_MAX as u32)) << Self::GAP_CNT_SHIFT;
        Self(cleared | v)
    }

    pub const fn with_nonce_gap_en(self, on: bool) -> Self {
        let cleared = self.0 & !Self::NONCE_GAP_EN_MASK;
        if on {
            Self(cleared | Self::NONCE_GAP_EN_MASK)
        } else {
            Self(cleared)
        }
    }

    pub const fn with_ro_relay_en(self, on: bool) -> Self {
        let cleared = self.0 & !Self::RO_RELAY_EN_MASK;
        if on {
            Self(cleared | Self::RO_RELAY_EN_MASK)
        } else {
            Self(cleared)
        }
    }

    pub const fn with_co_relay_en(self, on: bool) -> Self {
        let cleared = self.0 & !Self::CO_RELAY_EN_MASK;
        if on {
            Self(cleared | Self::CO_RELAY_EN_MASK)
        } else {
            Self(cleared)
        }
    }
}

impl Default for UartRelayReg {
    fn default() -> Self {
        Self::zero()
    }
}

// ---------------------------------------------------------------------------
// PR-024 — cold_boot_enable_nonce_path (env-gated, default-off)
// ---------------------------------------------------------------------------

/// The canonical env-gate that unlocks the INFERRED CV1835 SoC-register
/// path. **Re-used verbatim from
/// `dcentrald_hal::platform::cvitek_cold_boot::ACCEPT_INFERRED_SOC_REGS_ENV`**
/// — a new gate is NOT invented (per PR-024 directive and
/// ). Duplicated here as a
/// string literal because `dcentrald-asic` cannot depend on
/// `dcentrald-hal` (cycle). A test pins both literals byte-for-byte so the
/// two crates cannot drift.
///
/// W15.A3 rename (2026-05-10): was `DCENT_CV1835_ACCEPT_INFERRED_FPGA`.
/// The deprecated alias ([`ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED`]) is
/// still honoured for backwards compatibility — either being `"1"`
/// unlocks the path, exactly matching the hal orchestrator's behaviour.
pub const ACCEPT_INFERRED_SOC_REGS_ENV: &str = "DCENT_CV1835_ACCEPT_INFERRED_SOC_REGS";

/// Deprecated env-gate alias for [`ACCEPT_INFERRED_SOC_REGS_ENV`]. Kept
/// accepted (silently) to mirror the hal orchestrator's W15.A3
/// backwards-compat behaviour. Either name being `"1"` unlocks the path.
pub const ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED: &str = "DCENT_CV1835_ACCEPT_INFERRED_FPGA";

/// Returns `true` iff the operator has explicitly accepted the INFERRED
/// CV1835 SoC-register risk by setting either env-var to exactly `"1"`.
///
/// This is the SOLE boundary controlling whether
/// [`cold_boot_enable_nonce_path`] composes a real register frame. The
/// production daemon never sets these env-vars, so this returns `false`
/// in every shipping configuration — the function below is then a
/// byte-identical no-op vs. today's "not wired" behaviour.
fn inferred_soc_regs_unlocked() -> bool {
    std::env::var(ACCEPT_INFERRED_SOC_REGS_ENV).ok().as_deref() == Some("1")
        || std::env::var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED)
            .ok()
            .as_deref()
            == Some("1")
}

/// Outcome of [`cold_boot_enable_nonce_path`]. Either the path was a
/// gated-off no-op (`Skipped`) or it produced the exact 9-byte broadcast
/// WRITE frame that the caller would put on the chain UART (`Frame`).
///
/// Returning the frame instead of writing it keeps this function pure +
/// host-testable and keeps the actual UART write at the existing
/// `dcentrald-hal` transport (no new HAL helper, no asic→hal dep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoncePathOutcome {
    /// Gate unset (production default). No register frame composed, no
    /// behaviour change vs. the W12.7 "data only, not wired" state.
    Skipped,
    /// Gate set. `reg` = [`UART_RELAY_REG_ADDR`] (0x2C), `value` = the
    /// composed [`UartRelayReg`] raw u32, `frame` = the 9-byte BM1397+
    /// broadcast WRITE wire frame (CRC5 trailer included).
    Frame { reg: u8, value: u32, frame: [u8; 9] },
}

/// Compose the BM1362 UART_RELAY (`0x2C`) cold-boot enable register write
/// from a [`UartRelayReg`] bitfield — **but only when the existing
/// INFERRED CV1835 env-gate is set**.
///
/// ## Default-off contract (PR-024, load-bearing)
///
/// When neither [`ACCEPT_INFERRED_SOC_REGS_ENV`] nor its deprecated alias
/// is `"1"` (the production default in every shipping configuration),
/// this returns [`NoncePathOutcome::Skipped`] immediately, BEFORE any
/// frame is composed. No register value is computed, no wire frame is
/// produced, the caller writes nothing. This is byte-identical to the
/// pre-PR-024 behaviour where the W12.7 bitfield was "data only, NOT
/// wired into `cold_boot_enable_nonce_path()`". The single `tracing`
/// line is observability only — it performs no I/O and changes no state.
///
/// When the gate IS set (bench CV1835 unit only — see
/// , 0/4 hardware asks
/// landed), it composes the Layout-A value from `cfg` and returns the
/// 9-byte broadcast WRITE frame via [`build_broadcast_write_frame`]
/// (the existing canonical BM1397+ WRITE helper — reused, not
/// reinvented). The caller is responsible for putting `frame` on the
/// chain UART using the existing `dcentrald-hal` transport.
///
/// XXX: INFERRED — bit positions are RE3 §7 Layout A (5-source
/// consensus on the field windows) but the EXACT cold-boot enable WRITE
/// value is NOT confirmed against live CV1835 silicon. The `cfg`
/// argument is supplied by the caller precisely so that no inferred
/// constant is baked into this function; the gate is what makes the
/// inferred path opt-in.
//
// TODO(R11-6): replace the caller-supplied `cfg` with a live-captured
// bit pattern once a bench CV1835 carrier lands (Tier-1 hardware ask,
// 0/4 satisfied across R3→R11). When R11-6 closes, this XXX downgrades
// to a confirmed citation and the gate MAY be relaxed — but only after a
// 3-round-trip live verification on the bench unit per
// .
pub fn cold_boot_enable_nonce_path(cfg: UartRelayReg) -> NoncePathOutcome {
    if !inferred_soc_regs_unlocked() {
        // Production default: byte-identical to W12.7 "not wired".
        // No frame composed, no register write, no behaviour change.
        tracing::debug!(
            target: "uart_relay",
            gate = ACCEPT_INFERRED_SOC_REGS_ENV,
            "cold_boot_enable_nonce_path: INFERRED CV1835 SoC-register gate \
             unset — no-op (W12.7 data-only contract preserved; R11-6 \
             carry-forward)"
        );
        return NoncePathOutcome::Skipped;
    }

    // Gate is set: operator has explicitly accepted the INFERRED risk.
    // Compose the Layout-A value and the canonical 9-byte broadcast
    // WRITE frame. XXX: INFERRED write value (R11-6) — see fn doc.
    // Reuse the existing canonical BM1397+ broadcast WRITE builder from
    // the parent module — no new frame logic, no asic→hal dep.
    use super::build_broadcast_write_frame;
    let value = cfg.raw();
    let frame = build_broadcast_write_frame(UART_RELAY_REG_ADDR, value);
    tracing::warn!(
        target: "uart_relay",
        gate = ACCEPT_INFERRED_SOC_REGS_ENV,
        reg = format_args!("0x{:02X}", UART_RELAY_REG_ADDR),
        value = format_args!("0x{:08X}", value),
        "cold_boot_enable_nonce_path: INFERRED CV1835 gate SET — composing \
         UART_RELAY broadcast WRITE (XXX: INFERRED value, R11-6 — lab/bench \
         CV1835 only)"
    );
    NoncePathOutcome::Frame {
        reg: UART_RELAY_REG_ADDR,
        value,
        frame,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // W13.B1 NEW: address + value pinning regressions.
    // ------------------------------------------------------------------

    #[test]
    fn uart_relay_reg_addr_is_0x2c() {
        // BM1362 silicon UART_RELAY evidence points at register 0x2C.
        // Production writes remain gated pending R6-7 live capture.
        assert_eq!(UART_RELAY_REG_ADDR, 0x2C);
    }

    #[test]
    fn uart_relay_full_addr_is_c1002c() {
        // Informational full address (chip-base | reg). The serial protocol
        // uses the 8-bit reg only; this is for cross-reference docs.
        assert_eq!(BM1362_UART_RELAY_ASIC_REG, 0x00C1_002C);
    }

    #[test]
    fn uart_relay_default_value_is_0x000f0000() {
        // RE3 §7 stock-BM1362 default (Layout A: gap_cnt=15, all relays off).
        assert_eq!(BM1362_UART_RELAY_DEFAULT, 0x000F_0000);
    }

    #[test]
    fn uart_relay_bosminer_enable_value_is_0x007c0003() {
        // Bosminer/BraiinsOS overload (Layout B): co/ro_relay_en in low bits.
        // Retained as a lab-gated candidate value.
        assert_eq!(UART_RELAY_BOSMINER_ENABLE, 0x007C_0003);
    }

    #[test]
    fn uart_relay_alt_reg_addr_is_0x34() {
        assert_eq!(UART_RELAY_ALT_REG_ADDR, 0x34);
    }

    #[test]
    fn uart_relay_bosminer_enable_alt_value_is_0x000f0003() {
        assert_eq!(UART_RELAY_BOSMINER_ENABLE_ALT, 0x000F_0003);
    }

    #[test]
    fn stock_bitfield_layout_documented() {
        // Pin module-doc presence of the Layout A canonical marker.
        let src = include_str!("uart_relay.rs");
        assert!(
            src.contains("R4-CONFIRMED"),
            "uart_relay.rs MUST keep the `R4-CONFIRMED` Layout A marker. \
             5-source consensus traceability requires it."
        );
        assert!(src.contains("Layout A"), "Layout A label missing");
    }

    #[test]
    fn bosminer_overload_layout_documented() {
        // Pin module-doc presence of the Layout B INFERRED marker.
        let src = include_str!("uart_relay.rs");
        assert!(
            src.contains("R4-INFERRED scope-narrow to BraiinsOS"),
            "uart_relay.rs MUST keep the `R4-INFERRED scope-narrow to BraiinsOS` \
             Layout B marker. W12.7 inversion traceability requires it."
        );
        assert!(src.contains("Layout B"), "Layout B label missing");
    }

    // ------------------------------------------------------------------
    // UartRelayReg bitfield tests (Layout A — RE3 §7, INFERRED).
    // Relocated from dcentrald-hal/src/uart_relay.rs::tests verbatim.
    // ------------------------------------------------------------------

    #[test]
    fn uart_relay_reg_chip_address_round_trips() {
        for addr in [0u8, 1, 0x42, 0x7F, 0x80, 0xAA, 0xFF] {
            let reg = UartRelayReg::new(addr, 0, false, false, false);
            assert_eq!(reg.chip_address(), addr, "chip_address={:#04x}", addr);
            assert_eq!(reg.raw() & 0xFF, addr as u32);
        }
    }

    #[test]
    fn uart_relay_reg_gap_cnt_4bit_max_15() {
        for gc in 0u8..=15 {
            let reg = UartRelayReg::new(0, gc, false, false, false);
            assert_eq!(reg.gap_cnt(), gc, "gap_cnt={}", gc);
        }
        // Truncation: 0x10 (bit 4 set) must drop bit 4 — only [11:8] survive.
        let truncated = UartRelayReg::new(0, 0x1F, false, false, false);
        assert_eq!(truncated.gap_cnt(), 0x0F);
        assert_eq!(UartRelayReg::GAP_CNT_WIDTH, 4);
        assert_eq!(UartRelayReg::GAP_CNT_MAX, 15);
        assert_eq!(UartRelayReg::GAP_CNT_MASK, 0x0000_0F00);
    }

    #[test]
    fn uart_relay_reg_nonce_gap_en_bit_12() {
        let off = UartRelayReg::new(0, 0, false, false, false);
        let on = UartRelayReg::new(0, 0, true, false, false);
        assert!(!off.nonce_gap_en());
        assert!(on.nonce_gap_en());
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 12);
    }

    #[test]
    fn uart_relay_reg_ro_relay_en_bit_13() {
        let off = UartRelayReg::new(0, 0, false, false, false);
        let on = UartRelayReg::new(0, 0, false, true, false);
        assert!(!off.ro_relay_en());
        assert!(on.ro_relay_en());
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 13);
    }

    #[test]
    fn uart_relay_reg_co_relay_en_bit_14() {
        let off = UartRelayReg::new(0, 0, false, false, false);
        let on = UartRelayReg::new(0, 0, false, false, true);
        assert!(!off.co_relay_en());
        assert!(on.co_relay_en());
        assert_eq!(on.raw() ^ off.raw(), 1u32 << 14);
    }

    #[test]
    fn uart_relay_reg_setters_preserve_other_fields() {
        let foreign_reserved: u32 = 0xDEAD_8000; // bits in [31:15]
        let base_raw: u32 = 0x42 | (0x0A << 8) | (1 << 12) | (1 << 14) | foreign_reserved;
        let base = UartRelayReg::from_raw(base_raw);
        assert_eq!(base.chip_address(), 0x42);
        assert_eq!(base.gap_cnt(), 0x0A);
        assert!(base.nonce_gap_en());
        assert!(!base.ro_relay_en());
        assert!(base.co_relay_en());
        assert_eq!(base.raw() & 0xFFFF_8000, foreign_reserved);

        let flipped = base.with_ro_relay_en(true);
        assert_eq!(flipped.chip_address(), 0x42);
        assert_eq!(flipped.gap_cnt(), 0x0A);
        assert!(flipped.nonce_gap_en());
        assert!(flipped.ro_relay_en());
        assert!(flipped.co_relay_en());
        assert_eq!(flipped.raw() & 0xFFFF_8000, foreign_reserved);

        let addr = base.with_chip_address(0xFE);
        assert_eq!(addr.chip_address(), 0xFE);
        assert_eq!(addr.gap_cnt(), 0x0A);
        assert!(addr.nonce_gap_en());
        assert!(!addr.ro_relay_en());
        assert!(addr.co_relay_en());
        assert_eq!(addr.raw() & 0xFFFF_8000, foreign_reserved);

        let gc = base.with_gap_cnt(0x05);
        assert_eq!(gc.chip_address(), 0x42);
        assert_eq!(gc.gap_cnt(), 0x05);
        assert!(gc.nonce_gap_en());
        assert!(!gc.ro_relay_en());
        assert!(gc.co_relay_en());
        assert_eq!(gc.raw() & 0xFFFF_8000, foreign_reserved);

        let cleared = base.with_nonce_gap_en(false);
        assert!(!cleared.nonce_gap_en());
        assert_eq!(cleared.chip_address(), 0x42);
        assert_eq!(cleared.gap_cnt(), 0x0A);
        assert_eq!(cleared.raw() & 0xFFFF_8000, foreign_reserved);

        let co_off = base.with_co_relay_en(false);
        assert!(!co_off.co_relay_en());
        assert_eq!(co_off.raw() & 0xFFFF_8000, foreign_reserved);
    }

    #[test]
    fn uart_relay_reg_default_is_all_zeros_disabled() {
        let z = UartRelayReg::default();
        assert_eq!(z, UartRelayReg::zero());
        assert_eq!(z.raw(), 0);
        assert_eq!(z.chip_address(), 0);
        assert_eq!(z.gap_cnt(), 0);
        assert!(!z.nonce_gap_en());
        assert!(!z.ro_relay_en());
        assert!(!z.co_relay_en());
    }

    #[test]
    fn uart_relay_reg_xxx_marker_present_in_doc() {
        // Regression-pin: the load-bearing `XXX: R4-CONFIRMED` /
        // `XXX: R4-INFERRED` markers must stay in this file. Future
        // agents who delete them as "unnecessary defensive comments"
        // silently lose R4 traceability.
        let src = include_str!("uart_relay.rs");
        assert!(
            src.contains("XXX: R4-CONFIRMED"),
            "uart_relay.rs is missing the load-bearing `XXX: R4-CONFIRMED` \
             marker on the canonical Layout A. R4-2 traceability requires it."
        );
        assert!(
            src.contains("XXX: R4-INFERRED scope-narrow to BraiinsOS"),
            "uart_relay.rs is missing the `XXX: R4-INFERRED scope-narrow to \
             BraiinsOS` marker. W12.7 inversion traceability requires it."
        );
    }

    #[test]
    fn uart_relay_reg_known_decode_round_trip() {
        let expected: u32 = 0x07 | (0x03 << 8) | (1 << 12) | (1 << 13);
        let reg = UartRelayReg::new(0x07, 0x03, true, true, false);
        assert_eq!(reg.raw(), expected, "raw encode mismatch");
        assert_eq!(reg.chip_address(), 0x07);
        assert_eq!(reg.gap_cnt(), 0x03);
        assert!(reg.nonce_gap_en());
        assert!(reg.ro_relay_en());
        assert!(!reg.co_relay_en());

        let from_raw = UartRelayReg::from_raw(expected);
        assert_eq!(from_raw, reg);
    }

    #[test]
    fn uart_relay_reg_gap_cnt_15_max_edge_case() {
        let reg = UartRelayReg::new(0xAA, 15, false, false, false);
        assert_eq!(reg.gap_cnt(), 15);
        assert_eq!(reg.raw() & 0x0000_FF00, 0x0F00);
        assert!(!reg.nonce_gap_en());
        assert!(!reg.ro_relay_en());
        assert!(!reg.co_relay_en());
    }

    #[test]
    fn uart_relay_reg_all_bits_set_and_clear() {
        let on = UartRelayReg::new(0xFF, 15, true, true, true);
        assert_eq!(on.chip_address(), 0xFF);
        assert_eq!(on.gap_cnt(), 15);
        assert!(on.nonce_gap_en());
        assert!(on.ro_relay_en());
        assert!(on.co_relay_en());
        // Defined-field union: chip_address(8) | gap_cnt(4) | flags(3) = 0x7FFF
        assert_eq!(on.raw(), 0x0000_7FFF);

        let off = UartRelayReg::new(0, 0, false, false, false);
        assert_eq!(off.raw(), 0);
        assert_eq!(off, UartRelayReg::zero());
    }

    // ------------------------------------------------------------------
    // PR-024 — cold_boot_enable_nonce_path env-gate + encode regressions.
    //
    // These tests mutate process-global env vars, so they MUST run
    // serialized (a static Mutex) and MUST restore prior env state, or a
    // leaked `=1` would silently arm the inferred path for OTHER tests in
    // the same process. The gate-unset no-op test is the load-bearing
    // PR-024 guarantee.
    // ------------------------------------------------------------------

    use std::sync::Mutex;

    static ENV_GUARD: Mutex<()> = Mutex::new(());

    /// Save + clear BOTH env-var names, run `f`, then restore. Returns
    /// `f`'s value. Panics in `f` still restore via the drop guard.
    fn with_env_clean<R>(f: impl FnOnce() -> R) -> R {
        let _lock = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        struct Restore {
            new: Option<String>,
            old: Option<String>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                match &self.new {
                    Some(v) => std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV, v),
                    None => std::env::remove_var(ACCEPT_INFERRED_SOC_REGS_ENV),
                }
                match &self.old {
                    Some(v) => std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED, v),
                    None => std::env::remove_var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED),
                }
            }
        }
        let _restore = Restore {
            new: std::env::var(ACCEPT_INFERRED_SOC_REGS_ENV).ok(),
            old: std::env::var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED).ok(),
        };
        std::env::remove_var(ACCEPT_INFERRED_SOC_REGS_ENV);
        std::env::remove_var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED);
        f()
    }

    #[test]
    fn pr024_env_name_matches_hal_orchestrator_byte_for_byte() {
        // The asic-crate literal MUST stay byte-identical to
        // `dcentrald_hal::platform::cvitek_cold_boot::
        //  ACCEPT_INFERRED_SOC_REGS_ENV`. asic cannot dep on hal (cycle),
        // so this is the cross-crate drift pin. No NEW env-gate invented.
        assert_eq!(
            ACCEPT_INFERRED_SOC_REGS_ENV,
            "DCENT_CV1835_ACCEPT_INFERRED_SOC_REGS"
        );
        assert_eq!(
            ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED,
            "DCENT_CV1835_ACCEPT_INFERRED_FPGA"
        );
    }

    #[test]
    fn pr024_gate_unset_is_a_clean_noop_no_register_write() {
        // LOAD-BEARING: with neither env-var set (the production default
        // in every shipping configuration), the path composes NO frame
        // and returns Skipped — byte-identical to the W12.7 "data only,
        // NOT wired" state. This is the PR-024 prime-directive proof.
        with_env_clean(|| {
            assert!(
                !inferred_soc_regs_unlocked(),
                "gate must read unset after with_env_clean"
            );
            let any_cfg = UartRelayReg::new(0x07, 0x03, true, true, false);
            let out = cold_boot_enable_nonce_path(any_cfg);
            assert_eq!(
                out,
                NoncePathOutcome::Skipped,
                "gate unset MUST be a no-op — no inferred CV1835 register \
                 write may be composed when the gate is unset"
            );
            // Skipped carries no reg/value/frame — there is nothing for a
            // caller to ever put on the wire.
            assert!(matches!(out, NoncePathOutcome::Skipped));
        });
    }

    #[test]
    fn pr024_gate_unset_even_with_unrelated_env_present() {
        // A stray empty string or "0" must NOT unlock — only exact "1".
        with_env_clean(|| {
            std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV, "0");
            std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED, "");
            assert!(!inferred_soc_regs_unlocked());
            let out = cold_boot_enable_nonce_path(UartRelayReg::zero());
            assert_eq!(out, NoncePathOutcome::Skipped);
        });
    }

    #[test]
    fn pr024_gate_set_new_name_composes_canonical_broadcast_frame() {
        with_env_clean(|| {
            std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV, "1");
            assert!(inferred_soc_regs_unlocked());
            let cfg = UartRelayReg::new(0x07, 0x03, true, true, false);
            let out = cold_boot_enable_nonce_path(cfg);
            // Expected raw: chip=0x07 | gap_cnt=3<<8 | nonce_gap(12) |
            // ro_relay(13).
            let expected_val: u32 = 0x07 | (0x03 << 8) | (1 << 12) | (1 << 13);
            assert_eq!(cfg.raw(), expected_val, "bitfield encode pin");
            match out {
                NoncePathOutcome::Frame { reg, value, frame } => {
                    assert_eq!(reg, UART_RELAY_REG_ADDR, "must target 0x2C");
                    assert_eq!(value, expected_val);
                    // Frame == canonical BM1397+ broadcast WRITE builder
                    // output (reuse proof, not a reinvented encoder).
                    let canonical = super::super::build_broadcast_write_frame(
                        UART_RELAY_REG_ADDR,
                        expected_val,
                    );
                    assert_eq!(frame, canonical);
                    // HDR=0x51 broadcast WRITE, LEN=0x09, chip=0x00.
                    assert_eq!(&frame[0..3], &[0x51, 0x09, 0x00]);
                    assert_eq!(frame[3], 0x2C);
                    // VAL_BE.
                    assert_eq!(&frame[4..8], &expected_val.to_be_bytes());
                }
                NoncePathOutcome::Skipped => {
                    panic!("gate SET (new name) must compose a Frame")
                }
            }
        });
    }

    #[test]
    fn pr024_gate_set_deprecated_alias_also_unlocks() {
        // W15.A3 backwards-compat: the old env-var name still unlocks,
        // exactly matching the hal orchestrator's dual-name behaviour.
        with_env_clean(|| {
            std::env::set_var(ACCEPT_INFERRED_SOC_REGS_ENV_DEPRECATED, "1");
            assert!(inferred_soc_regs_unlocked());
            let out = cold_boot_enable_nonce_path(UartRelayReg::zero());
            match out {
                NoncePathOutcome::Frame { reg, value, .. } => {
                    assert_eq!(reg, UART_RELAY_REG_ADDR);
                    assert_eq!(value, 0, "zero() cfg → all-zero value");
                }
                NoncePathOutcome::Skipped => {
                    panic!("deprecated alias =1 must unlock")
                }
            }
        });
    }

    #[test]
    fn pr024_bitfield_encode_pin_for_nonce_path_cfgs() {
        // Independent encode pin for the values the nonce path would
        // carry — guards the struct→u32 packing the gated path relies on.
        let cases: &[(u8, u8, bool, bool, bool, u32)] = &[
            (0x00, 0x00, false, false, false, 0x0000_0000),
            (0xFF, 0x0F, true, true, true, 0x0000_7FFF),
            (
                0x42,
                0x0A,
                true,
                false,
                true,
                0x42 | (0x0A << 8) | (1 << 12) | (1 << 14),
            ),
            (0xAA, 0x0F, false, false, false, 0xAA | (0x0F << 8)),
        ];
        for &(addr, gc, ng, ro, co, want) in cases {
            let r = UartRelayReg::new(addr, gc, ng, ro, co);
            assert_eq!(r.raw(), want, "encode {addr:#x}/{gc}/{ng}/{ro}/{co}");
        }
    }

    #[test]
    fn pr024_xxx_inferred_and_r11_6_todo_markers_present() {
        // Regression-pin the load-bearing provenance markers on the
        // gated path. A future agent who deletes them as "noise" silently
        // loses R11-6 traceability + the PR-024 default-off rationale.
        let src = include_str!("uart_relay.rs");
        assert!(
            src.contains("TODO(R11-6)"),
            "uart_relay.rs is missing the load-bearing `TODO(R11-6)` \
             marker on cold_boot_enable_nonce_path"
        );
        assert!(
            src.contains("XXX: INFERRED — bit positions are RE3 §7 Layout A"),
            "uart_relay.rs is missing the cold_boot_enable_nonce_path \
             `XXX: INFERRED` provenance marker"
        );
        assert!(
            src.contains("Default-off contract (PR-024, load-bearing)"),
            "uart_relay.rs is missing the PR-024 default-off contract \
             doc section"
        );
    }
}
