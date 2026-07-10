//! AT-1: the autotuner's measured chip-rail voltage input.
//!
//! This is the autotuner-facing accessor for the AT-1 read-back spine that the
//! AT-3..13 DVFS efficiency build depends on. The pure resolver + the 0x3A
//! `MEASURE_VOLTAGE` decode live in the no-HAL leaf crate
//! [`dcentrald_common::chain_voltage`] (host-testable, single source of truth
//! reachable from both the autotuner and the `dcentrald-api` telemetry surface).
//! This module re-exports that surface and injects the canonical dsPIC DAC-span
//! ceiling [`dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV`] so callers never have
//! to thread the plausibility ceiling by hand.
//!
//! AT-1 is READ-BACK only — it provides no closed-loop voltage *adjustment*
//! (that is AT-3..13, out of scope) and the autotuner stays default-disabled.
//! When no valid measured 0x3A reading is available, every accessor falls back
//! to the commanded voltage, tagged, leaving the existing open-loop behavior
//! byte-identical.

pub use dcentrald_common::chain_voltage::{plausible_rail_mv, ChainRailVoltage, RailVoltageSource};

/// The dsPIC DAC-span ceiling used as the AT-1 rail-voltage plausibility bound.
///
/// Single source of truth: re-exported from `dcentrald-asic`. The pure resolver
/// in `dcentrald-common` is parameterized by this value; the wrappers below
/// inject it so a drift between the two crates is impossible at the call site.
/// The [`tests::rail_max_mv_is_the_dspic_dac_span_ceiling`] unit test pins it to
/// the asic constant + the `dcentrald-common` test mirror (15140 mV).
pub const RAIL_MAX_MV: u16 = dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV;

/// Resolve a per-chain rail voltage from the best available source (AT-1),
/// using the canonical [`RAIL_MAX_MV`] plausibility ceiling.
///
/// Thin wrapper over [`ChainRailVoltage::resolve`] — see that for the full
/// priority order (measured → commanded → profile-default → unknown). `None`
/// for `measured_mv` reproduces the existing commanded-only behavior exactly.
#[inline]
pub fn resolve_chain_rail_voltage(
    chain_id: u8,
    measured_mv: Option<u16>,
    commanded_mv: Option<u16>,
    default_mv: Option<u16>,
) -> ChainRailVoltage {
    ChainRailVoltage::resolve(chain_id, measured_mv, commanded_mv, default_mv, RAIL_MAX_MV)
}

/// Resolve a per-chain rail voltage directly from a raw dsPIC `MEASURE_VOLTAGE`
/// (0x3A) ADC reply, using the canonical [`RAIL_MAX_MV`] ceiling.
///
/// This is the literal AT-1 wire: the daemon hands the post-envelope ADC reply
/// bytes from `DspicService::measure_voltage` (or a raw `/dev/i2c-0` envelope
/// capture) and gets back a measured-or-commanded, provenance-tagged rail
/// voltage. Thin wrapper over [`ChainRailVoltage::from_measure_voltage_reply`].
#[inline]
pub fn measured_rail_from_0x3a_reply(
    chain_id: u8,
    reply: &[u8],
    commanded_mv: Option<u16>,
    default_mv: Option<u16>,
) -> ChainRailVoltage {
    ChainRailVoltage::from_measure_voltage_reply(
        chain_id,
        reply,
        commanded_mv,
        default_mv,
        RAIL_MAX_MV,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_max_mv_is_the_dspic_dac_span_ceiling() {
        // Pins the AT-1 plausibility ceiling to the asic single-source-of-truth
        // constant (15140 mV). If the dsPIC DAC span ever changes, this and the
        // dcentrald-common test mirror must move together.
        assert_eq!(RAIL_MAX_MV, 15_140);
        assert_eq!(RAIL_MAX_MV, dcentrald_asic::dspic::DSPIC_MAX_VOLTAGE_MV);
    }

    #[test]
    fn wrapper_injects_ceiling_and_prefers_measured() {
        let rail = resolve_chain_rail_voltage(6, Some(13_700), Some(13_800), Some(13_500));
        assert_eq!(rail.source, RailVoltageSource::Measured);
        assert_eq!(rail.mv, 13_700);
        // An over-ceiling measured value is rejected by the injected RAIL_MAX_MV.
        let over = resolve_chain_rail_voltage(6, Some(RAIL_MAX_MV + 1), Some(13_800), None);
        assert_eq!(over.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(over.mv, 13_800);
    }

    #[test]
    fn wrapper_decodes_0x3a_reply_to_measured() {
        // raw=574 (0x023E) -> 13,702 mV. Commanded fallback ignored when measured.
        let rail = measured_rail_from_0x3a_reply(7, &[0x02, 0x3E], Some(13_800), None);
        assert_eq!(rail.source, RailVoltageSource::Measured);
        assert_eq!(rail.mv, 13_702);
    }

    #[test]
    fn wrapper_0x3a_misframe_falls_back_to_commanded() {
        let rail = measured_rail_from_0x3a_reply(7, &[0xFF, 0xFF], Some(13_700), None);
        assert_eq!(rail.source, RailVoltageSource::CommandedNotMeasured);
        assert_eq!(rail.mv, 13_700);
    }
}
