//! Pure, dependency-free HAL write-protect policy for the TPS546 power IC
//! (XPSAFE-2, cross-pollinated from DCENT_OS).
//!
//! ## Why this exists
//!
//! DCENT_OS bakes a HAL-level write denylist into its `I2cBus` (`set_write_denylist`)
//! so a code bug cannot corrupt the hashboard EEPROM calibration store at I2C
//! addresses `0x50..=0x57` (the `.74` hb2 EEPROM corruption incident, 2026-04-29).
//! BitAxe has **no analogous persistent I2C calibration store**: board identity /
//! tuning lives in the ESP32's internal NVS flash (not reachable by any I2C
//! peripheral write), the DS4432U DAC registers are volatile current-sink codes,
//! and the firmware never issues the TPS546 `STORE_USER_ALL` NVM-commit command.
//!
//! The one genuinely-corruptible-by-a-buggy-write surface on BitAxe is the
//! **TPS546 protection-limit register set** — the OV / OC / OT / UV fault
//! thresholds that are the regulator's last-line *hardware* protection for the
//! ASIC rail. Those registers are volatile, but a buggy autotuner / MCP / REST
//! path (the raw `I2cBus::write*` primitives are public and reachable) landing a
//! stray write on one of them could *raise* a fault threshold above a safe value
//! and silently defeat the regulator's own protection while the chips run.
//!
//! This module is the SINGLE SOURCE OF TRUTH for which TPS546 registers belong
//! to that protected set and for the predicate that decides whether a given
//! `(addr, register)` write must be refused. It is deliberately NOT gated to the
//! ESP-IDF target (like `safety` / `cml_escalation` / `temp_decode`): every item
//! is a `const fn` / `const` over plain integers with no `esp-idf-hal` / `log` /
//! heap dependency, so the policy host-compiles and its truth table is unit-tested
//! on the host (`cargo test -p dcentaxe-core` / `-p dcentaxe-hal`). The espidf-only
//! `i2c::I2cBus` write path consults `is_protected_register` so a driver and the
//! guard can never disagree about which register is protected.
//!
//! ## Default-preserving (XPSAFE-2 triage: land-gated-default-off)
//!
//! The guard is **disarmed by default** (`GuardState::default().armed == false`).
//! On a field-proven board nothing changes: `is_write_blocked` returns `false`
//! for every register until the platform explicitly arms the guard AND latches it
//! at the end of `PowerManager` init (after the legitimate `configure_limits`
//! pass has written the thresholds). Reads are NEVER affected — only writes to the
//! protected set, and only once armed+latched. Legitimate re-init writes (a fresh
//! `PowerManager::new`, which constructs a new bus state) are therefore never
//! blocked; only post-init stray writes are.

/// TPS546 PMBus I2C address (matches `power::TPS546_ADDR`). The guard only ever
/// applies to writes targeting this device; every other I2C address (DS4432U,
/// INA260, EMC2101/2103, temp sensors) is unaffected.
pub const TPS546_ADDR: u8 = 0x24;

