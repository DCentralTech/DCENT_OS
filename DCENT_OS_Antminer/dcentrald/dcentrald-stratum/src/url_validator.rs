//! Strict Stratum pool URL validation.
//!
//! LuxOS accepted malformed pool URLs and failed later at TCP connect time.
//! DCENT_OS validates operator-written pool config before mining starts.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StratumUrlKind {
    V1,
    V2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedStratumUrl {
    pub kind: StratumUrlKind,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StratumUrlError {
    #[error("URL is empty")]
    Empty,

    #[error("URL must not contain leading or trailing whitespace")]
    OuterWhitespace,

    #[error("URL contains embedded whitespace")]
    EmbeddedWhitespace,

    #[error("unsupported scheme; use {expected}")]
    UnsupportedScheme { expected: &'static str },

    #[error("URL must not embed credentials")]
    Credentials,

    #[error("URL must not contain a path, query string, or fragment")]
    ExtraComponents,

    #[error("host is empty")]
    EmptyHost,

    #[error("host is invalid")]
    InvalidHost,

    #[error("port is missing")]
    MissingPort,

    #[error("port is invalid")]
    InvalidPort,
}

pub fn validate_v1_pool_url(url: &str) -> Result<ValidatedStratumUrl, StratumUrlError> {
    validate_pool_url_with_schemes(
        url,
        &["stratum+tcp://", "stratum+tls://", "stratum+ssl://"],
        StratumUrlKind::V1,
    )
}

pub fn validate_sv2_pool_url(url: &str) -> Result<ValidatedStratumUrl, StratumUrlError> {
    validate_pool_url_with_schemes(url, &["stratum2+tcp://", "sv2+tcp://"], StratumUrlKind::V2)
}

fn validate_pool_url_with_schemes(
    url: &str,
    schemes: &[&'static str],
    kind: StratumUrlKind,
) -> Result<ValidatedStratumUrl, StratumUrlError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(StratumUrlError::Empty);
    }
    if trimmed != url {
        return Err(StratumUrlError::OuterWhitespace);
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(StratumUrlError::EmbeddedWhitespace);
    }

    let Some((scheme, authority)) = schemes
        .iter()
        .find_map(|scheme| trimmed.strip_prefix(scheme).map(|rest| (*scheme, rest)))
    else {
        return Err(StratumUrlError::UnsupportedScheme {
            expected: match kind {
                StratumUrlKind::V1 => {
                    "stratum+tcp://host:port, stratum+tls://host:port, or stratum+ssl://host:port"
                }
                StratumUrlKind::V2 => "stratum2+tcp://host:port or sv2+tcp://host:port",
            },
        });
    };

    let _ = scheme;
    if authority.contains('@') {
        return Err(StratumUrlError::Credentials);
    }
    if authority.contains('/') || authority.contains('?') || authority.contains('#') {
        return Err(StratumUrlError::ExtraComponents);
    }

    let (host, port_text) = split_host_port(authority)?;
    validate_host(&host)?;
    let port = parse_port(port_text)?;

    Ok(ValidatedStratumUrl { kind, host, port })
}

fn split_host_port(authority: &str) -> Result<(String, &str), StratumUrlError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let Some((host, suffix)) = rest.split_once(']') else {
            return Err(StratumUrlError::InvalidHost);
        };
        let Some(port_text) = suffix.strip_prefix(':') else {
            return Err(StratumUrlError::MissingPort);
        };
        return Ok((host.to_string(), port_text));
    }

    let Some((host, port_text)) = authority.rsplit_once(':') else {
        return Err(StratumUrlError::MissingPort);
    };
    if host.contains(':') {
        return Err(StratumUrlError::InvalidHost);
    }
    Ok((host.to_string(), port_text))
}

