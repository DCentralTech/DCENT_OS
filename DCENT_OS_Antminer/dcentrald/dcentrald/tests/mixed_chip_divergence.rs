//! HAL-2: mixed production chip IDs must not be co-driven by one driver.
//!
//! The daemon keeps `a lab unit` XIL log-only by default while the refusal path is
//! bench-soaked; all other platforms enforce divergent production IDs.

use dcentrald_asic::chain::{
    driver_for_chain, driver_for_chain_with_policy, ChainDriverDecision, DivergentChipPolicy,
};

const CONFIG_RS: &str = include_str!("../src/config.rs");
const DAEMON_RS: &str = include_str!("../src/daemon.rs");
const WAVE54_LAUNCHER: &str = include_str!("../../../scripts/run_wave54_25_PROVEN_MINING.sh");
const XIL_OVERRIDE: &str = include_str!("../../configs/dcentrald_s19jpro_xil_override.toml");

#[test]
fn driver_for_chain_skips_divergent_valid_chip_ids() {
    assert_eq!(
        driver_for_chain(0x1398, 0x1362),
        ChainDriverDecision::SkipDivergent,
        "BM1398 + BM1362 must not be co-driven by the latched driver"
    );
    assert_eq!(
        driver_for_chain(0x1362, 0x1398),
        ChainDriverDecision::SkipDivergent,
        "BM1362 + BM1398 must not be co-driven by the latched driver"
    );
    assert_eq!(
        driver_for_chain(0x1362, 0x1362),
        ChainDriverDecision::Drive,
        "homogeneous production IDs keep existing behavior"
    );
    assert_eq!(
        driver_for_chain(0, 0x1362),
        ChainDriverDecision::Drive,
        "zero latched ID keeps discovery/model-hint behavior"
    );
    assert_eq!(
        driver_for_chain(0x1362, 0),
        ChainDriverDecision::Drive,
        "zero chain ID keeps discovery/model-hint behavior"
    );
}

#[test]
fn xil25_policy_is_log_only_until_config_opt_in() {
    assert_eq!(
        driver_for_chain_with_policy(0x1398, 0x1362, DivergentChipPolicy::LogOnly),
        ChainDriverDecision::LogOnlyDivergent
    );

    assert!(
        CONFIG_RS.contains("pub enforce_mixed_chip_id_refusal_on_xil25: bool"),
        ".25 refusal opt-in must stay explicit in config"
    );
    assert!(
        CONFIG_RS.contains("enforce_mixed_chip_id_refusal_on_xil25: false"),
        ".25 refusal must default off"
    );
    assert!(
        !WAVE54_LAUNCHER.contains("enforce_mixed_chip_id_refusal_on_xil25"),
        "Wave-54 proven .25 launcher must not opt into mixed-chip refusal"
    );
    assert!(
        !XIL_OVERRIDE.contains("enforce_mixed_chip_id_refusal_on_xil25"),
        "XIL .25 override config must not opt into mixed-chip refusal"
    );
}

#[test]
fn daemon_wires_mixed_chip_policy_into_enumeration_and_phase7() {
    for needle in [
        "divergent_chip_policy_for_platform",
        "fingerprint_matches_xil_25",
        "mixed_chip_policy",
        "mixed_chip_id_refused",
        "mixed_chip_id_phase7_skip",
        "enforce_mixed_chip_id_refusal_on_xil25",
    ] {
        assert!(
            DAEMON_RS.contains(needle),
            "daemon.rs must keep HAL-2 mixed-chip wiring: missing {needle}"
        );
    }
}
