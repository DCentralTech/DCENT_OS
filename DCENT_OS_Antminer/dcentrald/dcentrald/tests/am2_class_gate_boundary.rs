//! AM2-Zynq/BM1362 class-gate boundary pins.
//!
//! The `a lab unit` S19j Pro XIL standalone path is live-proven and must not be
//! widened accidentally. Sibling AM2-Zynq/BM1362 units may reach the recipe
//! only through explicit default-off class + per-unit proof gates.

const HYBRID_RS: &str = include_str!("../src/s19j_hybrid_mining.rs");

#[path = "../src/wave55a_recipe_guard.rs"]
mod guard;

use guard::{
    am2_zynq_bm1362_class_matches, fingerprint_matches_xil_109, fingerprint_matches_xil_25,
    AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV, XIL_109_FINGERPRINT_OVERRIDE_ENV,
};

fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: std::collections::HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name: &str| owned.get(name).cloned()
}

#[test]
fn xil25_fingerprint_boundary_remains_narrow() {
    assert!(fingerprint_matches_xil_25(
        "zynq-bm3-am2",
        "am2-xil",
        Some("loki")
    ));
    assert!(!fingerprint_matches_xil_25(
        "zynq-bm3-am2",
        "am2-s19jpro",
        Some("loki")
    ));
    assert!(!fingerprint_matches_xil_25(
        "amlogic-a113d",
        "am3-aml-s21",
        None
    ));
    assert!(!fingerprint_matches_xil_25(
        "am335x-bb",
        "am3-bb-s19jpro",
        None
    ));
    assert!(!fingerprint_matches_xil_25(
        "cvitek-cv1835",
        "cv1835-s19jpro",
        None
    ));
    assert!(!fingerprint_matches_xil_25(
        "zynq-bm3-am2",
        "am2-s19pro",
        None
    ));
    assert!(!fingerprint_matches_xil_25("zynq-bm1-s9", "am1-s9", None));
}

#[test]
fn am2_zynq_bm1362_class_matches_only_am2_zynq_bm1362() {
    assert!(am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-s19jpro",
        None
    ));
    assert!(am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-s19j",
        Some("BM1362")
    ));
    assert!(am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-xil",
        None
    ));

    assert!(!am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-s19jpro",
        Some("BM1398")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-s19jpro",
        Some("BM1368")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "zynq-bm3-am2",
        "am2-s19pro",
        Some("BM1398")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "amlogic-a113d",
        "am3-aml-s21",
        Some("BM1368")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "am335x-bb",
        "am3-bb-s19jpro",
        Some("BM1362")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "cvitek-cv1835",
        "cv1835-s19jpro",
        Some("BM1362")
    ));
    assert!(!am2_zynq_bm1362_class_matches(
        "zynq-bm1-s9",
        "am1-s9",
        Some("BM1387")
    ));
    assert!(!am2_zynq_bm1362_class_matches("", "", None));
}

#[test]
fn xil109_fingerprint_requires_explicit_per_unit_proof() {
    assert!(!fingerprint_matches_xil_109(
        "zynq-bm3-am2",
        "am2-s19jpro",
        Some("BM1362"),
        env_from(&[])
    ));
    assert!(fingerprint_matches_xil_109(
        "zynq-bm3-am2",
        "am2-s19jpro",
        Some("BM1362"),
        env_from(&[(XIL_109_FINGERPRINT_OVERRIDE_ENV, "1")])
    ));
    assert!(!fingerprint_matches_xil_109(
        "amlogic-a113d",
        "am3-aml-s21",
        Some("BM1368"),
        env_from(&[(XIL_109_FINGERPRINT_OVERRIDE_ENV, "1")])
    ));
    assert!(!fingerprint_matches_xil_109(
        "zynq-bm3-am2",
        "am2-s19pro",
        Some("BM1398"),
        env_from(&[(XIL_109_FINGERPRINT_OVERRIDE_ENV, "1")])
    ));
}

#[test]
fn hybrid_recipe_gate_is_default_off_and_class_scoped() {
    let gate_start = HYBRID_RS
        .find("fn am2_zynq_bm1362_recipe_gate_matches")
        .expect("hybrid recipe gate helper exists");
    let gate = &HYBRID_RS[gate_start..gate_start + 700];
    assert!(gate.contains("am2_xil_25_fingerprint_matches()"));
    assert!(gate.contains("AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV"));
    assert!(gate.contains("AM2_XIL109_FINGERPRINT_OVERRIDE_ENV"));
    assert!(gate.contains("am2_xil_109_fingerprint_matches()"));
    assert!(gate.contains("am2_zynq_bm1362_class_matches()"));

    assert!(HYBRID_RS.contains(AM2_ZYNQ_BM1362_CLASS_RECIPE_ENV));
    assert!(HYBRID_RS.contains(XIL_109_FINGERPRINT_OVERRIDE_ENV));
    assert!(!HYBRID_RS.contains("am3-aml-s21") || !gate.contains("am3-aml-s21"));
}

#[test]
fn xil109_gate_documents_external_per_unit_proof_contract() {
    let helper_start = HYBRID_RS
        .find("fn am2_xil_109_fingerprint_matches")
        .expect("xil109 helper exists");
    let helper = &HYBRID_RS[helper_start..helper_start + 700];

    assert!(helper.contains("fingerprint_matches_xil_109"));
    assert!(helper.contains("AM2_XIL109_FINGERPRINT_OVERRIDE_ENV"));
    assert!(!helper.contains("203.0.113.109"));
}

#[test]
fn default_off_bm1362_recipe_gates_use_class_recipe_wrapper() {
    for fn_name in [
        "am2_bm1362_re018_cold_sequence_enabled",
        "am2_free_chain1_work_tx_irq_for_kernel_uart",
        "am2_open_both_uarts_before_enum_enabled",
        "am2_fpga_uart_relay_cold_enabled",
        "am2_board_control_bit8_enabled",
        "am2_re018_write_ticket_hashcount_enabled",
        "am2_re018_full_core_init_enabled",
        "am2_dspic_cold_warmup_exclusive_enabled",
    ] {
        let start = HYBRID_RS.find(fn_name).expect(fn_name);
        let window = &HYBRID_RS[start..(start + 900).min(HYBRID_RS.len())];
        assert!(
            window.contains("am2_zynq_bm1362_recipe_gate_matches()"),
            "{fn_name} must use the default-off AM2-Zynq/BM1362 recipe wrapper"
        );
    }
}
