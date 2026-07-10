//!  (2026-05-22) — QA §10 CI for EE-LOKI-001 hard-skip wiring.
//!
//! Pins the contract:
//!
//!   - TOML `[power.psu_override].no_smbus_peer = true` round-trips
//!   - the hybrid path's Phase 0c branch hard-skips the lenient probe
//!     when `no_smbus_peer == Some(true)` — i.e., a runtime `if` guard
//!     references the field BEFORE the `bring_up_apw121215a_smart_lenient`
//!     call site
//!   - the skip branch sets `psu_arc = None; psu_hb_handle = None;`
//!     (the canonical "Loki-removed / genuinely-silent" fall-through state)
//!
//! ## Why this is a source-parse test
//!
//! The runtime Phase 0c branch lives deep inside
//! `S19jHybridMiner::run()` and can only be exercised end-to-end against
//! a live AM2 control board (which requires `/dev/i2c-0`, `/dev/uio*`,
//! and the Zynq PL bitstream). On Linux CI the source-parse approach is
//! the authoritative regression guard.
//!
//! Per QA §10 — closes the EE-LOKI-001 wiring contract.

const HYBRID_RS: &str = include_str!("../src/s19j_hybrid_mining.rs");

#[test]
fn phase_0c_branch_checks_no_smbus_peer_before_lenient_probe() {
    // The branch must inspect `ovr.no_smbus_peer == Some(true)` BEFORE
    // calling `bring_up_apw121215a_smart_lenient` — proving the hard-skip
    // is the first decision in the Phase-0 (b) branch (psu_override).
    //
    // Search only inside the `else if psu_override_active {` block (the
    // Phase-0 b branch). The lenient-probe HELPER itself is defined
    // earlier in the file at the module level (`fn bring_up_apw121215a_
    // smart_lenient(...)`); we need to compare against the CALL SITE,
    // not the definition.
    let phase_0b_block_start = HYBRID_RS
        .find("} else if psu_override_active {")
        .expect("Phase 0 (b) psu_override branch must exist");
    let phase_0b_block = &HYBRID_RS[phase_0b_block_start..];
    let lenient_call_pos = phase_0b_block
        .find("bring_up_apw121215a_smart_lenient(")
        .expect("the lenient probe CALL SITE must exist in Phase 0c");
    let no_smbus_check_pos = phase_0b_block
        .find("ovr.no_smbus_peer == Some(true)")
        .expect("Phase 0c must check `ovr.no_smbus_peer == Some(true)`");
    assert!(
        no_smbus_check_pos < lenient_call_pos,
        "no_smbus_peer hard-skip check ({no_smbus_check_pos}) must appear BEFORE \
         the lenient probe call site ({lenient_call_pos}) inside the Phase-0(b) block"
    );
}

#[test]
fn phase_0c_skip_branch_logs_canonical_message() {
    // The hard-skip log line MUST mention "no_smbus_peer=true" and
    // "hard-skipping" so post-incident forensics can identify the path
    // by grepping the log.
    assert!(
        HYBRID_RS.contains("no_smbus_peer=true") && HYBRID_RS.contains("hard-skipping"),
        "Phase 0c hard-skip must emit a log line containing 'no_smbus_peer=true' \
         AND 'hard-skipping' for operator forensics"
    );
}

#[test]
fn phase_0c_hard_skip_path_sets_psu_arc_none() {
    // The hard-skip branch must set both `psu_arc = None` and
    // `psu_hb_handle = None` — equivalent to the lenient-probe-deadline-
    // expired fall-through state. A regression that left psu_arc as
    // Some(...) would spawn a heartbeat thread targeting a bus the
    // operator has declared empty.
    //
    // Find the hard-skip block (the `if ovr.no_smbus_peer == Some(true) {`
    // branch) and assert its body contains both None assignments.
    let body = HYBRID_RS;
    let if_start = body
        .find("if ovr.no_smbus_peer == Some(true) {")
        .expect("hard-skip if-block must exist");
    let after_if = &body[if_start..];
    // The `} else {` closes the hard-skip arm and starts the lenient
    // probe arm.
    let else_pos = after_if
        .find("} else {")
        .expect("hard-skip must have an else arm");
    let skip_body = &after_if[..else_pos];
    assert!(
        skip_body.contains("psu_arc = None"),
        "hard-skip body must set psu_arc = None"
    );
    assert!(
        skip_body.contains("psu_hb_handle = None"),
        "hard-skip body must set psu_hb_handle = None"
    );
}

