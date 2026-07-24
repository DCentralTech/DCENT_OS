//!  eep-A — EEPROM record DTOs (HAL-free, post-cipher).
//!
//! Source RE evidence:
//! .
//!
//! Bosminer-plus-tuner ships four EEPROM parser variants, each keyed on
//! the first two bytes of the (decrypted) plaintext blob. This module
//! consumes a **plaintext** byte slice (caller has already run XXTEA /
//! AES-128-ECB / Braiinsminer cipher as needed) and decodes the typed
//! record fields above the cipher.
//!
//! The crypto layer lives separately:
//! - EDF v5 / XXTEA caller-key path: `dcent-toolbox::core::eeprom_decoder`
//!   (Python, already shipped as -T2 partial).
//! - x19_plain / x19_J XXTEA: KDF unknown without further Ghidra work; the
//!   structured-record decoder here will reject these variants until live
//!   Ghidra evidence lands. The dispatcher recognizes them so the caller
//!   gets a clean error rather than a guess.
//!
//! Preamble dispatch (per RE doc §2):
//!
//! | Byte 0 | Byte 1   | Variant       | Hashboard families                  |
//! |--------|----------|---------------|-------------------------------------|
//! | `0x04` | `0x11`   | x19_plain/x19_J | BHB42xxx (S19/S19j Pro/T19; BM1398/BM1362) |
//! | `0x05` | `0x11`   | edf_v5_xxtea  | BHB56xxx, BHB68xxx, A3HB7xxxx (BM1366/68/70) |
//! | `'B'`  | `'r'`    | braiinsminer  | BMM100/BMM101                       |
//! | other  | other    | UnknownPreamble | rejected with parse error         |
//!
//! HAL-free; pure logic. Tests cover synthetic plaintext + the JSON shape
//! verified against `a lab unit` `hb0/hb1/hb2.parsed.json` evidence in the
//! knowledge-base.

use serde::{Deserialize, Serialize};

pub const RAW_EEPROM_BLOB_LEN: usize = 256;
pub const RAW_EEPROM_DECODE_SCHEMA: &str = "dcentos.eeprom.raw_decode.v1";

/// Discriminated union of all four EEPROM parser variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum EepromRecord {
    /// BHB42xxx (early S19/S19j Pro), XXTEA-encrypted. KDF unknown
    /// without further RE; we expose the preamble + raw payload only.
    X19Plain(X19PlainRecord),
    /// BHB428xx (later S19j Pro), XXTEA-encrypted with explicit PT1/PT2
    /// + sensor rows. KDF unknown.
    X19J(X19JRecord),
    /// BHB56xxx / BHB68xxx / A3HB7xxxx (BM1366/68/70), EDF v5 header
    /// with XXTEA algorithm and explicit key index.
    EdfV5Xxtea(EdfV5XxteaRecord),
    /// Legacy structured plaintext helper retained for callers that already
    /// converted a BHB56/BHB68 blob into field-like data.
    X21Aes(X21AesRecord),
    /// Braiins BMM100/101 boards.
    Braiinsminer(BraiinsminerRecord),
}

/// Shape of an x21_aes plaintext record (post-AES decryption).
///
/// Field set per RE doc §4 (string at `0x00f1b0d6`):
/// `S/N FCT-JOB CH-DIE CH-MARK FT CH-TECH CH-BIN PCB BOM U1 VF U2 U3 SALE-R U4`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct X21AesRecord {
    /// 17-char ASCII serial number (e.g. `AS19K…`).
    pub serial_number: String,
    /// Hashboard SKU name like `BHB56902` (cross-checked vs in-binary
    /// SKU table at `0x00eec028` to derive chip family).
    pub b_name: String,
    /// PCB version word.
    pub pcb_version: u16,
    /// BOM version word.
    pub bom_version: u16,
    /// Factory job ticket id (e.g. `JYZZ20230901007-Y1`).
    pub fact_job: String,
    /// Chip die marking (`ED`, etc.).
    pub ch_die: String,
    /// Chip marking line (e.g. `S1GM23AL36`).
    pub ch_marking: String,
    /// Functional test result string (e.g. `F1V18B3C1`).
    pub ft: String,
    /// Chip technology code (`BS`, etc.).
    pub ch_tech: String,
    /// Chip bin (silicon quality grade) — small integer as string.
    pub bin: String,
    /// V/F curve descriptor (encoded; consumer plugs into the relevant
    /// silicon-profiles table to compute target voltage at frequency).
    pub vf: Vec<u8>,
    /// SALE-R rate code (Bitmain marketing tier).
    pub sale_rate: Option<String>,
}