/// The TPS546 protection-limit / fault-response register set.
///
/// These are the regulator's last-line hardware protection thresholds plus the
/// fault-RESPONSE policy bytes. They are written exactly once, by
/// `power::Tps546::configure_limits`, during `PowerManager` init. After init the
/// only legitimate runtime writes to the TPS546 are `VOUT_COMMAND` (0x21, the
/// per-tick core-voltage setpoint) and `OPERATION` (0x01, on/off) — neither of
/// which is in this set, so the guard never interferes with normal voltage
/// control.
///
/// Register codes are duplicated from the `power::pmbus` module **by value**
/// (this module is dependency-free and pure on purpose, and `power.rs` is
/// `cfg(target_os = "espidf")`-gated so it cannot be referenced from a host
/// test). The duplication is therefore kept in lockstep **manually**: any change
/// to a PMBus fault-limit/fault-response code in `power::pmbus` must be mirrored
/// here. These are PMBus standard command codes and must never be changed.
pub const PROTECTED_REGISTERS: &[u8] = &[
    // ── VOUT protection thresholds (ULINEAR16) ──────────────────────────────
    0x40, // VOUT_OV_FAULT_LIMIT  — output overvoltage FAULT (last-line)
    0x42, // VOUT_OV_WARN_LIMIT   — output overvoltage warning
    0x43, // VOUT_UV_WARN_LIMIT   — output undervoltage warning
    0x44, // VOUT_UV_FAULT_LIMIT  — output undervoltage FAULT
    0x2B, // VOUT_MIN             — output voltage floor clamp
    0x24, // VOUT_MAX             — output voltage ceiling clamp
    // ── VIN protection thresholds (Linear11) ────────────────────────────────
    0x55, // VIN_OV_FAULT_LIMIT   — input overvoltage FAULT
    0x58, // VIN_UV_WARN_LIMIT    — input undervoltage warning
    0x35, // VIN_ON               — input turn-on threshold
    0x36, // VIN_OFF              — input turn-off threshold
    // ── IOUT protection thresholds (Linear11) ───────────────────────────────
    0x46, // IOUT_OC_FAULT_LIMIT  — output overcurrent FAULT (last-line)
    0x4A, // IOUT_OC_WARN_LIMIT   — output overcurrent warning
    // ── Die over-temperature thresholds (Linear11) ──────────────────────────
    0x4F, // OT_FAULT_LIMIT       — TPS546 die over-temp FAULT (last-line)
    0x51, // OT_WARN_LIMIT        — TPS546 die over-temp warning
    // ── Fault-RESPONSE policy bytes (how the IC reacts to a fault) ───────────
    0x5F, // VIN_OV_FAULT_RESPONSE
    0x47, // IOUT_OC_FAULT_RESPONSE
    0x50, // OT_FAULT_RESPONSE
];

/// Returns `true` if `(addr, register)` targets a protected TPS546 fault-limit /
/// fault-response register.
///
/// `register` is the PMBus command code, i.e. the **first byte** of the I2C write
/// payload (`[reg, data..]`). A write whose payload is empty (a bare-address
/// probe / `CLEAR_FAULTS` is a single 0x03 byte, never in the protected set) is
/// not protected. Any write to a non-TPS546 address is not protected.
pub const fn is_protected_register(addr: u8, register: u8) -> bool {
    if addr != TPS546_ADDR {
        return false;
    }
    // const-fn linear scan over the small fixed set.
    let mut i = 0;
    while i < PROTECTED_REGISTERS.len() {
        if PROTECTED_REGISTERS[i] == register {
            return true;
        }
        i += 1;
    }
    false
}

/// HAL-side latch state for the TPS546 fault-limit write guard.
///
/// Mirrors the spirit of DCENT_OS's per-bus denylist, but as an opt-in,
/// latch-after-init guard rather than an always-on address denylist (BitAxe has
/// no EEPROM to deny outright, and the protected registers MUST be writable
/// during init). Held by `i2c::I2cBus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardState {
    /// Whether the platform has opted into the fault-limit guard at all
    /// (XPSAFE-2 default-off). `false` ⇒ the guard is fully inert and every
    /// write behaves exactly as before this feature existed.
    pub armed: bool,
    /// Whether init has finished and the guard is now enforcing. Writes to the
    /// protected set are only blocked when `armed && latched`. Set once, at the
    /// end of `PowerManager` init, after the legitimate `configure_limits` pass.
    pub latched: bool,
    /// Count of protected-register writes refused since arm. Surfaced to
    /// telemetry/logs so a latent bug that keeps hammering a fault limit is
    /// visible instead of silent.
    pub blocked_count: u64,
}

