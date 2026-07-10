//! Coin-parameterization seam (W3-B kickoff).
//!
//! DCENT_OS's Stratum V1 path was written for one coin (Bitcoin / SHA-256d).
//! Litecoin/Dogecoin **Scrypt** mining (Antminer L7 / BM1489) uses the *same*
//! Stratum V1 job/notify/submit wire pipeline and the *same* 80-byte block
//! header, but differs in three coin-specific ways:
//!
//!   1. **PoW function** — Scrypt, not double-SHA-256 (no SHA-256d midstates).
//!   2. **Version rolling** — Scrypt pools do NOT roll `version` (no BIP320 /
//!      AsicBoost); the header `version` is a plain field.
//!   3. **Difficulty-1 target family** — an altcoin pool may define its
//!      difficulty-1 target / hash display order differently, so the SHA-256d
//!      `difficulty_to_target` helper must not be assumed universal.
//!
//! This module is the **minimal seam** that makes an LTC coin config
//! *expressible* without rewriting the Bitcoin path. It is a pure descriptor:
//! it opens no sockets, builds no work, and validates no shares. The daemon /
//! work builder can branch on [`CoinParams`] at the seam points instead of
//! hardcoding Bitcoin assumptions.
//!
//! Default-OFF: only compiled under the `scrypt-l7` Cargo feature, so production
//! SHA-256 builds are byte-unchanged. The existing SHA-256d path is the implicit
//! [`CoinParams::BITCOIN`] behavior and is left entirely untouched.
//!
//! The Scrypt-specific wire contracts (80-byte preimage, full nonce range,
//! target byte order / comparison) already live in [`crate::scrypt`]; this seam
//! composes them into a per-coin descriptor rather than duplicating them.

use crate::scrypt::{
    ScryptDifficultyConversion, ScryptMidstateFormat, SCRYPT_DIFFICULTY_CONVERSION,
    SCRYPT_MIDSTATE_FORMAT,
};

/// Proof-of-work function a coin uses over its 80-byte block header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowAlgorithm {
    /// Bitcoin and SHA-256d ASICs (BM1387/BM139x…). Uses pre-computed SHA-256
    /// midstates; supports BIP320 version rolling.
    Sha256d,
    /// Litecoin / Dogecoin Scrypt ASICs (L7 / BM1489). Full 80-byte header
    /// preimage; NO version rolling; host-side difficulty filtering.
    Scrypt,
}

/// The per-coin parameters the Stratum V1 pipeline branches on.
///
/// Everything a coin needs to keep its share-acceptance / work-build seam
/// honest, without a second Stratum client. All fields are `'static` so the
/// well-known coins can be `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoinParams {
    /// Short ticker used in config / logs (e.g. `"BTC"`, `"LTC"`).
    pub coin_id: &'static str,
    /// Human-readable coin name.
    pub name: &'static str,
    /// Proof-of-work function over the 80-byte header.
    pub algorithm: PowAlgorithm,
    /// Block-header size fed to the PoW function. 80 for both BTC and LTC.
    pub pow_header_bytes: usize,
    /// Whether the pool/miner rolls the header `version` field (BIP320).
    /// `true` for Bitcoin SHA-256d (AsicBoost); `false` for Scrypt.
    ///
    /// LOAD-BEARING: Scrypt MUST stay `false`. The BIP320-rejection ban-gate
    /// applies to the am2 BM1362 SHA-256 path; Scrypt genuinely has no version
    /// rolling, so the L7 seam simply never wires it (it does not touch or
    /// re-add any BM1362 version-rolling guard).
    pub version_rolling_allowed: bool,
    /// Difficulty-1 target family label — a hint that the SHA-256d
    /// `v1::difficulty::difficulty_to_target` helper may not be the right
    /// diff→target conversion for this coin.
    pub diff1_target_family: &'static str,
}

impl CoinParams {
    /// Bitcoin / SHA-256d — the implicit legacy behavior of the existing V1
    /// path. Provided so callers can express "the default coin" explicitly at
    /// the seam without changing any Bitcoin code.
    pub const BITCOIN: CoinParams = CoinParams {
        coin_id: "BTC",
        name: "Bitcoin",
        algorithm: PowAlgorithm::Sha256d,
        pow_header_bytes: 80,
        version_rolling_allowed: true,
        diff1_target_family: "bitcoin-pdiff-1",
    };

    /// Litecoin / Scrypt (Antminer L7, BM1489). Composes the Scrypt wire
    /// contracts from [`crate::scrypt`].
    pub const LITECOIN: CoinParams = CoinParams {
        coin_id: "LTC",
        name: "Litecoin",
        algorithm: PowAlgorithm::Scrypt,
        // W3-A §5: Scrypt on LTC/DOGE uses the standard 80-byte header.
        pow_header_bytes: SCRYPT_MIDSTATE_FORMAT.header_bytes,
        // W3-A §5: Scrypt has NO BIP320 version-rolling.
        version_rolling_allowed: false,
        diff1_target_family: SCRYPT_DIFFICULTY_CONVERSION.diff1_target_family,
    };