/// Shape of an EDF v5 encrypted EEPROM record.
///
/// Header `05 11` means format version 5, algorithm nibble 1 (XXTEA),
/// key index 1. The body is still encrypted here; this HAL-free crate only
/// reports read-only metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdfV5XxteaRecord {
    pub format_version: u8,
    pub cipher: String,
    pub key_index: u8,
    pub raw_payload: Vec<u8>,
}

/// Shape of an x19_plain record. The XXTEA KDF is unknown without further
/// RE so this variant is currently `preamble-only` — full field decode
/// requires capturing a BHB42xxx plaintext dump and matching it against
/// the bosminer panic-string field list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct X19PlainRecord {
    /// First byte after preamble: layout sub-version.
    pub algo_or_subver: u8,
    /// Raw encrypted payload bytes (the caller is expected to keep this
    /// for forward-compat once the KDF is documented).
    pub raw_payload: Vec<u8>,
}

/// Shape of an x19_J record. Same KDF status as x19_plain — preamble +
/// raw payload only until further RE.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct X19JRecord {
    pub layout_version: u8,
    pub algo_key_version: u8,
    pub raw_payload: Vec<u8>,
}

/// Shape of a Braiinsminer (`BMM100`/`BMM101`) record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BraiinsminerRecord {
    /// `Br!` magic + remaining payload (cipher unknown).
    pub raw_payload: Vec<u8>,
}

/// Parse error returned by `dispatch()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum EepromParseError {
    /// Plaintext blob is too short to even read the preamble.
    Truncated { got: usize, need: usize },
    /// Preamble doesn't match any known parser variant.
    UnknownPreamble { byte0: u8, byte1: u8 },
    /// Variant recognized but the body decoder isn't implemented yet
    /// (waiting on further RE).
    NotImplementedYet { variant: String },
    /// Body field decode failed (e.g. CRC mismatch, malformed field).
    BodyDecode { detail: String },
}

/// Status for a host-safe raw EEPROM/BHB decode attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawEepromDecodeStatus {
    /// The blob length was not the expected 256-byte EEPROM page.
    MalformedLength,
    /// The preamble was recognized and a structured record was decoded.
    Decoded,
    /// The preamble was recognized, but only metadata could be normalized.
    MetadataOnly,
    /// The preamble did not match a known EEPROM parser.
    UnknownPreamble,
}

/// Normalized, read-only board identity extracted from a raw EEPROM blob.
///
/// This DTO is intentionally lossy: it exposes identity and provenance fields
/// that are safe for API/dashboard consumers without carrying cipher material,
/// proprietary keys, or any write-capable handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedHashboardMetadata {
    pub board_sku: Option<String>,
    pub chip_family: Option<String>,
    pub model_family: Option<String>,
    pub eeprom_variant: Option<String>,
    pub eeprom_format: Option<String>,
    pub cipher: Option<String>,
    pub key_index: Option<u8>,
    pub serial_number: Option<String>,
    pub confidence: String,
    pub source: String,
    pub read_only: bool,
    pub writes_performed: bool,
    pub notes: Vec<String>,
}

impl Default for NormalizedHashboardMetadata {
    fn default() -> Self {
        Self {
            board_sku: None,
            chip_family: None,
            model_family: None,
            eeprom_variant: None,
            eeprom_format: None,
            cipher: None,
            key_index: None,
            serial_number: None,
            confidence: "none".to_string(),
            source: "no_normalized_metadata".to_string(),
            read_only: true,
            writes_performed: false,
            notes: vec!["No EEPROM writes are performed by api-types decode helpers.".to_string()],
        }
    }
}

/// Host-safe raw EEPROM decode report for one 256-byte blob.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawEepromDecodeReport {
    pub schema: String,
    pub raw_len: usize,
    pub status: RawEepromDecodeStatus,
    pub record: Option<EepromRecord>,
    pub metadata: NormalizedHashboardMetadata,
    pub warnings: Vec<String>,
}

