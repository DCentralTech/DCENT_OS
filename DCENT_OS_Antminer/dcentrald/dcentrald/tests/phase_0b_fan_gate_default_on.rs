//!  (2026-05-22) — QA §10 CI-6.
//!
//! Source-parse regression pin: the  `am2_fan_gate_before_pic`
//! config knob defaults to `true`. A regression that silently flipped
//! this default to `false` would revert the bosminer-faithful Phase 0b
//! ordering (fan gate BEFORE PIC GET_VERSION) and re-introduce the
//! "reverse vs bosminer" ordering documented at `config.rs:1126-1129`.
//!
//! Per QA §10 — closes a slice of GAP-HYBRID-3 at the source level.

const CONFIG_RS: &str = include_str!("../src/config.rs");

#[test]
fn am2_fan_gate_before_pic_default_is_true() {
    // The field declaration must carry `#[serde(default = "default_true")]`
    // somewhere immediately before `pub am2_fan_gate_before_pic: bool`.
    // Use a regex-equivalent windowed check that's robust to CRLF/LF and
    // attribute reformatting.
    let field_pos = CONFIG_RS
        .find("pub am2_fan_gate_before_pic: bool")
        .expect("am2_fan_gate_before_pic field must exist");
    // Look back ~200 chars for the serde default attribute.
    let lookback_start = field_pos.saturating_sub(200);
    let window = &CONFIG_RS[lookback_start..field_pos];
    assert!(
        window.contains("default_true"),
        "am2_fan_gate_before_pic must use the `default_true` serde helper \
         (defaults to true). Window before field:\n{window}"
    );
}

#[test]
fn am2_fan_gate_before_pic_default_in_struct_default_impl() {
    // The programmatic `MiningConfig::default()` impl must also produce
    // `true` for this field — the inline test
    // `xil25_consolidated_fix_knobs_default_via_struct_default` in
    // config.rs already pins this behaviorally; this test pins the source
    // declaration so a refactor that changes one but not the other is
    // caught here.
    assert!(
        CONFIG_RS.contains("am2_fan_gate_before_pic: true"),
        "MiningConfig::default() must initialize am2_fan_gate_before_pic to true"
    );
}