impl Default for GuardState {
    /// Default-preserving: disarmed, not latched, nothing blocked. A board that
    /// never calls `arm()` keeps its exact pre-XPSAFE-2 behavior.
    fn default() -> Self {
        Self {
            armed: false,
            latched: false,
            blocked_count: 0,
        }
    }
}

impl GuardState {
    /// Opt into the fault-limit guard (does NOT start enforcing yet — call
    /// `latch()` after init). Idempotent.
    ///
    /// Not a `const fn` (it takes `&mut self`) so the module compiles on older
    /// toolchains that predate const-mutable-reference stabilization; the
    /// predicate methods below stay `const` for compile-time use in tests.
    pub fn arm(&mut self) {
        self.armed = true;
    }

    /// Begin enforcing the guard. No-op if not armed (so a stray latch on a
    /// board that never opted in can't accidentally start blocking). Idempotent.
    pub fn latch(&mut self) {
        if self.armed {
            self.latched = true;
        }
    }

    /// Is the guard currently enforcing writes? (`armed && latched`).
    pub const fn enforcing(&self) -> bool {
        self.armed && self.latched
    }

    /// Decide whether a write to `(addr, register)` must be refused.
    ///
    /// Returns `true` ONLY when the guard is enforcing AND the target is a
    /// protected TPS546 register. The caller is responsible for bumping
    /// `blocked_count` (via `record_block`) and returning a HAL error — keeping
    /// the decision pure and the side effect explicit.
    pub const fn is_write_blocked(&self, addr: u8, register: u8) -> bool {
        self.enforcing() && is_protected_register(addr, register)
    }

