//! AT-1: measured chip-rail voltage input for the autotuner.
//!
//! This is the **foundational read-back accessor** that the AT-3..13 DVFS
//! efficiency build sits on top of. It does NOT close any loop and never
//! *adjusts* voltage — it only turns a decoded dsPIC reading into a single,
//! provenance-tagged per-chain rail voltage that the autotuner and the
//! telemetry surfaces can consume. Today every voltage the autotuner sees is
//! the *commanded* (open-loop) DAC setpoint; AT-1 lets a *measured* value take
//! priority where one is available, while falling back — cleanly and tagged —
//! to commanded when it is not.
//!
//! Why it lives here (pure, no-HAL, host-testable):
//!   - The 0x3A `MEASURE_VOLTAGE` decode + plausibility guard already live in
//!     [`crate::dspic_decode`]. This module reuses that decoder verbatim, so the
//!     measured value AT-1 publishes is exactly what `DspicService::measure_voltage`
//!     produces on the live unit — same scale, same misframe rejection.
//!   - `dcentrald-autotuner` and `dcentrald-api` (the telemetry projection) both
//!     consume this. Putting it in the no-HAL leaf crate keeps a single source of
//!     truth reachable from both without dragging in a hardware dependency, and
//!     lets the AT-1 logic be unit-tested on the Windows host (the autotuner /
//!     api crates are HAL-blocked there).
//!
//! Firmware-scale note (RE-ASK-DSPIC-3A-FW8A-SCALE): the 0x3A ADC decode uses
//! the Ghidra-proven fw=0x89 affine fit `volts = raw * 0.02448 - 0.35`. fw=0x8A
//! is currently assumed to share that scale (no independent 0x8A selector path
//! was found in the local `bosminer.bin` RE, and the local trace reported fw=0x89
//! for both `a lab unit` slaves). AT-1 is therefore firmware-agnostic on decode: it
//! consumes whatever [`crate::dspic_decode`] returns and does NOT apply a second,
//! guessed 0x8A scale. If a live `a lab unit` 0x3A capture later proves a distinct 0x8A
//! scale, the fix belongs in `dspic_decode`, not here.
//!
//! Safety scope: AT-1 is READ-BACK only. It performs no I/O itself — the caller
//! supplies either a pre-decoded `Option<u16>` or the raw post-envelope ADC reply
//! bytes. It must never be wired into a hot-path periodic dsPIC read (the 0x3A
//! `I2C_RDWR` read phase can corrupt the dsPIC parser — see
//! `DspicService::measure_voltage`); a safe quiet-window read cadence is AT-3..13
//! work, out of scope here.
//!
//! No serde here on purpose: `dcentrald-common` is intentionally dependency-free
//! (see its `Cargo.toml`). The API surface serializes the `&'static str` tag from
//! [`RailVoltageSource::as_str`], not these types directly, so no derive is needed.

/// Provenance of a resolved per-chain rail-voltage reading.
///
/// The string forms returned by [`RailVoltageSource::as_str`] are the canonical
/// tags already used by the `dcentrald-api` per-chain telemetry projection
/// (`/api/status` `voltage_source`), so a caller can delegate its tagging to
/// this resolver without changing the wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailVoltageSource {
    /// Decoded from a live dsPIC `MEASURE_VOLTAGE` (0x3A) analog-ADC reply — a
    /// real rail reading, not a setpoint. This is the AT-1 value.
    Measured,
    /// The DAC value commanded to this specific chain. There is no per-chain ADC
    /// reading available (or it was implausible), so this is the open-loop
    /// setpoint, NOT a measured rail.
    CommandedNotMeasured,
    /// The chip-profile default voltage — the chain has not been individually
    /// commanded yet and no measured reading is available.
    CommandedDefault,
    /// No measured reading, no commanded value, and no profile default. The
    /// honest "we do not know this chain's rail voltage" state (never a fake 0).
    Unknown,
}

impl RailVoltageSource {
    /// Canonical wire tag. Matches the existing `dcentrald-api` `voltage_source`
    /// vocabulary exactly so the telemetry projection can adopt this resolver
    /// without a contract change.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::CommandedNotMeasured => "commanded_not_measured",
            Self::CommandedDefault => "commanded_default",
            Self::Unknown => "unknown",
        }
    }

    /// True only for a genuine measured (0x3A ADC) reading.
    pub const fn is_measured(self) -> bool {
        matches!(self, Self::Measured)
    }
}

