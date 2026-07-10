//!  (2026-05-22) — QA §10 CI for `psu_hardware_variant` serde shape.
//!
//! Source-parse pin for the new `Option<String>` field on
//! `config::PsuOverride`. Behavioral round-trip + skip-serializing-if-none
//! contract is tested inline in `config.rs::tests` (see
//! `psu_override_psu_hardware_variant_round_trip_loki` +
//! `psu_override_none_fields_skip_serialization_byte_identical`); this
//! file pins the source-text declaration so a regression that removes
//! the `#[serde(default, skip_serializing_if = "Option::is_none")]`
//! attribute is caught.
//!
//! Per QA §10.

const CONFIG_RS: &str = include_str!("../src/config.rs");

/// Windowed assertion that the serde attribute appears immediately before
/// the field declaration. Robust to LF/CRLF and reformatting.
fn assert_serde_attr_precedes_field(field_decl: &str, expected_attr_fragment: &str) {
    let field_pos = CONFIG_RS
        .find(field_decl)
        .unwrap_or_else(|| panic!("field declaration `{field_decl}` must exist"));
    let lookback_start = field_pos.saturating_sub(200);
    let window = &CONFIG_RS[lookback_start..field_pos];
    assert!(
        window.contains(expected_attr_fragment),
        "field `{field_decl}` must be preceded by serde attribute fragment \
         `{expected_attr_fragment}`. Window:\n{window}"
    );
}

#[test]
fn psu_hardware_variant_field_declaration_is_correct() {
    // The field must be `pub psu_hardware_variant: Option<String>` and
    // carry the `skip_serializing_if = "Option::is_none"` attribute so
    // existing units' TOML stays byte-identical when the operator never
    // sets it.
    assert!(
        CONFIG_RS.contains("pub psu_hardware_variant: Option<String>"),
        "psu_hardware_variant must be Option<String> (operator metadata only)"
    );
    assert_serde_attr_precedes_field(
        "pub psu_hardware_variant: Option<String>",
        "skip_serializing_if = \"Option::is_none\"",
    );
}

#[test]
fn no_smbus_peer_field_declaration_is_correct() {
    // Companion EE-LOKI-001 field — same serde contract.
    assert!(
        CONFIG_RS.contains("pub no_smbus_peer: Option<bool>"),
        "no_smbus_peer must be Option<bool> (None = legacy lenient probe)"
    );
    assert_serde_attr_precedes_field(
        "pub no_smbus_peer: Option<bool>",
        "skip_serializing_if = \"Option::is_none\"",
    );
}

#[test]
fn psu_override_struct_documents_wave23_fields() {
    // The new fields' doc-comments must cite EE-LOKI-001 /  so a
    // future reader can find the review trail.
    assert!(
        CONFIG_RS.contains("EE-LOKI-001"),
        "no_smbus_peer doc-comment must cite EE-LOKI-001 (the originating finding)"
    );
    assert!(
        CONFIG_RS.contains("Wave-23")
            || CONFIG_RS.contains("Wave 23")
            || CONFIG_RS.contains("2026-05-22"),
        "new fields must cite Wave-23 / 2026-05-22 provenance"
    );
}
