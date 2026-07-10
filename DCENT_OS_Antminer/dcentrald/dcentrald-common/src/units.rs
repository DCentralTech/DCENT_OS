//! Hashrate unit conversions + typed guards (GH/s ⇄ TH/s).
//!
//! gap-swarm G62: encodes the documented **1000× GH/s↔TH/s hazard** as named
//! helpers + newtype guards, so code that ingests proto/REST hashrate fields
//! can't silently inflate or deflate a value by 1000×. The hazard is called out
//! in `dcentrald-autotuner::braiinsos_dps_configuration` (the `*_ths`
//! proto-ingest fields, where a raw GH/s reading assigned to a `*_ths` field is
//! a 1000× error). Adopt [`ghs_to_ths`]/[`ths_to_ghs`] or the [`Ghs`]/[`Ths`]
//! newtypes at any hashrate boundary instead of bare `* 1000.0` / `/ 1000.0`.
//!
//! Invariant: `1 TH/s = 1000 GH/s`.
//!
//! HAL-free / OS-free / async-free — host-testable, per this crate's contract.

/// Gigahashes/s per terahash/s. `1 TH/s = 1000 GH/s`.
pub const GHS_PER_THS: f64 = 1000.0;

/// Convert gigahash/s → terahash/s. `1500.0 GH/s → 1.5 TH/s`.
#[must_use]
pub fn ghs_to_ths(ghs: f64) -> f64 {
    ghs / GHS_PER_THS
}

/// Convert terahash/s → gigahash/s. `1.5 TH/s → 1500.0 GH/s`.
#[must_use]
pub fn ths_to_ghs(ths: f64) -> f64 {
    ths * GHS_PER_THS
}

/// Gigahash/s, newtype-guarded so it can't be silently used where a [`Ths`] is
/// expected (the 1000× hazard). Convert explicitly via [`Ghs::to_ths`].
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Ghs(pub f64);

/// Terahash/s, newtype-guarded. Convert explicitly via [`Ths::to_ghs`].
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Ths(pub f64);

impl Ghs {
    /// This GH/s value as TH/s.
    #[must_use]
    pub fn to_ths(self) -> Ths {
        Ths(ghs_to_ths(self.0))
    }
}

impl Ths {
    /// This TH/s value as GH/s.
    #[must_use]
    pub fn to_ghs(self) -> Ghs {
        Ghs(ths_to_ghs(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_and_directional_conversions() {
        assert_eq!(GHS_PER_THS, 1000.0);
        assert_eq!(ghs_to_ths(1500.0), 1.5);
        assert_eq!(ths_to_ghs(1.5), 1500.0);
        // S19j Pro ~100 TH/s == 100_000 GH/s (the exact 1000× the hazard is about)
        assert_eq!(ghs_to_ths(100_000.0), 100.0);
        assert_eq!(ths_to_ghs(100.0), 100_000.0);
        assert_eq!(ghs_to_ths(0.0), 0.0);
    }

    #[test]
    fn round_trips_are_lossless_for_representative_values() {
        for ghs in [0.0_f64, 13_500.0, 100_000.0, 26_500.0] {
            assert_eq!(ths_to_ghs(ghs_to_ths(ghs)), ghs);
        }
    }

    #[test]
    fn newtypes_convert_explicitly_not_silently() {
        assert_eq!(Ghs(13_500.0).to_ths(), Ths(13.5));
        assert_eq!(Ths(13.5).to_ghs(), Ghs(13_500.0));
        // The guard's whole point: Ghs and Ths are distinct types, so a GH/s
        // value can't be passed where a TH/s is expected without an explicit
        // `.to_ths()` (caught at compile time, not runtime).
        assert_ne!(Ghs(1.5), Ghs(1500.0));
    }
}