fn validate_host(host: &str) -> Result<(), StratumUrlError> {
    if host.is_empty() {
        return Err(StratumUrlError::EmptyHost);
    }

    if host.contains(':') {
        return validate_ipv6_literal(host);
    }

    for label in host.split('.') {
        if label.is_empty() {
            return Err(StratumUrlError::InvalidHost);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(StratumUrlError::InvalidHost);
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(StratumUrlError::InvalidHost);
        }
    }

    Ok(())
}

fn validate_ipv6_literal(host: &str) -> Result<(), StratumUrlError> {
    if host
        .chars()
        .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
    {
        Ok(())
    } else {
        Err(StratumUrlError::InvalidHost)
    }
}

fn parse_port(port_text: &str) -> Result<u16, StratumUrlError> {
    if port_text.is_empty() {
        return Err(StratumUrlError::MissingPort);
    }
    let port = port_text
        .parse::<u16>()
        .map_err(|_| StratumUrlError::InvalidPort)?;
    if port == 0 {
        return Err(StratumUrlError::InvalidPort);
    }
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_accepts_strict_stratum_tcp_url() {
        let url = validate_v1_pool_url("stratum+tcp://solo.ckpool.org:3333").unwrap();
        assert_eq!(url.kind, StratumUrlKind::V1);
        assert_eq!(url.host, "solo.ckpool.org");
        assert_eq!(url.port, 3333);
    }

    #[test]
    fn v1_accepts_strict_tls_url_schemes() {
        for scheme in ["stratum+tls://", "stratum+ssl://"] {
            let url = validate_v1_pool_url(&format!("{scheme}pool.example.com:443")).unwrap();
            assert_eq!(url.kind, StratumUrlKind::V1);
            assert_eq!(url.host, "pool.example.com");
            assert_eq!(url.port, 443);
        }
    }

    #[test]
    fn v1_rejects_late_failure_forms() {
        for bad in [
            "pool.example.com:3333",
            "tcp://pool.example.com:3333",
            "stratum+tcp://pool.example.com",
            "stratum+tcp://pool.example.com:0",
            "stratum+tcp://user:pass@pool.example.com:3333",
            "stratum+tcp://pool.example.com:3333/path",
            "stratum+tcp://bad host:3333",
            " stratum+tcp://pool.example.com:3333",
        ] {
            assert!(validate_v1_pool_url(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn sv2_accepts_strict_sv2_tcp_url_schemes() {
        let stratum2 = validate_sv2_pool_url("stratum2+tcp://v2.pool.example.com:3336").unwrap();
        assert_eq!(stratum2.kind, StratumUrlKind::V2);
        assert_eq!(stratum2.host, "v2.pool.example.com");
        assert_eq!(stratum2.port, 3336);

        let sv2 = validate_sv2_pool_url("sv2+tcp://[2001:db8::1]:34255").unwrap();
        assert_eq!(sv2.host, "2001:db8::1");
        assert_eq!(sv2.port, 34255);
    }

    #[test]
    fn sv2_rejects_v1_or_default_port_shortcuts() {
        for bad in [
            "stratum+tcp://pool.example.com:3333",
            "sv2://pool.example.com",
            "stratum2+tcp://pool.example.com",
        ] {
            assert!(validate_sv2_pool_url(bad).is_err(), "{bad} should fail");
        }
    }

    // -----------------------------------------------------------------------
    // Per-variant error pinning.
    //
    // The existing tests assert "this URL fails" via `is_err()` but don't
    // verify which StratumUrlError variant fires. Pin each one so a future
    // refactor (e.g. switching to a real URL crate) cannot silently change
    // which classification the operator sees in their config-validation
    // error message.
    // -----------------------------------------------------------------------

    #[test]
    fn validate_returns_empty_for_blank_input() {
        // Empty string AND whitespace-only must surface Empty (whitespace
        // gets trimmed first, then the trim is checked for emptiness).
        assert_eq!(validate_v1_pool_url(""), Err(StratumUrlError::Empty));
        assert_eq!(validate_v1_pool_url("   "), Err(StratumUrlError::Empty));
        assert_eq!(validate_v1_pool_url("\t\n  "), Err(StratumUrlError::Empty));
        assert_eq!(validate_sv2_pool_url(""), Err(StratumUrlError::Empty));
    }

    #[test]
    fn validate_returns_outer_whitespace_for_padded_input() {
        // Leading/trailing whitespace around an otherwise-valid URL must
        // produce OuterWhitespace, NOT Empty (the trimmed value is non-empty).
        assert_eq!(
            validate_v1_pool_url(" stratum+tcp://pool.example.com:3333"),
            Err(StratumUrlError::OuterWhitespace)
        );
        assert_eq!(
            validate_v1_pool_url("stratum+tcp://pool.example.com:3333\n"),
            Err(StratumUrlError::OuterWhitespace)
        );
        assert_eq!(
            validate_v1_pool_url("\tstratum+tcp://pool.example.com:3333\t"),
            Err(StratumUrlError::OuterWhitespace)
        );
    }

    #[test]
    fn validate_returns_embedded_whitespace_for_internal_spaces() {
        // The host or port containing internal whitespace must surface
        // EmbeddedWhitespace, not InvalidHost / InvalidPort. Operators
        // benefit from the more specific error message.
        assert_eq!(
            validate_v1_pool_url("stratum+tcp://pool.example.com :3333"),
            Err(StratumUrlError::EmbeddedWhitespace)
        );
        assert_eq!(
            validate_v1_pool_url("stratum+tcp://pool example.com:3333"),
            Err(StratumUrlError::EmbeddedWhitespace)
        );
    }

    #[test]
    fn validate_returns_unsupported_scheme_for_v1_with_v2_url() {
        // V1 validator must reject V2 schemes and report what's expected.
        let result = validate_v1_pool_url("stratum2+tcp://pool.example.com:3336");
        match result {
            Err(StratumUrlError::UnsupportedScheme { expected }) => {
                assert!(expected.contains("stratum+tcp://"));
            }
            other => panic!("expected UnsupportedScheme, got {:?}", other),
        }
    }

    #[test]
    fn validate_returns_unsupported_scheme_for_v2_with_v1_url() {
        let result = validate_sv2_pool_url("stratum+tcp://pool.example.com:3333");
        match result {
            Err(StratumUrlError::UnsupportedScheme { expected }) => {
                assert!(expected.contains("stratum2+tcp://") || expected.contains("sv2+tcp://"));
            }
            other => panic!("expected UnsupportedScheme, got {:?}", other),
        }
    }

    #[test]
    fn validate_returns_unsupported_scheme_for_https_or_other() {
        // Non-stratum schemes must be rejected with UnsupportedScheme.
        for bad in [
            "https://pool.example.com:3333",
            "ssh://pool.example.com:3333",
            "ftp://pool.example.com:3333",
        ] {
            let result = validate_v1_pool_url(bad);
            assert!(
                matches!(result, Err(StratumUrlError::UnsupportedScheme { .. })),
                "{bad} must surface UnsupportedScheme, got {:?}",
                result
            );
        }
    }

    #[test]
    fn validate_returns_credentials_for_userinfo() {
        // A URL embedding user:pass@host must surface Credentials so a
        // misconfigured pool URL doesn't silently leak credentials in the
        // log line that would otherwise warn about the URL.
        let result = validate_v1_pool_url("stratum+tcp://user:pass@pool.example.com:3333");
        assert_eq!(result, Err(StratumUrlError::Credentials));

        let user_only = validate_v1_pool_url("stratum+tcp://user@pool.example.com:3333");
        assert_eq!(user_only, Err(StratumUrlError::Credentials));
    }

    #[test]
    fn validate_returns_extra_components_for_path_query_fragment() {
        let path = validate_v1_pool_url("stratum+tcp://pool.example.com:3333/path");
        assert_eq!(path, Err(StratumUrlError::ExtraComponents));

        let trailing_slash = validate_v1_pool_url("stratum+tcp://pool.example.com:3333/");
        assert_eq!(trailing_slash, Err(StratumUrlError::ExtraComponents));

        let query = validate_v1_pool_url("stratum+tcp://pool.example.com:3333?worker=foo");
        assert_eq!(query, Err(StratumUrlError::ExtraComponents));

        let fragment = validate_v1_pool_url("stratum+tcp://pool.example.com:3333#frag");
        assert_eq!(fragment, Err(StratumUrlError::ExtraComponents));
    }

    #[test]
    fn validate_returns_empty_host_for_missing_host() {
        let result = validate_v1_pool_url("stratum+tcp://:3333");
        assert_eq!(result, Err(StratumUrlError::EmptyHost));
    }

    #[test]
    fn validate_returns_invalid_host_for_malformed_dns() {
        // Hyphen at start/end of a label is invalid per DNS rules.
        let leading = validate_v1_pool_url("stratum+tcp://-pool.example.com:3333");
        assert_eq!(leading, Err(StratumUrlError::InvalidHost));

        let trailing = validate_v1_pool_url("stratum+tcp://pool-.example.com:3333");
        assert_eq!(trailing, Err(StratumUrlError::InvalidHost));

        // Empty label (consecutive dots).
        let empty_label = validate_v1_pool_url("stratum+tcp://pool..example.com:3333");
        assert_eq!(empty_label, Err(StratumUrlError::InvalidHost));

        // Non-alphanumeric character in label.
        let underscore = validate_v1_pool_url("stratum+tcp://pool_x.example.com:3333");
        assert_eq!(underscore, Err(StratumUrlError::InvalidHost));
    }

    #[test]
    fn validate_returns_invalid_host_for_malformed_ipv6() {
        // Bracketed but with illegal characters.
        let bad = validate_v1_pool_url("stratum+tcp://[2001:db8::xxxx]:3333");
        assert_eq!(bad, Err(StratumUrlError::InvalidHost));

        // Bracketed but no closing bracket.
        let unclosed = validate_v1_pool_url("stratum+tcp://[2001:db8::1:3333");
        assert_eq!(unclosed, Err(StratumUrlError::InvalidHost));

        // Unbracketed IPv6 (multiple colons in host without brackets) — caught
        // by `host.contains(':')` after rsplit_once.
        let unbracketed = validate_v1_pool_url("stratum+tcp://2001:db8::1:3333:4444");
        assert!(matches!(
            unbracketed,
            Err(StratumUrlError::InvalidHost) | Err(StratumUrlError::MissingPort)
        ));
    }

    #[test]
    fn validate_returns_missing_port_when_no_colon_present() {
        let result = validate_v1_pool_url("stratum+tcp://pool.example.com");
        assert_eq!(result, Err(StratumUrlError::MissingPort));
    }

    #[test]
    fn validate_returns_missing_port_when_bracket_has_no_port() {
        let result = validate_v1_pool_url("stratum+tcp://[2001:db8::1]");
        assert_eq!(result, Err(StratumUrlError::MissingPort));

        // With colon but no digits.
        let empty_after_colon = validate_v1_pool_url("stratum+tcp://pool.example.com:");
        assert_eq!(empty_after_colon, Err(StratumUrlError::MissingPort));
    }

    #[test]
    fn validate_returns_invalid_port_for_zero() {
        // Port 0 is reserved per IANA — TCP connect would fail anyway, but
        // surface it at config-validation time so operators see the bug
        // before mining starts.
        let result = validate_v1_pool_url("stratum+tcp://pool.example.com:0");
        assert_eq!(result, Err(StratumUrlError::InvalidPort));
    }

    #[test]
    fn validate_returns_invalid_port_for_oversized_value() {
        // u16::MAX = 65535. Anything larger overflows and must surface
        // InvalidPort instead of silently wrapping.
        let result = validate_v1_pool_url("stratum+tcp://pool.example.com:65536");
        assert_eq!(result, Err(StratumUrlError::InvalidPort));

        let huge = validate_v1_pool_url("stratum+tcp://pool.example.com:99999");
        assert_eq!(huge, Err(StratumUrlError::InvalidPort));
    }

    #[test]
    fn validate_returns_invalid_port_for_non_numeric() {
        let result = validate_v1_pool_url("stratum+tcp://pool.example.com:abc");
        assert_eq!(result, Err(StratumUrlError::InvalidPort));

        let signed = validate_v1_pool_url("stratum+tcp://pool.example.com:-1");
        assert_eq!(signed, Err(StratumUrlError::InvalidPort));
    }

    #[test]
    fn validate_accepts_max_port_65535() {
        // Boundary case: u16::MAX is a legal port.
        let url = validate_v1_pool_url("stratum+tcp://pool.example.com:65535").unwrap();
        assert_eq!(url.port, 65535);
    }

    #[test]
    fn validate_accepts_min_port_1() {
        let url = validate_v1_pool_url("stratum+tcp://pool.example.com:1").unwrap();
        assert_eq!(url.port, 1);
    }

    #[test]
    fn validate_accepts_ipv4_literal() {
        let url = validate_v1_pool_url("stratum+tcp://203.0.113.10:3333").unwrap();
        assert_eq!(url.host, "203.0.113.10");
        assert_eq!(url.port, 3333);
    }

    #[test]
    fn stratum_url_error_display_messages_are_actionable() {
        // Operators read these error strings in config-validation logs.
        assert!(StratumUrlError::Empty.to_string().contains("empty"));
        assert!(StratumUrlError::OuterWhitespace
            .to_string()
            .contains("whitespace"));
        assert!(StratumUrlError::EmbeddedWhitespace
            .to_string()
            .contains("whitespace"));
        let scheme_err = StratumUrlError::UnsupportedScheme {
            expected: "stratum+tcp://host:port",
        };
        assert!(scheme_err.to_string().contains("stratum+tcp://host:port"));
        assert!(StratumUrlError::Credentials
            .to_string()
            .contains("credentials"));
        assert!(StratumUrlError::ExtraComponents
            .to_string()
            .to_lowercase()
            .contains("path"));
        assert!(StratumUrlError::EmptyHost.to_string().contains("empty"));
        assert!(StratumUrlError::InvalidHost.to_string().contains("invalid"));
        assert!(StratumUrlError::MissingPort.to_string().contains("missing"));
        assert!(StratumUrlError::InvalidPort.to_string().contains("invalid"));
    }

    // -----------------------------------------------------------------------
    // version_mask error-variant pinning + format tests.
    // -----------------------------------------------------------------------

    use crate::version_mask::{
        format_version_mask, parse_and_clamp_version_mask, parse_version_mask, VersionMaskError,
    };

    #[test]
    fn version_mask_rejects_empty_string_with_specific_variant() {
        assert_eq!(parse_version_mask(""), Err(VersionMaskError::Empty));
        assert_eq!(parse_version_mask("   "), Err(VersionMaskError::Empty));
        assert_eq!(parse_version_mask("\t"), Err(VersionMaskError::Empty));
    }

    #[test]
    fn version_mask_rejects_bare_0x_prefix_as_empty() {
        // "0x" alone has the prefix stripped to "" — must surface Empty,
        // not InvalidHex.
        assert_eq!(parse_version_mask("0x"), Err(VersionMaskError::Empty));
        assert_eq!(parse_version_mask("0X"), Err(VersionMaskError::Empty));
    }

    #[test]
    fn version_mask_rejects_non_hex_with_specific_variant() {
        // 7-char non-hex → InvalidHex (length check passes, hex check fails).
        assert_eq!(
            parse_version_mask("not-hex"),
            Err(VersionMaskError::InvalidHex)
        );
        // 4-char Z-only → InvalidHex (length check passes).
        assert_eq!(
            parse_version_mask("ZZZZ"),
            Err(VersionMaskError::InvalidHex)
        );
    }

    #[test]
    fn version_mask_length_check_runs_before_hex_validity_check() {
        // Length check at `digits.len() > 8` runs BEFORE the hex-validity
        // check. So an input that's both too long AND contains non-hex
        // characters surfaces TooWide, not InvalidHex. Pin this ordering
        // so a refactor that swaps the check order doesn't silently flip
        // which error operators see for "stratum sent gibberish".
        // "1fff e000" is 9 chars (> 8) — length check fires first.
        assert_eq!(
            parse_version_mask("1fff e000"),
            Err(VersionMaskError::TooWide)
        );
        assert_eq!(
            parse_version_mask("ZZZZZZZZZ"),
            Err(VersionMaskError::TooWide)
        );
    }

    #[test]
    fn version_mask_rejects_too_wide_with_specific_variant() {
        // 9 hex digits = 36 bits, > u32. Must surface TooWide, not InvalidHex.
        assert_eq!(
            parse_version_mask("100000000"),
            Err(VersionMaskError::TooWide)
        );
        assert_eq!(
            parse_version_mask("0x100000000"),
            Err(VersionMaskError::TooWide)
        );
    }

    #[test]
    fn version_mask_accepts_lowercase_0x_prefix() {
        // The existing test covered "0X" uppercase; pin the standard
        // lowercase form too.
        assert_eq!(parse_version_mask("0x1fffe000").unwrap(), 0x1fff_e000);
    }

    #[test]
    fn version_mask_trims_outer_whitespace_before_parsing() {
        // Trim happens at the top of parse_version_mask. Pin so a future
        // refactor that removes the trim would be caught.
        assert_eq!(parse_version_mask("  1fffe000  ").unwrap(), 0x1fff_e000);
        assert_eq!(parse_version_mask("\t0x00ffe000\n").unwrap(), 0x00ff_e000);
    }

    #[test]
    fn format_version_mask_zeropads_to_eight_digits() {
        assert_eq!(format_version_mask(0), "00000000");
        assert_eq!(format_version_mask(1), "00000001");
        assert_eq!(format_version_mask(0xFF), "000000ff");
        assert_eq!(format_version_mask(u32::MAX), "ffffffff");
    }

    #[test]
    fn format_version_mask_uses_lowercase_hex() {
        // Lowercase per BIP310 convention. Uppercase would still parse but
        // the wire format is conventionally lowercase.
        assert_eq!(format_version_mask(0xABCD_EF12), "abcdef12");
    }

    #[test]
    fn parse_and_clamp_propagates_specific_parse_error() {
        // A malformed pool mask must surface the specific parse error,
        // not a generic clamp error.
        assert_eq!(
            parse_and_clamp_version_mask("", 0x1fff_e000),
            Err(VersionMaskError::Empty)
        );
        assert_eq!(
            parse_and_clamp_version_mask("not-hex", 0x1fff_e000),
            Err(VersionMaskError::InvalidHex)
        );
        assert_eq!(
            parse_and_clamp_version_mask("100000000", 0x1fff_e000),
            Err(VersionMaskError::TooWide)
        );
    }

    #[test]
    fn clamp_to_zero_requested_mask_disables_rolling() {
        // requested_mask=0 means "operator does not want any version
        // rolling". Pool mask is forced to 0 regardless of what pool offered.
        assert_eq!(
            parse_and_clamp_version_mask("1fffe000", 0).unwrap(),
            0,
            "operator with mask=0 must override any pool-offered mask"
        );
    }

    #[test]
    fn clamp_with_zero_pool_mask_disables_rolling() {
        // Pool offering mask=0 means "I don't support rolling". Operator's
        // requested mask is ANDed with 0 → 0.
        assert_eq!(
            parse_and_clamp_version_mask("00000000", 0x1fff_e000).unwrap(),
            0
        );
    }
}