#[test]
fn phase_0c_logs_psu_hardware_variant() {
    //  log emission: the operator-declared psu_hardware_variant
    // must be logged once at Phase 0(b) entry so fleet logs can be
    // grepped for "bare-apw3" / "loki" / "(unset)".
    assert!(
        HYBRID_RS.contains("psu_hardware_variant"),
        "Phase 0(b) must log the operator-declared psu_hardware_variant"
    );
}

#[test]
fn phase_0c_zero_psu_bytes_env_hard_skips_lenient_branch() {
    // : Patch 8's zero-PSU-byte launcher must be enforceable
    // without relying on a live TOML edit to add no_smbus_peer=true.
    // Under psu_override, DCENT_AM2_ZERO_PSU_BYTES=1 must skip the
    // opportunistic smart-APW12/Loki gpio-bitbang path before the helper
    // that can emit APW12/Loki frames is called.
    assert!(
        HYBRID_RS.contains("fn am2_zero_psu_bytes_enabled()"),
        "hybrid runtime must expose a zero-PSU-byte env gate helper"
    );
    assert!(
        HYBRID_RS.contains("DCENT_AM2_ZERO_PSU_BYTES"),
        "source must name the zero-PSU-byte env gate for log/test greps"
    );

    let phase_0b_block_start = HYBRID_RS
        .find("} else if psu_override_active {")
        .expect("Phase 0 (b) psu_override branch must exist");
    let phase_0b_block = &HYBRID_RS[phase_0b_block_start..];
    let lenient_call_pos = phase_0b_block
        .find("bring_up_apw121215a_smart_lenient(")
        .expect("the lenient probe CALL SITE must exist in Phase 0c");
    let zero_gate_pos = phase_0b_block
        .find("else if am2_zero_psu_bytes_enabled()")
        .expect("zero-PSU-byte env gate must exist in Phase 0c");
    assert!(
        zero_gate_pos < lenient_call_pos,
        "zero-PSU-byte env gate ({zero_gate_pos}) must appear BEFORE \
         the lenient probe call site ({lenient_call_pos})"
    );

    let zero_gate_body = &phase_0b_block[zero_gate_pos..lenient_call_pos];
    assert!(
        zero_gate_body.contains("psu_arc = None"),
        "zero-PSU-byte hard-skip must leave psu_arc = None"
    );
    assert!(
        zero_gate_body.contains("psu_hb_handle = None"),
        "zero-PSU-byte hard-skip must not spawn a PSU heartbeat"
    );
    assert!(
        zero_gate_body.contains("zero PSU bytes") && zero_gate_body.contains("PWR_CONTROL-only"),
        "zero-PSU-byte hard-skip log must be grep-friendly for live forensics"
    );
}

#[test]
fn wave55n_phase0_diag_stop_hard_stops_before_dspic_enable() {
    let gate_pos = HYBRID_RS
        .find("if am2_diag_stop_after_psu_enabled()")
        .expect("Phase-0 diagnostic stop gate must exist");
    let phase1_pos = HYBRID_RS[gate_pos..]
        .find("Phase 1: PIC init")
        .expect("Phase 1 marker must exist after Phase-0 diagnostic stop");
    let gate_body = &HYBRID_RS[gate_pos..gate_pos + phase1_pos];

    for marker in [
        "DCENT_AM2_DIAG_STOP_AFTER_PSU=1",
        "force_am2_home_hard_stop(&self.config, \"diag-stop-after-psu\")",
        "self.shutdown.cancel()",
        "no dsPIC voltage enable",
        "no chain UART init",
        "no Stratum",
        "no work dispatch",
    ] {
        assert!(
            gate_body.contains(marker),
            "Phase-0 diagnostic stop must keep marker `{}`",
            marker
        );
    }
}

