//! 9 (2026-05-09): Hashboard SKU catalog across the Antminer line.
//!
//! Source-cite: `DCENT_OS_DEVELOPMENT_KITRE2/DCENT_OS_DEVELOPMENT_KIT/`
//! `DCENT_OS_HARDWARE_CATALOG.md` §9.2 (lines 706-723) for BMU partition
//! / hashboard SKU naming, and `MASTER_DOCS/S19J_PRO_PORTING_PLAN.md` §10
//! for per-SKU freq/voltage tables. The W11.5 BM1362 SKU enum
//! (`bm1362::Bm1362HashboardSku`) is the rich version; this module is
//! the lighter cross-chip catalog used by the registry / install
//! preflight.
//!
//! ## EEPROM preamble — load-bearing identifier
//!
//! The first two bytes of an AT24C02D chain EEPROM (i2c address 0x50)
//! are a hashboard family preamble:
//! - `0x04 0x11` — BHB42xxx family (S19j Pro / S19 Pro+ — BM1362 +
//!   APW121215a).,
//!   `dcent install` BLOCKS unless the EEPROM header matches the
//!   product family.
//! - `0x05 0x11` — BHB56902 family (S19k Pro — BM1366 + BHB56902 + APW
//!   fw=0x76). this
//!   is the canonical preamble; **routing code MUST distinguish from
//!   BHB42xxx**.
//!
//! Other hashboard families (S9 / S11 / S17 BHB-class) ship pre-AT24C02D
//! EEPROMs with vendor-specific header layouts; RE2 doesn't pin
//! preamble bytes for those, so we leave them as `None`.

use serde::{Deserialize, Serialize};

/// Hashboard EEPROM preamble (first 2 bytes at I²C 0x50). `None` for
/// hashboard families where RE2 doesn't pin the preamble.
pub type EepromPreamble = Option<[u8; 2]>;

// ---------------------------------------------------------------------------
// A25 (goldmine 2026-06-10): hashboard temperature-sensor I²C addresses.
// ---------------------------------------------------------------------------
//
// From the Bitmain S21xp single-board-test jig
// `check_asic_sensor_type@272A4` (findings/s12-corpus-catalog.md D3/E5,
// cross-checked against s3-bm1370-s21pro.md F55). The jig fingerprints the
// on-board sensor by these 8-bit I²C addresses. DATA ONLY — DCENT_OS reads
// die/board temps via the per-platform thermal path, not these constants
// directly; they are recorded here for cross-SKU thermal-scan reference.

/// TMP451 / NCT218 temperature sensor — 8-bit I²C address `0x98`.
pub const TEMP_SENSOR_I2C_TMP451_NCT218: u8 = 0x98;
/// TMP411B temperature sensor — 8-bit I²C address `0x9A`.
pub const TEMP_SENSOR_I2C_TMP411B: u8 = 0x9A;
/// TMP411C temperature sensor — 8-bit I²C address `0x9C`.
pub const TEMP_SENSOR_I2C_TMP411C: u8 = 0x9C;

/// A22 (goldmine 2026-06-10): S11 (BM1391) factory single-board-test fan-curve
/// thresholds, recorded verbatim from the jig for **reference/telemetry only**.
///
/// Source: HashSource S11 `single-board-test.dec/set_fan_by_temp@29470`
/// (findings/s1-bm1391-s11.md F38). The jig seeds PWM = 100%, then for a valid
/// (min, max) ASIC-temp pair:
///   - `min_temp <= low_thresh - 1` (≤ 74 °C) → PWM ≈ 30
///   - `min_temp >  low_thresh`     (> 75 °C) → PWM = `min_temp - low_base_pwm`
///   - `max_temp >  emergency_thresh` (> 85 °C) → PWM = 100 (emergency override)
///
/// ⚠️ This is the Bitmain FACTORY jig curve, which blasts fans to 100%. It is
/// deliberately **NOT** wired into DCENT_OS's `FanController`, which caps
/// commanded PWM at 30 for home/space-heater use (cut hash power before raising
/// fan noise). Do NOT consume these values in any live fan path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct S11FanCurveThresholds {
    /// Low-temperature knee in °C (jig: 75). At/below this the jig holds a
    /// low PWM; above it the jig ramps PWM = `min_temp - low_base_pwm`.
    pub low_thresh: u8,
    /// Linear PWM offset subtracted from `min_temp` above `low_thresh`
    /// (jig: 45).
    pub low_base_pwm: u8,
    /// Emergency knee in °C (jig: 85). Above this the jig forces PWM = 100%.
    pub emergency_thresh: u8,
}

