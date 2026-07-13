//! Per-model silicon view backed directly by `dcentrald-silicon-profiles`.
//!
//! No PLL frequency or voltage envelope is copied into the simulator. This
//! adapter selects the canonical table and exposes lookups over its rows, so
//! later golden-driver checks compare emitted values against the same data the
//! runtime/autotuner already consumes.

use dcentrald_silicon_profiles::{Profile, SiliconTable};

use super::{SimBoardProfile, SimModel};

#[derive(Debug, Clone, Copy)]
pub struct SimSiliconState {
    board: SimBoardProfile,
    table: &'static SiliconTable,
}

impl SimSiliconState {
    pub const fn for_profile(board: SimBoardProfile) -> Self {
        use dcentrald_silicon_profiles::{
            bm1362::BM1362_TABLE, bm1366::BM1366_TABLE, bm1368::BM1368_TABLE, bm1370::BM1370_TABLE,
            bm1373::BM1373_TABLE, bm1387::BM1387_TABLE, bm1391::BM1391_TABLE, bm1396::BM1396_TABLE,
            bm1397::BM1397_TABLE, bm1398::BM1398_TABLE,
        };
        use SimModel::*;

        let table = match board.model {
            S9 => &BM1387_TABLE,
            S11 | S15 | T15 => &BM1391_TABLE,
            S17 | S17Pro | T17 | S17e => &BM1397_TABLE,
            S17Plus | T17Plus => &BM1396_TABLE,
            S19 | S19Pro => &BM1398_TABLE,
            S19jPro => &BM1362_TABLE,
            S19Xp | S19kPro => &BM1366_TABLE,
            S21 => &BM1368_TABLE,
            S21Pro | S21Xp => &BM1370_TABLE,
            S23 => &BM1373_TABLE,
        };
        Self { board, table }
    }

    pub const fn board(&self) -> SimBoardProfile {
        self.board
    }

    pub const fn table(&self) -> &'static SiliconTable {
        self.table
    }

    pub fn default_operating_point(&self) -> Option<&'static Profile> {
        self.table.default_profile()
    }

    pub fn profile_for_frequency(&self, freq_mhz: u32) -> Option<&'static Profile> {
        self.table
            .profiles
            .iter()
            .find(|profile| profile.freq_mhz == freq_mhz)
    }

    pub fn voltage_envelope_v(&self) -> Option<(f32, f32)> {
        let mut voltages = self.table.profiles.iter().map(|profile| profile.voltage_v);
        let first = voltages.next()?;
        Some(voltages.fold((first, first), |(min, max), voltage| {
            (min.min(voltage), max.max(voltage))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcentrald_silicon_profiles::{bm1373::BM1373_TABLE, bm1398::BM1398_TABLE};

    #[test]
    fn s19pro_uses_canonical_bm1398_table_without_copying_rows() {
        let state = SimSiliconState::for_profile(SimBoardProfile::for_model(SimModel::S19Pro));
        assert_eq!(state.table().chip_family, BM1398_TABLE.chip_family);
        assert_eq!(state.table().profiles, BM1398_TABLE.profiles);
        assert_eq!(
            state
                .default_operating_point()
                .map(|profile| profile.freq_mhz),
            BM1398_TABLE
                .default_profile()
                .map(|profile| profile.freq_mhz)
        );
    }

    #[test]
    fn s23_uses_experimental_table_but_keeps_board_geometry_unknown() {
        let state = SimSiliconState::for_profile(SimBoardProfile::for_model(SimModel::S23));
        assert_eq!(state.table().chip_family, BM1373_TABLE.chip_family);
        assert_eq!(state.table().profiles, BM1373_TABLE.profiles);
        assert_eq!(state.board().chips_per_chain, None);
    }

    #[test]
    fn voltage_envelope_is_derived_from_table_rows() {
        let state = SimSiliconState::for_profile(SimBoardProfile::for_model(SimModel::S19Pro));
        let expected_min = BM1398_TABLE
            .profiles
            .iter()
            .map(|profile| profile.voltage_v)
            .fold(f32::INFINITY, f32::min);
        let expected_max = BM1398_TABLE
            .profiles
            .iter()
            .map(|profile| profile.voltage_v)
            .fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(
            state.voltage_envelope_v(),
            Some((expected_min, expected_max))
        );
    }
}