#[test]
fn wave55n_dspic_diag_stop_before_chain_uart_probe_and_mining() {
    let gate_pos = HYBRID_RS
        .find("if am2_diag_stop_after_dspic_enable_enabled()")
        .expect("dsPIC-enable diagnostic stop gate must exist");
    let chain_probe_pos = HYBRID_RS[gate_pos..]
        .find("post_enable_chain_uart_probe(")
        .expect("post-enable chain UART probe must remain after diagnostic stop gate");
    let gate_body = &HYBRID_RS[gate_pos..gate_pos + chain_probe_pos];

    for marker in [
        "DCENT_AM2_DIAG_STOP_AFTER_DSPIC_ENABLE=1",
        "active_dspic_addrs(active_chains)",
        "disable_addrs",
        "Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint)",
        "force_am2_home_hard_stop(&self.config, \"diag-stop-after-dspic-enable\")",
        "no chain UART probe",
        "no BM1362 init",
        "no Stratum",
        "no work dispatch",
    ] {
        assert!(
            gate_body.contains(marker),
            "dsPIC-enable diagnostic stop must keep marker `{}`",
            marker
        );
    }
}

#[test]
fn wave55o_bm1362_enum_diag_stop_before_stratum_and_work() {
    let enum_log_pos = HYBRID_RS
        .find("BM1362 GetAddress @{}baud")
        .expect("BM1362 GetAddress enumeration log must exist");
    let gate_offset = HYBRID_RS[enum_log_pos..]
        .find("if am2_diag_stop_after_bm1362_enum_enabled()")
        .expect("BM1362-enum diagnostic stop gate must exist after enumeration");
    let gate_pos = enum_log_pos + gate_offset;
    let stratum_pos = HYBRID_RS[gate_pos..]
        .find("Phase 10a: Connecting to pool")
        .expect("Stratum connection marker must remain after BM1362-enum stop gate");
    let gate_body = &HYBRID_RS[gate_pos..gate_pos + stratum_pos];

    for marker in [
        "DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM=1",
        "active_dspic_addrs(active_chains)",
        "unique_chip_replies = init_unique_count",
        "expected_chip_count = chip_count",
        "disable_addrs",
        "Pic0x89Service::new_with_fw(service.clone(), addr, fw_hint)",
        "force_am2_home_hard_stop(&self.config, \"diag-stop-after-bm1362-enum\")",
        "no Stratum",
        "no work dispatch",
        "no shares",
    ] {
        assert!(
            gate_body.contains(marker),
            "BM1362-enum diagnostic stop must keep marker `{}`",
            marker
        );
    }
}

#[test]
fn wave55p_bm1362_enum_diag_failure_path_still_logs_no_mining_stop() {
    let failure_pos = HYBRID_RS
        .find("Phase 4-7 failed after power-up")
        .expect("BM1362 init failure branch must exist");
    let hard_stop_pos = HYBRID_RS[failure_pos..]
        .find("force_am2_home_hard_stop(&self.config, \"bm1362-init-failed\")")
        .expect("BM1362 init failure branch must force home hard-stop");
    let failure_body = &HYBRID_RS[failure_pos..failure_pos + hard_stop_pos];

    for marker in [
        "DCENT_AM2_DIAG_STOP_AFTER_BM1362_ENUM=1",
        "BM1362 init failed before successful GetAddress enumeration",
        "effective_chain_uart_device",
        "active_dspic_addrs(active_chains)",
        "disable_dspic_addrs_best_effort",
        "no Stratum",
        "no work dispatch",
        "no shares",
    ] {
        assert!(
            failure_body.contains(marker),
            "BM1362-enum failure diagnostic path must keep marker `{}`",
            marker
        );
    }
}
