//! BIP310 version-rolling mask helpers.
//!
//! Pools negotiate an initial `version-rolling.mask` in `mining.configure` and
//! may later update it with `mining.set_version_mask`. Parsing and clamping
//! live here so invalid pool messages cannot silently disable or widen
//! ASICBoost.

use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum VersionMaskError {
    #[error("version mask is empty")]
    Empty,

    #[error("version mask must be hexadecimal")]
    InvalidHex,

    #[error("version mask must fit in 32 bits")]
    TooWide,
}

pub fn parse_version_mask(mask: &str) -> Result<u32, VersionMaskError> {
    let trimmed = mask.trim();
    if trimmed.is_empty() {
        return Err(VersionMaskError::Empty);
    }

    let digits = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    if digits.is_empty() {
        return Err(VersionMaskError::Empty);
    }
    if digits.len() > 8 {
        return Err(VersionMaskError::TooWide);
    }
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(VersionMaskError::InvalidHex);
    }

    u32::from_str_radix(digits, 16).map_err(|_| VersionMaskError::InvalidHex)
}

pub fn clamp_to_requested_mask(pool_mask: u32, requested_mask: u32) -> u32 {
    pool_mask & requested_mask
}

pub fn parse_and_clamp_version_mask(
    pool_mask: &str,
    requested_mask: u32,
) -> Result<u32, VersionMaskError> {
    Ok(clamp_to_requested_mask(
        parse_version_mask(pool_mask)?,
        requested_mask,
    ))
}

pub fn format_version_mask(mask: u32) -> String {
    format!("{mask:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bip310_hex_masks() {
        assert_eq!(parse_version_mask("1fffe000").unwrap(), 0x1fff_e000);
        assert_eq!(parse_version_mask("0X00FFE000").unwrap(), 0x00ff_e000);
        assert_eq!(parse_version_mask("00000000").unwrap(), 0);
    }

    #[test]
    fn rejects_missing_or_malformed_masks() {
        for bad in ["", "0x", "not-hex", "100000000", "1fff e000"] {
            assert!(parse_version_mask(bad).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn clamps_pool_mask_to_operator_requested_bits() {
        assert_eq!(
            parse_and_clamp_version_mask("1fffe000", 0x00ff_e000).unwrap(),
            0x00ff_e000
        );
        assert_eq!(
            parse_and_clamp_version_mask("0000e000", 0x00ff_e000).unwrap(),
            0x0000_e000
        );
    }

    #[test]
    fn formats_lowercase_eight_digit_mask() {
        assert_eq!(format_version_mask(0x00ff_e000), "00ffe000");
    }
}
