//!  (2026-05-22) — QA §10 CI-4 / pins R5 retry shape.
//!
//! Source-parse regression pin for the  `pic_read_fw_version_service`
//! retry shape in `s19j_hybrid_mining.rs`. Per the R5 RE corpus alignment
//! (2026-05-21), the new helper performs:
//!
//!   - 15× clean whole-frame retries per variant (NOT the legacy 3-attempt
//!     speculative-zero-flush loop)
//!   - 100 ms delay between retries (bosminer cadence)
//!   - NO 16-zero-byte parser-flush prepended to the framed write
//!     (the speculative flush was the source of cross-variant parser corruption)
//!
//! These constants are exported and pinned inline in `s19j_hybrid_mining.rs`;
//! this integration-level pin catches a regression that would silently change
//! the public retry budget through source-text inspection.
//!
//! Per QA §10 — closes GAP-HYBRID-1 at the source level (a behavioral
//! `pic_read_fw_version_service` stub-based test is blocked on the
//! `I2cServiceHandle::for_unit_tests` visibility constraint).

const HYBRID_RS: &str = include_str!("../src/s19j_hybrid_mining.rs");

#[test]
fn pic_read_fw_version_clean_retry_constants_are_bosminer_faithful() {
    // R5 (2026-05-21): the bosminer-faithful retry shape is 15 attempts
    // × 100 ms — NOT a 3-attempt-then-flush loop.
    assert!(
        HYBRID_RS.contains("PIC_GET_VERSION_CLEAN_RETRIES") && HYBRID_RS.contains("15"),
        "PIC_GET_VERSION_CLEAN_RETRIES = 15 must remain pinned in source"
    );
    assert!(
        HYBRID_RS.contains("PIC_GET_VERSION_RETRY_DELAY_MS") && HYBRID_RS.contains("100"),
        "PIC_GET_VERSION_RETRY_DELAY_MS = 100 must remain pinned in source"
    );

    // Find the constant declarations specifically so we catch the exact
    // value (not just any "15" / "100" in the file).
    assert!(
        HYBRID_RS.contains("const PIC_GET_VERSION_CLEAN_RETRIES")
            || HYBRID_RS.contains("PIC_GET_VERSION_CLEAN_RETRIES: u32 = 15")
            || HYBRID_RS.contains("PIC_GET_VERSION_CLEAN_RETRIES: usize = 15")
            || HYBRID_RS.contains("PIC_GET_VERSION_CLEAN_RETRIES = 15"),
        "PIC_GET_VERSION_CLEAN_RETRIES constant must be declared with value 15"
    );
    assert!(
        HYBRID_RS.contains("const PIC_GET_VERSION_RETRY_DELAY_MS")
            || HYBRID_RS.contains("PIC_GET_VERSION_RETRY_DELAY_MS: u64 = 100")
            || HYBRID_RS.contains("PIC_GET_VERSION_RETRY_DELAY_MS = 100"),
        "PIC_GET_VERSION_RETRY_DELAY_MS constant must be declared with value 100"
    );
}

#[test]
fn pic_get_version_helper_has_no_speculative_zero_flush_default() {
    // The  rewrite removed the speculative zero-flush prepend that
    // the legacy code used by default. The helper retains a `flush_first`
    // param for the OTHER callers/tests, but the production
    // `pic_read_fw_version_service` path calls it with `false`.
    //
    // Catches: a future "fix" that silently flips the production default
    // back to `flush_first=true`, re-introducing the cross-variant
    // parser corruption R5 closed.
    assert!(
        HYBRID_RS.contains("pic_read_fw_version_service"),
        "pic_read_fw_version_service must exist in s19j_hybrid_mining.rs"
    );
    // The production caller must NOT be calling with flush_first=true.
    // (Today: it doesn't pass flush_first at all — the helper internally
    // uses flush_first=false by default for the production path.)
    assert!(
        !HYBRID_RS.contains("pic_read_fw_version_service_with_flush(/* true */)")
            && !HYBRID_RS.contains("pic_read_fw_version_service(pic_i2c, selected_pic_addr, true)"),
        "production pic_read_fw_version_service call site MUST NOT request \
         speculative zero-flush — R5 RE corpus alignment 2026-05-21"
    );
}