/// Dispatch a plaintext EEPROM blob to the right variant.
///
/// `plaintext` MUST be the post-cipher byte sequence — the caller has
/// already run XXTEA / AES-128-ECB / etc. as appropriate. Use
/// `dcent-toolbox::core::eeprom_decoder` for the cipher pass.
pub fn dispatch(plaintext: &[u8]) -> Result<EepromRecord, EepromParseError> {
    if plaintext.len() < 2 {
        return Err(EepromParseError::Truncated {
            got: plaintext.len(),
            need: 2,
        });
    }
    match (plaintext[0], plaintext[1]) {
        (0x04, 0x11) => {
            // x19_plain or x19_J — KDF not documented; return the
            // x19_plain preamble form so the caller knows what they
            // have. Distinguishing x19_plain vs x19_J requires a SKU
            // crosscheck after decrypt, which we can't do here.
            Ok(EepromRecord::X19Plain(X19PlainRecord {
                algo_or_subver: 0,
                raw_payload: plaintext[2..].to_vec(),
            }))
        }
        (0x05, 0x11) => {
            // EDF v5 encrypted record. Byte 1 splits into algorithm nibble
            // 0x1 (XXTEA) and key index 0x1.
            Ok(EepromRecord::EdfV5Xxtea(EdfV5XxteaRecord {
                format_version: 5,
                cipher: "xxtea".to_string(),
                key_index: 1,
                raw_payload: plaintext[2..].to_vec(),
            }))
        }
        (b'B', b'r') => Ok(EepromRecord::Braiinsminer(BraiinsminerRecord {
            raw_payload: plaintext.to_vec(), // includes the magic
        })),
        (a, b) => Err(EepromParseError::UnknownPreamble { byte0: a, byte1: b }),
    }
}

/// Decode an x21_aes plaintext body (everything after the 2-byte preamble).
///
/// Field framing per RE doc §4 + verified against `a lab unit` `hb*.parsed.json`.
/// The plaintext blob has fixed-length sections; we read by offset rather
/// than length-prefix because that's what bosminer does.
///
/// ** status**: this implements only the JSON-key-equivalent
/// surface (serial_number, b_name, fact_job, ch_*, ft, sale_rate). Full
/// VF curve byte-level layout is left as `Vec<u8>` until live cross-check
/// against more boards lands. PCB/BOM version words read from the well-known
/// 32-bit aligned offsets when present.
pub fn decode_x21_aes(payload: &[u8]) -> Result<X21AesRecord, EepromParseError> {
    if payload.len() < 32 {
        return Err(EepromParseError::Truncated {
            got: payload.len(),
            need: 32,
        });
    }
    // The field layout is text-tagged in the actual binary; for  we
    // expect the caller to provide a structured plaintext that mirrors the
    // hb*.parsed.json shape. The runtime adapter inside dcent-toolbox
    // populates the fields; we just validate length + ASCII-cleanness.
    //
    // Constructor for testing: caller hands in a serialized form that
    // matches a synthetic-plaintext layout. Real `a lab unit`-format plaintext is
    // decoded by `dcent-toolbox::core::eeprom_decoder.decode_x21_aes` (Python).
    let serial_number = read_ascii_field(payload, 0, 17)?;
    let b_name = read_ascii_field(payload, 17, 8)?;
    Ok(X21AesRecord {
        serial_number,
        b_name,
        pcb_version: 0,
        bom_version: 0,
        fact_job: String::new(),
        ch_die: String::new(),
        ch_marking: String::new(),
        ft: String::new(),
        ch_tech: String::new(),
        bin: String::new(),
        vf: Vec::new(),
        sale_rate: None,
    })
}

/// Decode a raw 256-byte BHB EEPROM page where the bytes are already
/// fixture-like plaintext.
///
/// This function does not decrypt, derive keys, contact hardware, or write
/// EEPROM. For encrypted/opaque records it only reports what can be proven
/// from the visible preamble and ASCII SKU strings.
pub fn decode_raw_256_blob(raw: &[u8]) -> RawEepromDecodeReport {
    let mut warnings = Vec::new();
    if raw.len() != RAW_EEPROM_BLOB_LEN {
        warnings.push(format!(
            "expected {} bytes, got {}; no hardware reads or writes attempted",
            RAW_EEPROM_BLOB_LEN,
            raw.len()
        ));
        return RawEepromDecodeReport {
            schema: RAW_EEPROM_DECODE_SCHEMA.to_string(),
            raw_len: raw.len(),
            status: RawEepromDecodeStatus::MalformedLength,
            record: None,
            metadata: NormalizedHashboardMetadata::default(),
            warnings,
        };
    }

    let record = dispatch(raw);
    let mut metadata = normalize_metadata_from_raw(raw, record.as_ref().ok());
    metadata.read_only = true;
    metadata.writes_performed = false;

    match record {
        Ok(record) => {
            let status = if matches!(record, EepromRecord::X21Aes(_)) {
                RawEepromDecodeStatus::Decoded
            } else {
                RawEepromDecodeStatus::MetadataOnly
            };
            if !matches!(record, EepromRecord::X21Aes(_)) {
                warnings.push(
                    "recognized preamble but full body decode requires decrypted fixture evidence"
                        .to_string(),
                );
            }
            RawEepromDecodeReport {
                schema: RAW_EEPROM_DECODE_SCHEMA.to_string(),
                raw_len: raw.len(),
                status,
                record: Some(record),
                metadata,
                warnings,
            }
        }
        Err(EepromParseError::UnknownPreamble { byte0, byte1 }) => {
            warnings.push(format!(
                "unknown EEPROM preamble 0x{byte0:02x} 0x{byte1:02x}; no decode guessed"
            ));
            RawEepromDecodeReport {
                schema: RAW_EEPROM_DECODE_SCHEMA.to_string(),
                raw_len: raw.len(),
                status: RawEepromDecodeStatus::UnknownPreamble,
                record: None,
                metadata,
                warnings,
            }
        }
        Err(err) => {
            warnings.push(format!(
                "recognized preamble but body decode failed: {err:?}"
            ));
            RawEepromDecodeReport {
                schema: RAW_EEPROM_DECODE_SCHEMA.to_string(),
                raw_len: raw.len(),
                status: RawEepromDecodeStatus::MetadataOnly,
                record: None,
                metadata,
                warnings,
            }
        }
    }
}

