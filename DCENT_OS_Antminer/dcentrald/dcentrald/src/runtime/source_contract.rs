//! Cross-platform helpers for source-level architecture contracts.
//!
//! A small number of hardware-lifecycle invariants cannot yet be exercised
//! without physical miners, so their regression tests inspect Rust fixtures.
//! Checkout line endings and `rustfmt` wrapping are not part of those
//! invariants and must not make the tests platform-dependent.

/// Return source text with formatting-only whitespace removed.
///
/// Contract patterns should retain Rust punctuation and complete identifiers;
/// this helper is for layout independence, not for broad keyword searches.
pub(crate) fn compact_rust_source(source: &str) -> String {
    source.split_whitespace().collect()
}

#[cfg(test)]
mod tests {
    use super::compact_rust_source;

    #[test]
    fn compaction_is_independent_of_line_endings_and_rustfmt_wrapping() {
        let lf = "owner.push(\n    \"worker\",\n    handle,\n);\n";
        let crlf = "owner\r\n    .push(\r\n        \"worker\", handle,\r\n    );\r\n";

        assert_eq!(compact_rust_source(lf), compact_rust_source(crlf));
        assert_eq!(compact_rust_source(lf), "owner.push(\"worker\",handle,);");
    }
}
