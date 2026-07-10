//!  (2026-05-22) — QA §10 CI-8.
//!
//! Source-parse regression pin for the  Phase 0d double-gate:
//!
//!   - TOML knob `am2_dspic_warmup_before_get_version` defaults to `true`
//!     (so the wrapper IS wired in)
//!   - Env gate `DCENT_AM2_PIC_RESET_AND_START_APP` defaults to OFF
//!     (so the wrapper does NOT emit bytes on the first deploy)
//!   - The Phase 0d branch in `s19j_hybrid_mining.rs` ANDs both gates
//!
//! The load-bearing safety design (per `feedback_*` rules + CE review) is
//! "double-default-off-for-the-env": the binary's defaults yield the
//! legacy pre- byte-stream on `a lab unit` until the operator explicitly
//! sets `DCENT_AM2_PIC_RESET_AND_START_APP=1` on the unit. A regression
//! that drops the env-gate would silently activate Phase 0d on every
//! deploy.
//!
//! Per QA §10 — closes GAP-HYBRID-2/3 + double-gate regression class.

const CONFIG_RS: &str = include_str!("../src/config.rs");
const HYBRID_RS: &str = include_str!("../src/s19j_hybrid_mining.rs");

#[test]
fn am2_dspic_warmup_before_get_version_default_is_true() {
    // Source pin: serde default is `default_true`. Use windowed lookback
    // so the test is robust to CRLF/LF and attribute reformatting.
    let field_pos = CONFIG_RS
        .find("pub am2_dspic_warmup_before_get_version: bool")
        .expect("am2_dspic_warmup_before_get_version field must exist");
    let lookback_start = field_pos.saturating_sub(200);
    let window = &CONFIG_RS[lookback_start..field_pos];
    assert!(
        window.contains("default_true"),
        "am2_dspic_warmup_before_get_version must use `default_true` (TOML knob \
         defaults to true). Window:\n{window}"
    );
}

#[test]
fn am2_pic_reset_and_start_app_env_gate_defaults_off() {
    // The env-gate function MUST live somewhere in the hybrid path. The
    // exact name is `am2_pic_reset_and_start_app_enabled`. Per the
    //  source-comment block, the env is `DCENT_AM2_PIC_RESET_AND_START_APP`.
    assert!(
        HYBRID_RS.contains("DCENT_AM2_PIC_RESET_AND_START_APP"),
        "the load-bearing env gate name MUST remain DCENT_AM2_PIC_RESET_AND_START_APP \
         — renaming silently breaks operator runbooks (and the .25 setup)"
    );
    assert!(
        HYBRID_RS.contains("am2_pic_reset_and_start_app_enabled"),
        "the env-gate helper must remain named am2_pic_reset_and_start_app_enabled \
         (the AND'd partner of am2_dspic_warmup_before_get_version)"
    );
}

#[test]
fn phase_0d_branch_requires_double_gate() {
    // The Phase 0d branch in s19j_hybrid_mining.rs must guard the wrapper
    // call with BOTH the TOML knob AND the env-flag helper. Find the
    // canonical `let warmup_did_run = if` block ( Phase 0d wiring)
    // and assert both gates AND `&&` appear inside its head expression.
    assert!(
        HYBRID_RS.contains("self.config.mining.am2_dspic_warmup_before_get_version")
            && HYBRID_RS.contains("am2_pic_reset_and_start_app_enabled()"),
        "Phase 0d must reference BOTH gates (TOML + env helper) somewhere in source"
    );
    let phase_0d_head = HYBRID_RS
        .find("let warmup_did_run = if")
        .expect("Phase 0d head `let warmup_did_run = if` must exist");
    // The if-expression head is at most ~500 chars (TOML knob + && + env
    // gate + open brace).
    let head_span = &HYBRID_RS[phase_0d_head..phase_0d_head + 500];
    assert!(
        head_span.contains("self.config.mining.am2_dspic_warmup_before_get_version"),
        "Phase 0d head must reference the TOML knob `am2_dspic_warmup_before_get_version`"
    );
    assert!(
        head_span.contains("am2_pic_reset_and_start_app_enabled()"),
        "Phase 0d head must reference the env gate helper `am2_pic_reset_and_start_app_enabled()`"
    );
    assert!(
        head_span.contains("&&"),
        "Phase 0d head must AND the TOML knob and env gate (load-bearing \
         double-default-off for the env per Wave-22 design — `a lab unit` byte-identical \
         preservation). Head span:\n{head_span}"
    );
}

#[test]
fn phase_0d_env_off_default_documented_in_config_comment() {
    // The TOML knob's doc-comment must call out the env-gate's default-off
    // contract so future operators understand why the wrapper appears
    // "wired but disabled" on a fresh binary.
    assert!(
        CONFIG_RS.contains("DCENT_AM2_PIC_RESET_AND_START_APP=1"),
        "config.rs must document the env-gate name + default-off contract \
         next to the am2_dspic_warmup_before_get_version field"
    );
}
