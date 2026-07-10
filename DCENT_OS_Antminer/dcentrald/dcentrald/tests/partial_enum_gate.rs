//! HAL-1: optional partial-enumeration gate.
//!
//! The gate is intentionally opt-in. The `a lab unit` proven 28/126 path must keep
//! working when `min_chip_fraction` is omitted.

use dcentrald_asic::chain::chain_meets_min_fraction;

const CONFIG_RS: &str = include_str!("../src/config.rs");
const WAVE54_LAUNCHER: &str = include_str!("../../../scripts/run_wave54_25_PROVEN_MINING.sh");
const XIL_OVERRIDE: &str = include_str!("../../configs/dcentrald_s19jpro_xil_override.toml");

#[test]
fn partial_enum_fraction_helper_preserves_zero_floor_recipe() {
    assert!(
        chain_meets_min_fraction(28, 126, 0.0),
        "floor 0.0 must preserve the proven .25 28/126 partial-enum path"
    );
    assert!(
        !chain_meets_min_fraction(28, 126, 0.5),
        "operator floor 0.5 must reject 28/126"
    );
    assert!(
        chain_meets_min_fraction(126, 126, 1.0),
        "full population must pass a 100% floor"
    );
}

#[test]
fn mining_config_keeps_partial_enum_gate_optional() {
    assert!(
        CONFIG_RS.contains("pub min_chip_fraction: Option<f32>"),
        "min_chip_fraction must stay optional so absence preserves existing recipes"
    );
    assert!(
        CONFIG_RS.contains("min_chip_fraction: None"),
        "MiningConfig::default must keep min_chip_fraction absent"
    );
}

#[test]
fn wave54_xil_25_recipe_does_not_enable_partial_enum_gate() {
    assert!(
        !WAVE54_LAUNCHER.contains("min_chip_fraction"),
        "Wave-54 proven .25 launcher must not set min_chip_fraction"
    );
    assert!(
        !XIL_OVERRIDE.contains("min_chip_fraction"),
        "XIL .25 override config must not set min_chip_fraction"
    );
}