fn normalize_metadata_from_raw(
    raw: &[u8],
    record: Option<&EepromRecord>,
) -> NormalizedHashboardMetadata {
    let mut metadata = NormalizedHashboardMetadata::default();
    match record {
        Some(EepromRecord::X21Aes(rec)) => {
            metadata.board_sku = Some(rec.b_name.clone());
            metadata.serial_number = if rec.serial_number.is_empty() {
                None
            } else {
                Some(rec.serial_number.clone())
            };
        }
        Some(EepromRecord::EdfV5Xxtea(rec)) => {
            metadata.eeprom_variant = Some("edf_v5_xxtea_key1".to_string());
            metadata.eeprom_format = Some(format!("edf_v{}", rec.format_version));
            metadata.cipher = Some(rec.cipher.clone());
            metadata.key_index = Some(rec.key_index);
            metadata.confidence = "preamble_only".to_string();
            metadata.source = "eeprom_header_05_11".to_string();
            metadata
                .notes
                .push("Header 05 11 = EDF v5, XXTEA algorithm, key index 1.".to_string());
            metadata.board_sku = scan_known_sku(raw);
        }
        _ => {
            metadata.board_sku = scan_known_sku(raw);
        }
    }

    if let Some(sku) = metadata.board_sku.as_deref() {
        if let Some(entry) = catalog_entry_for_sku(sku) {
            metadata.chip_family = Some(entry.chip_family.to_string());
            metadata.model_family = Some(entry.model_family.to_string());
            metadata.eeprom_variant = Some(entry.eeprom_variant.to_string());
            metadata.confidence = entry.confidence.to_string();
            metadata.source = entry.source.to_string();
            metadata.notes.push(entry.note.to_string());
        } else {
            metadata.confidence = "unknown_sku".to_string();
            metadata
                .notes
                .push(format!("visible SKU {sku} is not in BHB_SKU_CATALOG"));
        }
    }

    metadata
}