    /// Record that one protected-register write was refused. Saturating so a
    /// runaway bug can never wrap the counter back to a small number.
    /// Not `const` (takes `&mut self`) for older-toolchain compatibility.
    pub fn record_block(&mut self) {
        self.blocked_count = self.blocked_count.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_set_covers_the_last_line_fault_limits() {
        // The three last-line hardware protections MUST be protected.
        assert!(is_protected_register(TPS546_ADDR, 0x40)); // VOUT_OV_FAULT_LIMIT
        assert!(is_protected_register(TPS546_ADDR, 0x46)); // IOUT_OC_FAULT_LIMIT
        assert!(is_protected_register(TPS546_ADDR, 0x4F)); // OT_FAULT_LIMIT
        assert!(is_protected_register(TPS546_ADDR, 0x55)); // VIN_OV_FAULT_LIMIT
    }

    #[test]
    fn normal_voltage_control_registers_are_never_protected() {
        // The two registers the runtime touches every tick MUST stay writable
        // even when the guard is fully armed+latched, or normal mining breaks.
        // VOUT_COMMAND (0x21) and OPERATION (0x01) are not in the set.
        assert!(!is_protected_register(TPS546_ADDR, 0x21)); // VOUT_COMMAND
        assert!(!is_protected_register(TPS546_ADDR, 0x01)); // OPERATION
        assert!(!is_protected_register(TPS546_ADDR, 0x03)); // CLEAR_FAULTS (1-byte)
        assert!(!is_protected_register(TPS546_ADDR, 0x02)); // ON_OFF_CONFIG
                                                            // ...and read-only status / telemetry registers are never protected.
        assert!(!is_protected_register(TPS546_ADDR, 0x79)); // STATUS_WORD
        assert!(!is_protected_register(TPS546_ADDR, 0x8B)); // READ_VOUT
    }

    #[test]
    fn guard_never_touches_other_i2c_devices() {
        // DS4432U (0x48), INA260 (0x40 device addr), EMC2101 (0x4C),
        // EMC2103 (0x2E) — even a register code that collides with a protected
        // TPS546 code must NOT be guarded on a different device address.
        for &dev in &[0x48u8, 0x4C, 0x2E, 0x40, 0x49, 0x4A, 0x4B] {
            if dev == TPS546_ADDR {
                continue;
            }
            // 0x40 is a protected TPS546 reg code; on another device it's free.
            assert!(!is_protected_register(dev, 0x40));
            assert!(!is_protected_register(dev, 0x46));
            assert!(!is_protected_register(dev, 0x4F));
        }
    }

    #[test]
    fn guard_disarmed_by_default_blocks_nothing() {
        // XPSAFE-2 default-off: a fresh GuardState must let every write through,
        // including a protected register — default behavior is fully preserved.
        let g = GuardState::default();
        assert!(!g.armed);
        assert!(!g.latched);
        assert!(!g.enforcing());
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x40));
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x46));
    }

    #[test]
    fn armed_but_not_latched_does_not_enforce_during_init() {
        // Between arm() and latch() (i.e. during configure_limits) the
        // legitimate fault-limit writes MUST still go through.
        let mut g = GuardState::default();
        g.arm();
        assert!(g.armed);
        assert!(!g.enforcing());
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x40));
    }

    #[test]
    fn armed_and_latched_blocks_only_protected_writes() {
        let mut g = GuardState::default();
        g.arm();
        g.latch();
        assert!(g.enforcing());
        // Protected fault-limit writes are now refused...
        assert!(g.is_write_blocked(TPS546_ADDR, 0x40));
        assert!(g.is_write_blocked(TPS546_ADDR, 0x4F));
        // ...but the live voltage setpoint + enable path are still allowed.
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x21)); // VOUT_COMMAND
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x01)); // OPERATION
                                                         // ...and a different device is entirely unaffected.
        assert!(!g.is_write_blocked(0x48, 0x40)); // DS4432U
    }

    #[test]
    fn latch_is_a_noop_without_arm() {
        // A stray latch() on a board that never opted in must not start blocking.
        let mut g = GuardState::default();
        g.latch();
        assert!(!g.armed);
        assert!(!g.latched);
        assert!(!g.enforcing());
        assert!(!g.is_write_blocked(TPS546_ADDR, 0x40));
    }

    #[test]
    fn record_block_counts_and_saturates() {
        let mut g = GuardState::default();
        g.arm();
        g.latch();
        g.record_block();
        g.record_block();
        assert_eq!(g.blocked_count, 2);
        // Saturating: priming near the ceiling must not wrap to a small value.
        g.blocked_count = u64::MAX - 1;
        g.record_block();
        g.record_block(); // would overflow without saturation
        assert_eq!(g.blocked_count, u64::MAX);
    }

    #[test]
    fn protected_set_has_no_accidental_overlap_with_runtime_writes() {
        // Defensive: enumerate the registers the *runtime* (post-init) path can
        // write and assert none of them are in the protected set. If a future
        // edit adds VOUT_COMMAND/OPERATION/ON_OFF_CONFIG/CLEAR_FAULTS to the set
        // it would brick normal mining — this test catches that at host-test time.
        const RUNTIME_WRITABLE: &[u8] = &[
            0x21, // VOUT_COMMAND (every-tick setpoint)
            0x01, // OPERATION (enable/disable)
            0x02, // ON_OFF_CONFIG (init, before latch)
            0x03, // CLEAR_FAULTS (fault-clear opcode; transient-CML tolerance)
        ];
        for &reg in RUNTIME_WRITABLE {
            assert!(
                !PROTECTED_REGISTERS.contains(&reg),
                "register 0x{reg:02x} is a runtime-writable command and must NOT \
                 be in PROTECTED_REGISTERS (would break normal voltage control)"
            );
        }
    }

    #[test]
    fn protected_set_is_deduplicated() {
        // A duplicate entry would be harmless functionally but signals a sloppy
        // edit; pin uniqueness so the table stays a clean source of truth.
        let mut seen = std::collections::BTreeSet::new();
        for &reg in PROTECTED_REGISTERS {
            assert!(seen.insert(reg), "duplicate protected register 0x{reg:02x}");
        }
    }
}