/// A resolved per-chain rail voltage with provenance.
///
/// `mv` is `0` only when `source == Unknown`. A measured or commanded result
/// always carries a plausible, positive millivolt value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainRailVoltage {
    pub chain_id: u8,
    pub mv: u16,
    pub source: RailVoltageSource,
}

/// Plausibility gate for a rail-voltage millivolt value.
///
/// Rejects `0` (a dead/unread rail is not a usable rail voltage for the power
/// model — it should fall back to commanded, tagged) and anything above the
/// dsPIC DAC span ceiling `max_mv` (e.g. a `0xFFFF = 65535` misframe, or the
/// `0x3A00` cmd-echo-shape artifact). The 0x3A decoder in [`crate::dspic_decode`]
/// already applies the same ceiling on the way in; this is a defensive
/// belt-and-suspenders re-check at the AT-1 boundary so a pre-decoded
/// `Option<u16>` from any other path cannot smuggle an implausible value into the
/// "measured" tag.
pub const fn plausible_rail_mv(mv: u16, max_mv: u16) -> bool {
    mv > 0 && mv <= max_mv
}

impl ChainRailVoltage {
    /// Resolve a per-chain rail voltage from the best available source (AT-1).
    ///
    /// Priority:
    ///   1. `measured_mv` if present AND plausible → [`RailVoltageSource::Measured`].
    ///   2. `commanded_mv` if present and `> 0` → [`RailVoltageSource::CommandedNotMeasured`].
    ///   3. `default_mv` if present and `> 0` → [`RailVoltageSource::CommandedDefault`].
    ///   4. otherwise → [`RailVoltageSource::Unknown`] (`mv = 0`).
    ///
    /// `max_mv` is the dsPIC DAC-span ceiling used by the plausibility gate
    /// (callers in the daemon / autotuner pass `dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV`).
    ///
    /// This is deliberately additive and conservative: when `measured_mv` is
    /// `None` (the default until a safe quiet-window 0x3A read is wired in
    /// AT-3..13), the result is byte-identical to the existing commanded-only
    /// projection.
    pub fn resolve(
        chain_id: u8,
        measured_mv: Option<u16>,
        commanded_mv: Option<u16>,
        default_mv: Option<u16>,
        max_mv: u16,
    ) -> Self {
        if let Some(mv) = measured_mv {
            if plausible_rail_mv(mv, max_mv) {
                return Self {
                    chain_id,
                    mv,
                    source: RailVoltageSource::Measured,
                };
            }
        }
        if let Some(mv) = commanded_mv.filter(|&v| v > 0) {
            return Self {
                chain_id,
                mv,
                source: RailVoltageSource::CommandedNotMeasured,
            };
        }
        if let Some(mv) = default_mv.filter(|&v| v > 0) {
            return Self {
                chain_id,
                mv,
                source: RailVoltageSource::CommandedDefault,
            };
        }
        Self {
            chain_id,
            mv: 0,
            source: RailVoltageSource::Unknown,
        }
    }