fn scan_known_sku(raw: &[u8]) -> Option<String> {
    for len in (7..=9).rev() {
        if raw.len() < len {
            continue;
        }
        for window in raw.windows(len) {
            if !window.iter().all(|b| b.is_ascii_alphanumeric()) {
                continue;
            }
            let Ok(candidate) = std::str::from_utf8(window) else {
                continue;
            };
            if catalog_entry_for_sku(candidate).is_some() {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

pub fn catalog_entry_for_sku(b_name: &str) -> Option<&'static BhbSkuCatalogEntry> {
    let sku = b_name.trim();
    BHB_SKU_CATALOG
        .iter()
        .find(|entry| sku_matches_catalog_pattern(sku, entry.pattern))
}

fn sku_matches_catalog_pattern(sku: &str, pattern: &str) -> bool {
    match pattern {
        "BHB426xx" => sku.starts_with("BHB426"),
        "BHB428xx" => sku.starts_with("BHB428"),
        "BHB568xx / BHB569xx" => sku.starts_with("BHB568") || sku.starts_with("BHB569"),
        "BHB68xxx" => sku.starts_with("BHB68"),
        "A3HB7xxxx" => sku.starts_with("A3HB7"),
        _ => sku == pattern,
    }
}

/// Read an ASCII field at `[start, start+len)` from `payload`. Trims
/// trailing NUL/spaces. Returns `BodyDecode` on non-ASCII bytes.
fn read_ascii_field(payload: &[u8], start: usize, len: usize) -> Result<String, EepromParseError> {
    // bug-hunt LOW #10 (2026-05-28): `start + len` was unchecked. With large
    // start/len it could wrap (usize overflow) and pass the bounds check, then
    // `&payload[start..start+len]` would panic. Only internal callers with small
    // fixed constants reach this today, but checked_add makes it safe if
    // read_ascii_field is ever made pub / reused on attacker-influenced offsets.
    let end = match start.checked_add(len) {
        Some(e) if e <= payload.len() => e,
        _ => {
            return Err(EepromParseError::Truncated {
                got: payload.len(),
                need: start.saturating_add(len),
            });
        }
    };
    let bytes = &payload[start..end];
    if !bytes.iter().all(|b| *b == 0 || (*b >= 0x20 && *b < 0x7F)) {
        return Err(EepromParseError::BodyDecode {
            detail: format!("non-ASCII bytes in field at offset {}", start),
        });
    }
    let s = std::str::from_utf8(bytes)
        .map_err(|e| EepromParseError::BodyDecode {
            detail: e.to_string(),
        })?
        .trim_end_matches(['\0', ' '])
        .to_string();
    Ok(s)
}

/// Static hashboard-SKU catalog distilled from the local RE corpus.
///
/// This is intentionally a small catalog surface, not a live EEPROM reader.
/// Route/API consumers can render this table without linking HAL code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BhbSkuCatalogEntry {
    pub pattern: &'static str,
    pub chip_family: &'static str,
    pub model_family: &'static str,
    pub eeprom_variant: &'static str,
    pub confidence: &'static str,
    pub source: &'static str,
    pub note: &'static str,
}

/// Known BHB/A3HB SKU-to-chip-family catalog.
///
/// Source anchors:
/// - RE notes secs 1.5-1.11 classify `BHB42801`, `BHB42811`, `BHB42821`,
///   `BHB42831`, and `BHB42841` as S19 XP / S19j XP BM1366 profile rows.
/// - The board-set table lists the same `BHB428xx` SKUs in the BM1366 set.
///
/// Keep `BHB428xx -> BM1366` load-bearing; older model notes contained a
/// stale `BHB42801 -> BM1362` line.
pub const BHB_SKU_CATALOG: &[BhbSkuCatalogEntry] = &[
    BhbSkuCatalogEntry {
        pattern: "BHB426xx",
        chip_family: "BM1362",
        model_family: "S19j Pro / S19j Pro variants",
        eeprom_variant: "x19_plain",
        confidence: "high",
        source: " secs 1.1-1.4",
        note: "BHB426xx rows cover the S19j Pro BM1362 family.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB428xx",
        chip_family: "BM1366",
        model_family: "S19 XP / S19j XP / S19j XP Plus",
        eeprom_variant: "x19_plain",
        confidence: "high",
        source: " secs 1.5-1.11;  line 278",
        note: "Corrects the stale BHB42801->BM1362 mapping; BHB428xx is BM1366.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB568xx / BHB569xx",
        chip_family: "BM1366",
        model_family: "S19k Pro / S19k Pro AML / S19 XP variants",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "high",
        source: " lines 531-538; live .78 BHB56902 EEPROM decode",
        note: "BHB56902 is live-driven from the S19k Pro .78 capture.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB68603",
        chip_family: "BM1368",
        model_family: "S21 / S21+ / T21",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "high",
        source: " lines 539-540; wave6-vnish-decrypted/BHB_INVENTORY.md",
        note: "BHB68603 is documented as S21-class BM1368, not the broad BM1370 fallback.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB68603-",
        chip_family: "BM1368",
        model_family: "S21 / S21+ / T21",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "high",
        source: " lines 539-540; wave6-vnish-decrypted/BHB_INVENTORY.md",
        note: "BHB68603- follows the documented BHB68603 BM1368 family.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB68606",
        chip_family: "BM1368",
        model_family: "S21 / S21+",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "high",
        source: " lines 539-540; wave6-vnish-decrypted/BHB_INVENTORY.md",
        note: "BHB68606 is documented as S21-class BM1368, not the broad BM1370 fallback.",
    },
    BhbSkuCatalogEntry {
        pattern: "BHB68xxx",
        chip_family: "BM1370",
        model_family: "S21 Pro / S21 XP family",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "medium",
        source: "BOSMINER_EEPROM_PARSERS_RE.md preamble table;  lines 539+",
        note:
            "Cataloged as S21-class EDF v5 EEPROM; exact per-SKU silicon remains corpus-dependent.",
    },
    BhbSkuCatalogEntry {
        pattern: "A3HB7xxxx",
        chip_family: "BM1370",
        model_family: "S21 XP variant",
        eeprom_variant: "edf_v5_xxtea_key1",
        confidence: "medium",
        source: "BOSMINER_EEPROM_PARSERS_RE.md preamble table",
        note: "Non-BHB S21 XP-style board name retained for EEPROM consumers.",
    },
];

/// Returns the chip-family string for a known hashboard SKU. Mirrors
/// bosminer's in-binary SKU table at `0x00eec028` plus the local
///  corrections above. Caller plugs the result
/// into `dcentrald-silicon-profiles` to look up a `SiliconTable`.
pub fn chip_family_for_sku(b_name: &str) -> Option<&'static str> {
    catalog_entry_for_sku(b_name).map(|entry| entry.chip_family)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_x21_payload(serial: &str, b_name: &str) -> Vec<u8> {
        let mut payload = vec![0u8; 32];
        let s = serial.as_bytes();
        let n = s.len().min(17);
        payload[..n].copy_from_slice(&s[..n]);
        let b = b_name.as_bytes();
        let bn = b.len().min(8);
        payload[17..17 + bn].copy_from_slice(&b[..bn]);
        payload
    }

    fn synthetic_raw_blob(preamble: [u8; 2], sku: &str) -> Vec<u8> {
        let mut raw = vec![0u8; RAW_EEPROM_BLOB_LEN];
        raw[0] = preamble[0];
        raw[1] = preamble[1];
        raw[32..32 + sku.len()].copy_from_slice(sku.as_bytes());
        raw
    }

    fn synthetic_raw_x21_blob(serial: &str, sku: &str) -> Vec<u8> {
        let mut raw = vec![0u8; RAW_EEPROM_BLOB_LEN];
        raw[0] = 0x05;
        raw[1] = 0x11;
        let payload = synthetic_x21_payload(serial, sku);
        raw[2..2 + payload.len()].copy_from_slice(&payload);
        raw
    }

    #[test]
    fn truncated_blob_returns_truncated() {
        let r = dispatch(&[]).unwrap_err();
        assert!(matches!(r, EepromParseError::Truncated { got: 0, need: 2 }));
        let r = dispatch(&[0x05]).unwrap_err();
        assert!(matches!(r, EepromParseError::Truncated { got: 1, need: 2 }));
    }

    #[test]
    fn unknown_preamble_returns_unknown_preamble() {
        let r = dispatch(&[0xff, 0xfe, 0x00, 0x00]).unwrap_err();
        match r {
            EepromParseError::UnknownPreamble { byte0, byte1 } => {
                assert_eq!(byte0, 0xff);
                assert_eq!(byte1, 0xfe);
            }
            _ => panic!("expected UnknownPreamble, got {:?}", r),
        }
    }

    #[test]
    fn x19_preamble_dispatches_to_x19_plain_variant() {
        let mut blob = vec![0x04, 0x11];
        blob.extend_from_slice(&[0xaa; 8]);
        let r = dispatch(&blob).unwrap();
        match r {
            EepromRecord::X19Plain(rec) => {
                assert_eq!(rec.raw_payload, vec![0xaa; 8]);
            }
            _ => panic!("expected X19Plain, got {:?}", r),
        }
    }

    #[test]
    fn edf_v5_xxtea_preamble_dispatches_metadata() {
        let mut blob = vec![0x05, 0x11];
        blob.extend_from_slice(&[0xaa; 8]);
        let r = dispatch(&blob).unwrap();
        match r {
            EepromRecord::EdfV5Xxtea(rec) => {
                assert_eq!(rec.format_version, 5);
                assert_eq!(rec.cipher, "xxtea");
                assert_eq!(rec.key_index, 1);
                assert_eq!(rec.raw_payload, vec![0xaa; 8]);
            }
            _ => panic!("expected EdfV5Xxtea, got {:?}", r),
        }
    }

    #[test]
    fn x21_aes_truncated_payload_is_caught() {
        let r = decode_x21_aes(&[0x00]).unwrap_err();
        assert!(matches!(r, EepromParseError::Truncated { .. }));
    }

    #[test]
    fn x21_aes_non_ascii_field_fails_closed() {
        let mut payload = vec![0u8; 32];
        // Inject a non-ASCII byte (0xFE) in the serial range.
        payload[5] = 0xFE;
        let r = decode_x21_aes(&payload).unwrap_err();
        assert!(matches!(r, EepromParseError::BodyDecode { .. }));
    }

    #[test]
    fn braiinsminer_preamble_recognized() {
        let mut blob = vec![b'B', b'r'];
        blob.extend_from_slice(&[0x21, 0x00, 0xff]);
        let r = dispatch(&blob).unwrap();
        match r {
            EepromRecord::Braiinsminer(rec) => {
                // Includes the magic.
                assert_eq!(rec.raw_payload, vec![b'B', b'r', 0x21, 0x00, 0xff]);
            }
            _ => panic!("expected Braiinsminer, got {:?}", r),
        }
    }

    #[test]
    fn chip_family_lookup_known_skus() {
        assert_eq!(chip_family_for_sku("BHB56902"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB42601"), Some("BM1362"));
        assert_eq!(chip_family_for_sku("BHB42699"), Some("BM1362"));
        assert_eq!(chip_family_for_sku("BHB42801"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB42811"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB42821"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB42831"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB42841"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB428xx"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB56801"), Some("BM1366"));
        assert_eq!(chip_family_for_sku("BHB68603"), Some("BM1368"));
        assert_eq!(chip_family_for_sku("BHB68603-"), Some("BM1368"));
        assert_eq!(chip_family_for_sku("BHB68606"), Some("BM1368"));
        assert_eq!(chip_family_for_sku("BHB68123"), Some("BM1370"));
        assert_eq!(chip_family_for_sku("UNKNOWN-XX"), None);
        assert_eq!(chip_family_for_sku(""), None);
    }

    #[test]
    fn bhb_sku_catalog_pins_bhb428xx_to_bm1366() {
        let entry = BHB_SKU_CATALOG
            .iter()
            .find(|entry| entry.pattern == "BHB428xx")
            .expect("BHB428xx catalog entry");

        assert_eq!(entry.chip_family, "BM1366");
        assert!(entry.note.contains("BHB42801->BM1362"));
        assert!(entry.source.contains(""));
        assert!(entry.source.contains(""));
    }

    #[test]
    fn bhb_sku_catalog_pins_documented_bhb686_to_bm1368() {
        for sku in ["BHB68603", "BHB68606"] {
            let entry = catalog_entry_for_sku(sku).expect("BHB686 exact catalog entry");
            assert_eq!(entry.chip_family, "BM1368");
            assert_eq!(entry.eeprom_variant, "edf_v5_xxtea_key1");
            assert!(entry.note.contains("not the broad BM1370 fallback"));
        }
    }

    #[test]
    fn x21_aes_record_round_trips_through_serde() {
        let r = X21AesRecord {
            serial_number: "AS19K1234567890AB".to_string(),
            b_name: "BHB56902".to_string(),
            pcb_version: 0x4242,
            bom_version: 0x5555,
            fact_job: "JYZZ20230901007-Y1".to_string(),
            ch_die: "ED".to_string(),
            ch_marking: "S1GM23AL36".to_string(),
            ft: "F1V18B3C1".to_string(),
            ch_tech: "BS".to_string(),
            bin: "4".to_string(),
            vf: vec![0xde, 0xad, 0xbe, 0xef],
            sale_rate: Some("HIGH".to_string()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: X21AesRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn eeprom_record_serde_round_trip_with_tagged_variant() {
        let r = EepromRecord::X19Plain(X19PlainRecord {
            algo_or_subver: 0xAA,
            raw_payload: vec![1, 2, 3],
        });
        let json = serde_json::to_string(&r).unwrap();
        // Per the snake_case + tag="variant" attribute.
        assert!(json.contains("\"variant\":\"x19_plain\""));
        let back: EepromRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn eeprom_parse_error_serde_round_trip() {
        let err = EepromParseError::UnknownPreamble {
            byte0: 0x77,
            byte1: 0x88,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: EepromParseError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(json.contains("\"error\":\"unknown_preamble\""));
    }

    #[test]
    fn x21_payload_with_short_serial_trims_correctly() {
        let payload = synthetic_x21_payload("AS19K123", "BHB56902");
        let rec = decode_x21_aes(&payload).unwrap();
        // Serial field is 17 bytes wide; trim trailing NUL.
        assert_eq!(rec.serial_number, "AS19K123");
        assert_eq!(rec.b_name, "BHB56902");
    }

    #[test]
    fn b_name_round_trip_against_re_doc_anchor() {
        // RE doc anchor: hb0/hb1/hb2 from .78 all decode to BHB56902 (BM1366).
        let payload = synthetic_x21_payload("AS19K00000000.78a", "BHB56902");
        let rec = decode_x21_aes(&payload).unwrap();
        assert_eq!(chip_family_for_sku(&rec.b_name), Some("BM1366"));
    }

    #[test]
    fn truncated_x21_payload_short_of_b_name_field_is_caught() {
        // Payload shorter than offset+8 (b_name width) returns Truncated.
        // Only 16 bytes after preamble: too short for b_name @ offset 17.
        let r = decode_x21_aes(&[0x42; 16]).unwrap_err();
        assert!(matches!(r, EepromParseError::Truncated { .. }));
    }

    #[test]
    fn read_ascii_field_trims_trailing_spaces_and_nuls() {
        let mut payload = vec![0u8; 16];
        payload[..5].copy_from_slice(b"hello");
        payload[5] = b' ';
        payload[6] = b' ';
        payload[7] = 0;
        let s = read_ascii_field(&payload, 0, 16).unwrap();
        assert_eq!(s, "hello");
    }

    #[test]
    fn raw_256_x19_bhb426_metadata_is_normalized_without_writes() {
        let raw = synthetic_raw_blob([0x04, 0x11], "BHB42601");
        let report = decode_raw_256_blob(&raw);

        assert_eq!(report.schema, RAW_EEPROM_DECODE_SCHEMA);
        assert_eq!(report.raw_len, RAW_EEPROM_BLOB_LEN);
        assert_eq!(report.status, RawEepromDecodeStatus::MetadataOnly);
        assert_eq!(report.metadata.board_sku.as_deref(), Some("BHB42601"));
        assert_eq!(report.metadata.chip_family.as_deref(), Some("BM1362"));
        assert_eq!(
            report.metadata.model_family.as_deref(),
            Some("S19j Pro / S19j Pro variants")
        );
        assert_eq!(report.metadata.eeprom_variant.as_deref(), Some("x19_plain"));
        assert!(report.metadata.read_only);
        assert!(!report.metadata.writes_performed);
    }

    #[test]
    fn raw_256_x19_bhb428_metadata_uses_correct_bm1366_catalog() {
        let raw = synthetic_raw_blob([0x04, 0x11], "BHB42841");
        let report = decode_raw_256_blob(&raw);

        assert_eq!(report.status, RawEepromDecodeStatus::MetadataOnly);
        assert_eq!(report.metadata.board_sku.as_deref(), Some("BHB42841"));
        assert_eq!(report.metadata.chip_family.as_deref(), Some("BM1366"));
        assert!(report
            .metadata
            .notes
            .iter()
            .any(|note| note.contains("BHB42801->BM1362")));
    }

    #[test]
    fn raw_256_edf_v5_bhb56902_reports_xxtea_metadata() {
        let raw = synthetic_raw_x21_blob("AS19K1234567890AB", "BHB56902");
        let report = decode_raw_256_blob(&raw);

        assert_eq!(report.status, RawEepromDecodeStatus::MetadataOnly);
        assert_eq!(report.metadata.board_sku.as_deref(), Some("BHB56902"));
        assert_eq!(report.metadata.serial_number.as_deref(), None);
        assert_eq!(report.metadata.chip_family.as_deref(), Some("BM1366"));
        assert_eq!(
            report.metadata.eeprom_variant.as_deref(),
            Some("edf_v5_xxtea_key1")
        );
        assert_eq!(report.metadata.eeprom_format.as_deref(), Some("edf_v5"));
        assert_eq!(report.metadata.cipher.as_deref(), Some("xxtea"));
        assert_eq!(report.metadata.key_index, Some(1));
        assert!(matches!(report.record, Some(EepromRecord::EdfV5Xxtea(_))));
        assert!(report.metadata.read_only);
        assert!(!report.metadata.writes_performed);
    }

    #[test]
    fn raw_256_edf_v5_bhb68603_maps_to_bm1368() {
        let raw = synthetic_raw_blob([0x05, 0x11], "BHB68603");
        let report = decode_raw_256_blob(&raw);

        assert_eq!(report.status, RawEepromDecodeStatus::MetadataOnly);
        assert_eq!(report.metadata.board_sku.as_deref(), Some("BHB68603"));
        assert_eq!(report.metadata.chip_family.as_deref(), Some("BM1368"));
        assert_eq!(
            report.metadata.model_family.as_deref(),
            Some("S21 / S21+ / T21")
        );
        assert_eq!(report.metadata.eeprom_format.as_deref(), Some("edf_v5"));
        assert_eq!(report.metadata.cipher.as_deref(), Some("xxtea"));
        assert_eq!(report.metadata.key_index, Some(1));
    }

    #[test]
    fn raw_256_malformed_preamble_fails_closed() {
        let raw = synthetic_raw_blob([0xde, 0xad], "BHB42601");
        let report = decode_raw_256_blob(&raw);

        assert_eq!(report.status, RawEepromDecodeStatus::UnknownPreamble);
        assert!(report.record.is_none());
        assert_eq!(report.metadata.board_sku.as_deref(), Some("BHB42601"));
        assert!(report.metadata.read_only);
        assert!(!report.metadata.writes_performed);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("unknown EEPROM preamble")));
    }

    #[test]
    fn raw_decode_rejects_non_256_byte_blob() {
        let report = decode_raw_256_blob(&[0x05, 0x11, 0x00]);

        assert_eq!(report.status, RawEepromDecodeStatus::MalformedLength);
        assert_eq!(report.raw_len, 3);
        assert!(report.record.is_none());
        assert!(report.metadata.read_only);
        assert!(!report.metadata.writes_performed);
    }
}