/// Canonical S11 jig fan-curve thresholds (reference data — see
/// [`S11FanCurveThresholds`]).
pub const S11_FAN_CURVE_THRESHOLDS: S11FanCurveThresholds = S11FanCurveThresholds {
    low_thresh: 75,
    low_base_pwm: 45,
    emergency_thresh: 85,
};

/// Hashboard SKU catalog row. Per-SKU freq/voltage tables remain in the
/// dedicated chip modules (`bm1362::BHB42601_FREQ_VOLT_TABLE` etc.) so
/// there's a single source of truth.
///
/// `Deserialize` is intentionally NOT derived — `used_in` borrows from
/// a `&'static` table which serde can't reconstruct from JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HashboardCatalogEntry {
    /// Bitmain SKU string (matches the `ProfileBundle.hashboard` JSON
    /// field used by the runtime registry).
    pub sku: &'static str,
    /// ASIC chip mounted on this hashboard (chip name string;
    /// references `asics::AsicChip::name()`).
    pub chip_name: &'static str,
    /// Number of chips per chain when this SKU is fitted to its
    /// canonical product. 0 when not pinned.
    pub chips_per_chain: u8,
    /// EEPROM preamble bytes when known.
    pub eeprom_preamble: EepromPreamble,
    /// Antminer products that fit this hashboard.
    pub used_in: &'static [&'static str],
}

/// Cross-chip hashboard SKU enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hashboard {
    /// **BHB42601** — S19j Pro standard. BM1362, 126 chips/chain.
    /// EEPROM `0x04 0x11`. Per-SKU freq/voltage table:
    /// `bm1362::BHB42601_FREQ_VOLT_TABLE` (5 rows, 545→465 MHz @
    /// 1320..1380 mV).
    Bhb42601,
    /// **BHB42801** — S19 Pro+ higher-grade. BM1362. EEPROM `0x04 0x11`.
    /// Per-SKU freq/voltage table: `bm1362::BHB42801_FREQ_VOLT_TABLE`
    /// (4 rows, 675→585 MHz @ 1530..1600 mV).
    Bhb42801,
    /// **BHB42611** — high-grade S19j Pro variant. BM1362. EEPROM
    /// `0x04 0x11`. Same voltage envelope as BHB42601 but freq band
    /// shifted up to 610-670 MHz.
    Bhb42611,
    // ---
    // W13.C2 (2026-05-10): RE4 levels.json full PVT-table expansion. The
    // `Hashboard` enum mirrors `bm1362::Bm1362HashboardSku` so the
    // light cross-chip catalog can refer to all 15 BHB42xxx variants by
    // name. All BHB42xxx variants share the `0x04 0x11` EEPROM
    // preamble — `classify_by_eeprom_preamble()` returns the canonical
    // `Bhb42601`; `dcent install` preflight refines via `/etc/subtype`
    // string and a separate routing helper.
    // ---
    /// **BHB42603** — Standard family alias of BHB42601. BM1362.
    Bhb42603,
    /// **BHB42621** — Standard family alias of BHB42601. BM1362.
    Bhb42621,
    /// **BHB42641** — Standard family alias of BHB42601. BM1362.
    Bhb42641,
    /// **BHB42631** — Extended-low family. 545→440 MHz @ 1320-1380 mV.
    Bhb42631,
    /// **BHB42632** — Extended-low family alias of BHB42631.
    Bhb42632,
    /// **BHB42651** — Extended-low family alias of BHB42631.
    Bhb42651,
    /// **BHB42811** — High-bin family alias of BHB42801. Requires APW12+.
    Bhb42811,
    /// **BHB42821** — High-bin family alias of BHB42801. Requires APW12+.
    Bhb42821,
    /// **BHB42831** — High-bin extended (+585 MHz row). Requires APW12+.
    Bhb42831,
    /// **BHB42803** — Single-voltage repair-class. 84 ASICs × **3 chains**.
    /// `voltage_fixed=true`. Requires APW12+.
    Bhb42803,
    /// **BHB42701** — Efficiency-optimised. 1220-1260 mV floor.
    Bhb42701,
    /// **BHB42841** — Low-power salvage. Inverted curve (freq↓ ⇒ volt↑).
    Bhb42841,
    /// **BHB56902** — S19k Pro. BM1366. EEPROM **`0x05 0x11`** (NEW
    /// family preamble, distinct from BHB42xxx). APW121215f fw=0x76.
    /// 77 chips/chain × 3 chains.
    Bhb56902,
    /// **BHB-S9-A** — generic S9 hashboard placeholder. RE2 doesn't
    /// pin a published Bitmain SKU string for the S9 hashboards;
    /// `BHB-S9-A` is the existing convention used by W7-D round-trip
    /// tests in `registry.rs`. EEPROM preamble unknown.
    BhbS9 {
        /// 0..3 — physical chain index. S9 hashboards are not all
        /// identical (silicon binning); use the chain index as a
        /// placeholder discriminator until a real per-board SKU is
        /// recovered.
        chain_index: u8,
    },
    /// **BHB-S11** — generic S11 hashboard placeholder. RE2 §2.2 lists
    /// 4 chains × 63 chips. SKU string unknown.
    BhbS11,
    /// **BHB-S17** — generic S17 hashboard placeholder. S17 = 3 chains ×
    /// 48 **BM1397** (7 nm) chips per DCENT_OS_Antminer/ HW quick-ref;
    /// the old "4 chains × ~72 BM1387" was a stale RE2 §2.4 guess. SKU string
    /// unknown.
    BhbS17,
    /// **BHB-T15** — Antminer T15 hashboard. BM1391 (7 nm), 63 chips/chain
    /// per the RE Dev Kit (findings/s20-devkit-re.md F22/IC-1; XC7Z020
    /// control board, distinct AXI/NAND geometry). SKU string unknown.
    /// Catalog/identity placeholder — no active T15 in the fleet.
    BhbT15,
}