    /// Whether this coin uses pre-computed SHA-256d midstates. `false` for
    /// Scrypt — a caller must NOT pass SHA-256d midstates on the Scrypt path.
    pub const fn uses_sha256d_midstates(&self) -> bool {
        matches!(self.algorithm, PowAlgorithm::Sha256d)
    }

    /// The Scrypt midstate/preimage contract for this coin, or `None` for
    /// SHA-256d coins (which use the existing `WorkBuilder` midstate path).
    pub fn scrypt_midstate_format(&self) -> Option<ScryptMidstateFormat> {
        match self.algorithm {
            PowAlgorithm::Scrypt => Some(SCRYPT_MIDSTATE_FORMAT),
            PowAlgorithm::Sha256d => None,
        }
    }

    /// The Scrypt difficulty-conversion contract for this coin, or `None` for
    /// SHA-256d coins (which use `v1::difficulty::difficulty_to_target`).
    pub fn scrypt_difficulty_conversion(&self) -> Option<ScryptDifficultyConversion> {
        match self.algorithm {
            PowAlgorithm::Scrypt => Some(SCRYPT_DIFFICULTY_CONVERSION),
            PowAlgorithm::Sha256d => None,
        }
    }

    /// Resolve a coin by its config ticker (case-insensitive). Returns `None`
    /// for an unknown ticker so config parsing can fail closed.
    pub fn from_coin_id(id: &str) -> Option<CoinParams> {
        match id.to_ascii_uppercase().as_str() {
            "BTC" | "BITCOIN" => Some(Self::BITCOIN),
            "LTC" | "LITECOIN" | "DOGE" | "DOGECOIN" => Some(Self::LITECOIN),
            _ => None,
        }
    }
}

impl Default for CoinParams {
    /// Bitcoin is the default coin — the existing SHA-256d behavior.
    fn default() -> Self {
        Self::BITCOIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_coin_is_bitcoin_sha256d() {
        let c = CoinParams::default();
        assert_eq!(c, CoinParams::BITCOIN);
        assert_eq!(c.algorithm, PowAlgorithm::Sha256d);
        assert!(c.uses_sha256d_midstates());
        assert!(c.version_rolling_allowed, "BTC rolls version (BIP320)");
        assert!(c.scrypt_midstate_format().is_none());
        assert!(c.scrypt_difficulty_conversion().is_none());
    }

    #[test]
    fn litecoin_is_scrypt_no_version_rolling() {
        let c = CoinParams::LITECOIN;
        assert_eq!(c.algorithm, PowAlgorithm::Scrypt);
        assert!(!c.uses_sha256d_midstates());
        // LOAD-BEARING: Scrypt never rolls version.
        assert!(
            !c.version_rolling_allowed,
            "Scrypt must not wire BIP320 version rolling"
        );
        // Same 80-byte header as Bitcoin.
        assert_eq!(c.pow_header_bytes, 80);
        assert_eq!(c.pow_header_bytes, CoinParams::BITCOIN.pow_header_bytes);
    }

    #[test]
    fn scrypt_contracts_are_exposed_via_the_seam() {
        let c = CoinParams::LITECOIN;
        let ms = c.scrypt_midstate_format().expect("LTC has a scrypt format");
        // Must NOT claim SHA-256d midstates (W3-A §5).
        assert_eq!(ms.sha256d_midstate_words, 0);
        assert_eq!(ms.header_bytes, 80);
        let dc = c
            .scrypt_difficulty_conversion()
            .expect("LTC has a scrypt diff conversion");
        assert_eq!(dc.diff1_target_family, c.diff1_target_family);
    }

    #[test]
    fn from_coin_id_resolves_known_and_fails_closed_on_unknown() {
        assert_eq!(CoinParams::from_coin_id("btc"), Some(CoinParams::BITCOIN));
        assert_eq!(CoinParams::from_coin_id("BTC"), Some(CoinParams::BITCOIN));
        assert_eq!(CoinParams::from_coin_id("ltc"), Some(CoinParams::LITECOIN));
        assert_eq!(
            CoinParams::from_coin_id("Dogecoin"),
            Some(CoinParams::LITECOIN)
        );
        assert_eq!(CoinParams::from_coin_id("eth"), None);
        assert_eq!(CoinParams::from_coin_id(""), None);
    }

    #[test]
    fn seam_does_not_perturb_the_bitcoin_path() {
        // The Bitcoin descriptor must be byte-identical to "the default" and
        // carry no Scrypt state — proving the seam is additive.
        assert_eq!(CoinParams::default(), CoinParams::BITCOIN);
        assert_ne!(CoinParams::BITCOIN, CoinParams::LITECOIN);
        assert!(CoinParams::BITCOIN.scrypt_midstate_format().is_none());
    }
}