    /// Resolve directly from a raw dsPIC `MEASURE_VOLTAGE` (0x3A) ADC reply.
    ///
    /// This is the literal AT-1 wire from the existing 0x3A ADC decoder to the
    /// autotuner's measured-voltage input: it runs the reply through
    /// [`crate::dspic_decode::decode_framed_measure_voltage_i2c0_capture`] (which
    /// accepts both the post-envelope 2-byte ADC buffer that
    /// `DspicService::measure_voltage` consumes AND a raw 7-byte `/dev/i2c-0`
    /// envelope capture), and on a successful + plausible decode tags the result
    /// `Measured`. Any decode error (misframe / `0xFFFF` / `ZeroRail` dead-rail /
    /// too-short) is NOT a measured value — the result falls back to the
    /// commanded voltage, tagged, exactly as [`Self::resolve`] does with
    /// `measured_mv = None`.
    ///
    /// Firmware-scale: the decode is the fw=0x89 affine fit; fw=0x8A is assumed
    /// to share it pending a live `a lab unit` 0x3A capture (RE-ASK-DSPIC-3A-FW8A-SCALE).
    /// See the module header — do NOT introduce a guessed 0x8A scale here.
    pub fn from_measure_voltage_reply(
        chain_id: u8,
        reply: &[u8],
        commanded_mv: Option<u16>,
        default_mv: Option<u16>,
        max_mv: u16,
    ) -> Self {
        let measured_mv =
            crate::dspic_decode::decode_framed_measure_voltage_i2c0_capture(reply, max_mv).ok();
        Self::resolve(chain_id, measured_mv, commanded_mv, default_mv, max_mv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirror of dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV (the dsPIC DAC-span
    // ceiling) for these pure tests only. Production code never hardcodes this:
    // the resolver is parameterized by `max_mv`, and the dcentrald-autotuner
    // wrapper injects the real asic constant. A drift guard against the asic
    // value lives in the autotuner test
    // `chain_voltage::tests::rail_max_mv_is_the_dspic_dac_span_ceiling`.
    const MAX: u16 = 15_140;

    // -- plausibility gate ----------------------------------------------------

    #[test]
    fn plausibility_gate_rejects_zero_over_max_and_misframes() {
        assert!(
            !plausible_rail_mv(0, MAX),
            "0 mV (dead/unread) is not a usable rail"
        );
        assert!(plausible_rail_mv(1, MAX));
        assert!(
            plausible_rail_mv(13_700, MAX),
            "a real chip rail ~13.7 V is plausible"
        );
        assert!(
            plausible_rail_mv(MAX, MAX),
            "exactly the ceiling is allowed"
        );
        assert!(!plausible_rail_mv(MAX + 1, MAX));
        assert!(
            !plausible_rail_mv(0xFFFF, MAX),
            "0xFFFF misframe rejected by ceiling"
        );
    }

    // -- 0x3A decode wire -----------------------------------------------------

    #[test]
    fn from_0x3a_reply_decodes_known_bytes_to_measured() {
        // raw=574 (0x023E) -> round(574 * 24.48 mV - 350 mV) = 13,702 mV.
        let rail = ChainRailVoltage::from_measure_voltage_reply(
            6,
            &[0x02, 0x3E],
            Some(13_800), // commanded fallback that must be IGNORED in favor of measured
            Some(13_500),
            MAX,
        );
        assert_eq!(rail.source, RailVoltageSource::Measured);
        assert_eq!(rail.mv, 13_702);
        assert_eq!(rail.chain_id, 6);
        assert!(rail.source.is_measured());
    }

    #[test]
    fn from_0x3a_reply_accepts_raw_i2c0_envelope() {
        // The 7-byte /dev/i2c-0 envelope [len,cmd,status,adc_hi,adc_lo,zero,cksum];
        // ADC payload 0x0222 -> 13,016 mV. Proves AT-1 handles a raw capture too.
        let rail = ChainRailVoltage::from_measure_voltage_reply(
            7,
            &[0x07, 0x3A, 0x01, 0x02, 0x22, 0x00, 0x66],
            Some(13_800),
            None,
            MAX,
        );
        assert_eq!(rail.source, RailVoltageSource::Measured);
        assert_eq!(rail.mv, 13_016);
    }

    #[test]
    fn from_0x3a_reply_ffff_misframe_falls_back_to_commanded() {
        // The live regression shape: a framed fw=0x89 reply blind-read as 0xFCF8 /
        // 0xFFFF. The decoder rejects it (ExceedsMax); AT-1 must NOT tag it
        // measured — it falls back to the commanded value, tagged not-measured.
        let rail = ChainRailVoltage::from_measure_voltage_reply(
            6,
            &[0xFF, 0xFF],
            Some(13_700),
            Some(13_500),
            MAX,
        );
        assert_eq!(rail.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(rail.mv, 13_700);
        assert!(!rail.source.is_measured());
    }

    #[test]
    fn from_0x3a_reply_cmd_echo_shape_falls_back_to_commanded() {
        // A [cmd_echo, status, ...] shaped reply decodes raw=0x3A00 -> impossible.
        // Not a measured value -> commanded fallback.
        let rail = ChainRailVoltage::from_measure_voltage_reply(
            8,
            &[0x3A, 0x00, 0x35, 0x84],
            Some(13_650),
            None,
            MAX,
        );
        assert_eq!(rail.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(rail.mv, 13_650);
    }

    #[test]
    fn from_0x3a_reply_dead_rail_falls_back_to_commanded_not_a_fake_zero() {
        // raw=0 -> ZeroRail (a trustworthy de-energized verdict in the decoder),
        // but AT-1's job is to feed the POWER model a usable rail voltage: a
        // dead-rail read is NOT a measured operating voltage, so it falls back to
        // the commanded value rather than publishing a measured 0.
        let rail =
            ChainRailVoltage::from_measure_voltage_reply(6, &[0x00, 0x00], Some(13_700), None, MAX);
        assert_eq!(rail.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(rail.mv, 13_700);
    }

    // -- resolver (pre-decoded Option<u16>) -----------------------------------

    #[test]
    fn resolve_prefers_measured_when_valid() {
        let rail = ChainRailVoltage::resolve(6, Some(13_700), Some(13_800), Some(13_500), MAX);
        assert_eq!(rail.source, RailVoltageSource::Measured);
        assert_eq!(rail.mv, 13_700);
    }

    #[test]
    fn resolve_rejects_implausible_measured_then_uses_commanded() {
        // measured 0 (dead) -> commanded.
        let dead = ChainRailVoltage::resolve(6, Some(0), Some(13_800), Some(13_500), MAX);
        assert_eq!(dead.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(dead.mv, 13_800);
        // measured over-max -> commanded.
        let over = ChainRailVoltage::resolve(6, Some(20_000), Some(13_800), Some(13_500), MAX);
        assert_eq!(over.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(over.mv, 13_800);
    }

    #[test]
    fn resolve_commanded_default_when_chain_not_individually_commanded() {
        let rail = ChainRailVoltage::resolve(6, None, None, Some(13_500), MAX);
        assert_eq!(rail.source, RailVoltageSource::CommandedDefault);
        assert_eq!(rail.mv, 13_500);
    }

    #[test]
    fn resolve_unknown_when_nothing_available() {
        let rail = ChainRailVoltage::resolve(6, None, None, None, MAX);
        assert_eq!(rail.source, RailVoltageSource::Unknown);
        assert_eq!(rail.mv, 0);
        // A zero commanded / zero default is treated as "not present" too.
        let rail0 = ChainRailVoltage::resolve(6, None, Some(0), Some(0), MAX);
        assert_eq!(rail0.source, RailVoltageSource::Unknown);
        assert_eq!(rail0.mv, 0);
    }

    // -- wire-contract pins ---------------------------------------------------

    #[test]
    fn source_tags_match_the_api_voltage_source_contract() {
        assert_eq!(RailVoltageSource::Measured.as_str(), "measured");
        assert_eq!(
            RailVoltageSource::CommandedNotMeasured.as_str(),
            "commanded_not_measured"
        );
        assert_eq!(
            RailVoltageSource::CommandedDefault.as_str(),
            "commanded_default"
        );
        assert_eq!(RailVoltageSource::Unknown.as_str(), "unknown");
    }

    #[test]
    fn measured_none_is_byte_identical_to_commanded_only_projection() {
        // The default AT-1 state (no measured reading) reproduces exactly the
        // existing commanded-only mapping the api projection emits today.
        // c.voltage_mv > 0 -> commanded_not_measured
        let a = ChainRailVoltage::resolve(6, None, Some(8900), Some(8600), MAX);
        assert_eq!(a.mv, 8900);
        assert_eq!(a.source.as_str(), "commanded_not_measured");
        // chain not commanded, profile default present -> commanded_default
        let b = ChainRailVoltage::resolve(7, None, None, Some(8600), MAX);
        assert_eq!(b.mv, 8600);
        assert_eq!(b.source.as_str(), "commanded_default");
        // nothing -> unknown / 0
        let c = ChainRailVoltage::resolve(8, None, None, None, MAX);
        assert_eq!(c.mv, 0);
        assert_eq!(c.source.as_str(), "unknown");
    }
}