impl Hashboard {
    /// Catalog row.
    pub const fn catalog(self) -> HashboardCatalogEntry {
        match self {
            Hashboard::Bhb42601 => HashboardCatalogEntry {
                sku: "BHB42601",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro"],
            },
            Hashboard::Bhb42801 => HashboardCatalogEntry {
                sku: "BHB42801",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19 Pro+", "S19j Pro+"],
            },
            Hashboard::Bhb42611 => HashboardCatalogEntry {
                sku: "BHB42611",
                chip_name: "BM1362",
                // BHB42611 is mid-band mixable: 120 chips/chain
                // (`pvt_tables.h` line 253). Was 126 in W11; corrected
                // to match the RE4 levels.json table.
                chips_per_chain: 120,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (high-grade)"],
            },
            // --- W13.C2 standard family aliases ---
            Hashboard::Bhb42603 => HashboardCatalogEntry {
                sku: "BHB42603",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (standard alias)"],
            },
            Hashboard::Bhb42621 => HashboardCatalogEntry {
                sku: "BHB42621",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (standard alias)"],
            },
            Hashboard::Bhb42641 => HashboardCatalogEntry {
                sku: "BHB42641",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (standard alias)"],
            },
            // --- W13.C2 extended-low family ---
            Hashboard::Bhb42631 => HashboardCatalogEntry {
                sku: "BHB42631",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (extended-low)"],
            },
            Hashboard::Bhb42632 => HashboardCatalogEntry {
                sku: "BHB42632",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (extended-low alias)"],
            },
            Hashboard::Bhb42651 => HashboardCatalogEntry {
                sku: "BHB42651",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (extended-low alias)"],
            },
            // --- W13.C2 high-bin family (REQUIRES APW12+) ---
            Hashboard::Bhb42811 => HashboardCatalogEntry {
                sku: "BHB42811",
                chip_name: "BM1362",
                chips_per_chain: 88,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19 Pro+ (high-bin alias)"],
            },
            Hashboard::Bhb42821 => HashboardCatalogEntry {
                sku: "BHB42821",
                chip_name: "BM1362",
                chips_per_chain: 88,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19 Pro+ (high-bin alias)"],
            },
            Hashboard::Bhb42831 => HashboardCatalogEntry {
                sku: "BHB42831",
                chip_name: "BM1362",
                chips_per_chain: 88,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19 Pro+ (high-bin extended)"],
            },
            // --- W13.C2 fixed-voltage repair-class (3-chain) ---
            Hashboard::Bhb42803 => HashboardCatalogEntry {
                sku: "BHB42803",
                chip_name: "BM1362",
                chips_per_chain: 84,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (repair-class, fixed-V, 3-chain)"],
            },
            // --- W13.C2 efficiency-optimised ---
            Hashboard::Bhb42701 => HashboardCatalogEntry {
                sku: "BHB42701",
                chip_name: "BM1362",
                chips_per_chain: 108,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (efficiency)"],
            },
            // --- W13.C2 low-power salvage (inverted curve) ---
            Hashboard::Bhb42841 => HashboardCatalogEntry {
                sku: "BHB42841",
                chip_name: "BM1362",
                chips_per_chain: 126,
                eeprom_preamble: Some([0x04, 0x11]),
                used_in: &["S19j Pro (low-power salvage)"],
            },
            Hashboard::Bhb56902 => HashboardCatalogEntry {
                sku: "BHB56902",
                chip_name: "BM1366",
                // 77 here is INTENTIONAL — it matches (a) the live driver
                // default `dcentrald-asic::drivers::bm1366::DEFAULT_CHIPS_PER_CHAIN_S19K
                // = 77` (the actual mining path) and (b) the live probe of `a lab unit`
                // (: "77 chips/chain
                // × 3 chains", pinned by `bhb56902_uses_bm1366_77_chips` below).
                // The richer silicon-profile SKU table
                // `bm1366::Bm1366HashboardSku::S19kPro.chips_per_chain()` deliberately
                // pins 76 instead — the HASHCOUNTING-register-encoded count (`0x115A`),
                // documented at `bm1366.rs` ~L299 ("the driver default rounds to 77.
                // We pin the HASHCOUNTING-derived 76 here"). The 76-vs-77 gap is a
                // KNOWN, by-design divergence (HASHCOUNTING-encoded 76 vs physical/
                // driver/live-probe 77), NOT drift — do NOT "reconcile" this display
                // value down to 76; that would contradict the driver and the live
                // probe. Display/telemetry only — no enum/safety consumer reads it.
                chips_per_chain: 77,
                // NEW preamble per W11.9 / Phase O.3
                //.
                eeprom_preamble: Some([0x05, 0x11]),
                used_in: &["S19k Pro"],
            },
            Hashboard::BhbS9 { .. } => HashboardCatalogEntry {
                sku: "BHB-S9",
                chip_name: "BM1387",
                chips_per_chain: 63,
                eeprom_preamble: None,
                used_in: &["S9", "S9i"],
            },
            Hashboard::BhbS11 => HashboardCatalogEntry {
                sku: "BHB-S11",
                chip_name: "BM1391",
                // 84 chips/chain — byte-exact from the S11 single-board-test
                // jig (`board_init@1338C.c` loops until count == 84).
                // The previous 63 was a copy-paste from the BM1387 BhbS9
                // entry (S9 = 63). HashSource goldmine 2026-06-10. No active
                // S11 in the fleet, so this is catalog/telemetry-only today.
                chips_per_chain: 84,
                eeprom_preamble: None,
                used_in: &["S11"],
            },
            Hashboard::BhbS17 => HashboardCatalogEntry {
                sku: "BHB-S17",
                // Chip CORRECTED BM1387 -> BM1397 (2026-07-02): the Antminer
                // S17 / S17 Pro use the 7nm BM1397 (chip-ID 0x1397), NOT the
                // S9's 16nm BM1387. The prior "BM1387" + "4 chains x ~72" was a
                // stale RE2 §2.4 placeholder. Geometry corrected to 3 chains x
                // 48 BM1397 per DCENT_OS_Antminer/ HW quick-ref
                // ("S17: Same Zynq ... 3x48 BM1397") + bm1397.rs. (S17 Pro
                // binning may differ; this catalog row is telemetry-only.)
                chip_name: "BM1397",
                chips_per_chain: 48,
                eeprom_preamble: None,
                used_in: &["S17", "S17 Pro"],
            },
            Hashboard::BhbT15 => HashboardCatalogEntry {
                sku: "BHB-T15",
                chip_name: "BM1391",
                // 63 chips/chain per the RE Dev Kit T15 board config
                // (findings/s20-devkit-re.md F22). Distinct geometry from
                // the BhbS11 BM1391 board (84/chain). Catalog-only.
                chips_per_chain: 63,
                eeprom_preamble: None,
                used_in: &["T15"],
            },
        }
    }

    /// SKU string.
    pub const fn sku(self) -> &'static str {
        self.catalog().sku
    }
}

/// Look up a hashboard entry by EEPROM preamble. Used by `dcent install`
/// preflight to decide which platform/chip family to route through.
/// Returns the first match; preambles are intentionally non-overlapping
/// across the BHB42xxx and BHB56902 families.
pub fn classify_by_eeprom_preamble(preamble: [u8; 2]) -> Option<Hashboard> {
    // We can't iterate enum variants in a const fn, so spell out the
    // preamble→variant mapping. Order matters for readability only —
    // the preambles don't collide.
    if preamble == [0x04, 0x11] {
        // The BHB42xxx family shares a preamble; we return the
        // canonical S19j Pro standard SKU and let the caller (which
        // also has the levels.json table or a subtype string) refine
        // to BHB42801 / BHB42611.
        return Some(Hashboard::Bhb42601);
    }
    if preamble == [0x05, 0x11] {
        return Some(Hashboard::Bhb56902);
    }
    None
}

/// Catalog of all known hashboard SKUs (excluding the per-chain
/// `BhbS9 { chain_index }` enumeration which is parameterized).
///
/// W13.C2 (2026-05-10): expanded from 7 → 19 entries. The 12 new
/// BHB42xxx variants per `pvt_tables.h` round out the full RE4 PVT
/// table coverage.
pub const ALL_HASHBOARDS: &[Hashboard] = &[
    // BHB42xxx family — 15 SKUs.
    Hashboard::Bhb42601,
    Hashboard::Bhb42603,
    Hashboard::Bhb42621,
    Hashboard::Bhb42641,
    Hashboard::Bhb42631,
    Hashboard::Bhb42632,
    Hashboard::Bhb42651,
    Hashboard::Bhb42801,
    Hashboard::Bhb42811,
    Hashboard::Bhb42821,
    Hashboard::Bhb42831,
    Hashboard::Bhb42803,
    Hashboard::Bhb42611,
    Hashboard::Bhb42701,
    Hashboard::Bhb42841,
    // Other families.
    Hashboard::Bhb56902,
    Hashboard::BhbS11,
    Hashboard::BhbS17,
    // A55 (BhbT15, BM1391 @ 63 chips/chain — findings/s20-devkit-re.md F22/IC-1).
    // Landed 2026-06-10 together with the matching `| Hashboard::BhbT15` arms in
    // `dcentrald-autotuner::pvt_envelope::hashboard_to_bm1362_sku` (→ None, not a
    // BM1362 board) and `dcentrald::runtime::hardware_info::
    // pic_type_for_classified_sku` (→ None, placeholder like BhbS11).
    Hashboard::BhbT15,
    // BhbS9 is parameterized by chain index; pin chain 0 here for
    // catalog completeness.
    Hashboard::BhbS9 { chain_index: 0 },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bhb56902_eeprom_preamble_is_05_11() {
        //: BHB56902
        // ships the NEW EEPROM preamble `0x05 0x11`, distinct from the
        // BHB42xxx family's `0x04 0x11`.
        let cat = Hashboard::Bhb56902.catalog();
        assert_eq!(cat.eeprom_preamble, Some([0x05, 0x11]));
    }

    #[test]
    fn bhb42xxx_share_preamble_04_11() {
        // W13.C2: ALL 15 BHB42xxx variants carry the BHB42xxx-family
        // EEPROM preamble. Adding a future BHB42xxx SKU MUST keep this
        // invariant — `dcent install` preflight depends on it.
        for hb in [
            Hashboard::Bhb42601,
            Hashboard::Bhb42603,
            Hashboard::Bhb42621,
            Hashboard::Bhb42641,
            Hashboard::Bhb42631,
            Hashboard::Bhb42632,
            Hashboard::Bhb42651,
            Hashboard::Bhb42801,
            Hashboard::Bhb42811,
            Hashboard::Bhb42821,
            Hashboard::Bhb42831,
            Hashboard::Bhb42803,
            Hashboard::Bhb42611,
            Hashboard::Bhb42701,
            Hashboard::Bhb42841,
        ] {
            assert_eq!(hb.catalog().eeprom_preamble, Some([0x04, 0x11]));
        }
    }

    #[test]
    fn bhb42xxx_and_bhb56902_preambles_do_not_collide() {
        // Pin: the routing rule depends on the two preambles being
        // distinct. If a future refactor lined them up, install
        // preflight would route an S19k Pro into the S19j Pro path.
        assert_ne!(
            Hashboard::Bhb42601.catalog().eeprom_preamble,
            Hashboard::Bhb56902.catalog().eeprom_preamble
        );
    }

    #[test]
    fn classify_by_eeprom_preamble_routes_correctly() {
        // BHB42xxx → return the BHB42601 canonical SKU; caller refines.
        assert_eq!(
            classify_by_eeprom_preamble([0x04, 0x11]),
            Some(Hashboard::Bhb42601)
        );
        // BHB56902 → unique route.
        assert_eq!(
            classify_by_eeprom_preamble([0x05, 0x11]),
            Some(Hashboard::Bhb56902)
        );
        // Unknown preambles → None (caller must error out).
        assert_eq!(classify_by_eeprom_preamble([0x00, 0x00]), None);
        assert_eq!(classify_by_eeprom_preamble([0xFF, 0xFF]), None);
    }

    #[test]
    fn s19jpro_hashboards_use_bm1362() {
        // W13.C2: every BHB42xxx variant mounts BM1362.
        for hb in [
            Hashboard::Bhb42601,
            Hashboard::Bhb42603,
            Hashboard::Bhb42621,
            Hashboard::Bhb42641,
            Hashboard::Bhb42631,
            Hashboard::Bhb42632,
            Hashboard::Bhb42651,
            Hashboard::Bhb42801,
            Hashboard::Bhb42811,
            Hashboard::Bhb42821,
            Hashboard::Bhb42831,
            Hashboard::Bhb42803,
            Hashboard::Bhb42611,
            Hashboard::Bhb42701,
            Hashboard::Bhb42841,
        ] {
            assert_eq!(hb.catalog().chip_name, "BM1362");
        }
    }

    #[test]
    fn bhb56902_uses_bm1366_77_chips() {
        // Per the S19k Pro probe (memory rule
        // ): BHB56902
        // mounts BM1366 with 77 chips/chain × 3 chains.
        let cat = Hashboard::Bhb56902.catalog();
        assert_eq!(cat.chip_name, "BM1366");
        assert_eq!(cat.chips_per_chain, 77);
    }

    #[test]
    fn legacy_hashboards_have_no_pinned_preamble() {
        // BHB-S9 / BHB-S11 / BHB-S17 — RE2 doesn't pin preambles, so
        // the catalog must not lie about them.
        for hb in [
            Hashboard::BhbS9 { chain_index: 0 },
            Hashboard::BhbS11,
            Hashboard::BhbS17,
        ] {
            assert_eq!(hb.catalog().eeprom_preamble, None);
        }
    }

    #[test]
    fn all_hashboards_present() {
        // W13.C2: 15 BHB42xxx + BHB56902 + 3 legacy = 19; +BhbT15 (A55,
        // goldmine 2026-06-10) = 20 catalog entries.
        assert_eq!(ALL_HASHBOARDS.len(), 20);
    }

    #[test]
    fn temp_sensor_i2c_addresses_pinned() {
        // A25 (goldmine 2026-06-10): S21xp jig check_asic_sensor_type@272A4
        // (findings/s12-corpus-catalog.md D3/E5). DATA ONLY.
        assert_eq!(TEMP_SENSOR_I2C_TMP451_NCT218, 0x98);
        assert_eq!(TEMP_SENSOR_I2C_TMP411B, 0x9A);
        assert_eq!(TEMP_SENSOR_I2C_TMP411C, 0x9C);
    }

    #[test]
    fn s11_fan_curve_thresholds_pinned() {
        // A22 (goldmine 2026-06-10): S11 jig set_fan_by_temp@29470
        // (findings/s1-bm1391-s11.md F38). REFERENCE DATA ONLY — never wired
        // into the DCENT FanController (PWM-30 home cap stands).
        assert_eq!(S11_FAN_CURVE_THRESHOLDS.low_thresh, 75);
        assert_eq!(S11_FAN_CURVE_THRESHOLDS.low_base_pwm, 45);
        assert_eq!(S11_FAN_CURVE_THRESHOLDS.emergency_thresh, 85);
    }

    #[test]
    fn each_hashboard_sku_string_is_unique() {
        let mut seen = std::collections::HashSet::new();
        for hb in ALL_HASHBOARDS {
            assert!(
                seen.insert(hb.sku()),
                "duplicate hashboard SKU string: {}",
                hb.sku()
            );
        }
    }
}
