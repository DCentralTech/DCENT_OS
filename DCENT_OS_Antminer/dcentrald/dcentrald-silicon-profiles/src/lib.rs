//! Canonical silicon characterization tables for the ASIC families
//! DCENT_OS supports.
//!
//! Each table is a discrete, integer-step `Profile` index pairing target
//! frequency (MHz), chain voltage (V), expected wall power (W, all boards),
//! and expected nameplate hashrate (TH/s, all boards). The tables are derived
//! from live `cgminer-API profiles` output captures of competitor firmware
//! (LuxOS, BraiinsOS+) and reverse-engineering notes in
//!  and .
//!
//! Every row is labeled `live-confirmed` (lifted verbatim from a live capture)
//! or `reconstructed` (linear-extrapolated from the cadence proven by
//! confirmed rows). Reconstructed rows must be re-verified the next time a
//! session has live API access — see the source RE document for the
//! re-verification checklist.
//!
//! This crate is pure data + light helpers. It has zero HAL dependency so
//! the autotuner, dashboard, and toolbox can all consume the same canonical
//! profile tables.
//!
//! Provenance / source documents:
//! - BM1362:
//!   (LUXminer 2026.4.3.192353 on Antminer S19j Pro at 203.0.113.79).
//!
//! Other ASIC families (BM1366/BM1368/BM1387/BM1398) are stubs in this
//! version pending live capture from a target on each silicon.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// 9 (2026-05-09): full ASIC chip catalog (10 chips spanning
/// BM1387 → BM1368). Lightweight metadata only — per-SKU geometry and
/// freq/voltage tables remain in the chip-specific `bm13xx.rs` modules.
pub mod asics;
///  W7-A: BM1360 silicon profile.
///
/// **Status until W7.4 (2026-05-07): NAMED ONLY** placeholder (per
///  memory rule). **W7.4
/// (2026-05-07):** populated with PLL register data lifted from
///  (BM1366+ encoding family). Every
/// numeric row is still `Reconstructed`; no live BM1360 unit yet.
/// Live status: `ChipStatus::RegisterMappedFromRE`. See module docs.
///
/// Gated behind the `experimental_chips` Cargo feature so the
/// production registry does not silently route a BM1360 detection to
/// register-mapped-only fallback values; lab/research builds only.
#[cfg(feature = "experimental_chips")]
pub mod bm1360;
pub mod bm1362;
///  silicon-C: BM1366 (Antminer S19k Pro / S19 XP) silicon profile.
pub mod bm1366;
///  silicon-D: BM1368 (Antminer S21 / S21 Hydro — 5nm) silicon profile.
pub mod bm1368;
///  silicon-E: BM1370 (Antminer S21 Pro / S21 XP — 3nm) silicon profile.
pub mod bm1370;
///  W7.4: BM1373 silicon profile (Antminer S23 — pre-hardware).
///
/// BM1373 does NOT appear in the  RE chip-name inventory (see
/// `wave6-vnish-decrypted/CHIP_INVENTORY.md` — 8 names, BM1373 not
/// among them). The chip name comes from `general/BM1373_S23_RESEARCH.md`
/// (internal intel 2026-04-14, all values projected from BM1370). The
/// driver scaffold in `dcentrald-asic/src/drivers/bm1373.rs` is keyed on
/// `0x1373`. This silicon profile mirrors its register-mapped projection.
///
/// Gated behind `experimental_chips`.
#[cfg(feature = "experimental_chips")]
pub mod bm1373;
///  tune-D: BM1387 (Antminer S9 / S9i / T9) silicon profile.
pub mod bm1387;
///  scaffold: BM1391 (S11/S15/T15) data-only geometry profile.
pub mod bm1391;
///  scaffold: BM1396 (S17+/T17+) data-only profile.
pub mod bm1396;
///  silicon-A: BM1397 (Antminer S17 / S19 / S19j) silicon profile.
pub mod bm1397;
///  silicon-B: BM1398 (Antminer S19 / S19 Pro) silicon profile.
pub mod bm1398;
///  tune-E: BM1485 (Antminer L3 / L3+, Litecoin scrypt) silicon profile.
pub mod bm1485;
///  tune-F: BM1489 (Antminer L7, Litecoin scrypt) silicon profile.
pub mod bm1489;
///  W7-A: BM1491 silicon profile.
///
/// **Status until W7.4 (2026-05-07): NAMED ONLY** placeholder. **W7.4
/// (2026-05-07):** marked Scrypt-family (per `BM1373_S23_RESEARCH.md`
/// +  §10.1 — BM1491 sits adjacent to BM1489
/// in the VNish chip-name array, both Scrypt-class). Register
/// addresses still `[GAP]` (the 7 cgminer ELFs that mention BM1491
/// don't expose its registers in plain text — XOR-sealed); only the
/// Scrypt family classification + a placeholder PLL table shape is
/// `RegisterMappedFromRE`. Numeric rows remain `Reconstructed`.
///
/// Gated behind `experimental_chips` so production builds don't
/// route an unknown BM1491 detection to placeholder values.
#[cfg(feature = "experimental_chips")]
pub mod bm1491;
pub mod efficiency;
///  B2 (2026-05-22): runtime energize-refusal gate (drive-half
/// of matrix §7 #15). Classifies per-chain EEPROM preamble bytes into
/// a `ChainProbe`, then folds the platform's chains into a fail-closed
/// `SkuBinding` set OR an `EnergizeRefusal`. Called from each mining
/// path BEFORE any `set_voltage` / `enable_voltage` / `cold_boot_init`
/// write on a chain. HAL-free (pure data + classifier); env-gated
/// strictness (`DCENT_AM2_STRICT_SKU_REFUSE=1`, default OFF for
/// first-deploy telemetry-only rollout).
pub mod energize_gate;
///  tune-A: BraiinsOS GDTUNER state machine port.
pub mod gdtuner;
/// 9: per-control-board GPIO maps (CV1835 / AM335x / Amlogic /
/// Zynq / Braiins BBB).
pub mod gpio_maps;
/// 9: hashboard SKU catalog (BHB42601/42801/42611 + BHB56902 +
/// legacy BHB-S9/S11/S17 placeholders).
pub mod hashboards;
/// Per-model power/efficiency operating-point catalog (S9 → S21).
///
/// Harvested + cited companion to the per-chip `SiliconTable` step ladder.
/// Holds dense, per-MODEL freq-vs-volt-vs-watt operating points (per-board +
/// per-unit + J/TH) with explicit MEASURED / INFERRED / GAP provenance so a
/// gap is never silently treated as fact. Additive: does NOT touch
/// `Profile` / `SiliconTable` or the autotuner `c_eff` power model.
pub mod operating_points;
/// W15.A4: PIC16F1704 CRC8 polynomial catalog (HAL-free).
pub mod pic1704_crc;
/// 6: per-(Platform, PicFw) heartbeat matrix.
///
/// Single source of truth for PIC / dsPIC / NoPic-DAC heartbeat timing.
/// Companion doc: .
pub mod pic_heartbeat;
/// 9: PIC microcontroller catalog (dsPIC33EP16GS202 + PIC1704
/// + S21 Amlogic NoPic sentinel).
pub mod pics;
/// 9: PSU catalog (15 PSUs from APW3++ → APW12+ → APW121215a).
pub mod psus;
///  W5-A: runtime profile registry + JSON-bundle loader.
///
/// Owns the disk-backed profile catalog at `/etc/dcentrald/profiles.d/`.
/// See `plans/wave4-profile-import-infrastructure.md` §B for the spec.
pub mod registry;
///  tune-C: staggered chain power-up planner.
pub mod staggered_powerup;

pub use registry::{
    global, ProfileBundle, ProfileLoadError, ProfileMetadata, ProfileRegistry,
    ProfileSourceMetadata, ReloadStats,
};

pub use pic_heartbeat::{
    pic_heartbeat_config, HeartbeatConfig, PicFw, Platform as PicHeartbeatPlatform,
};

pub use operating_points::{
    by_chip_id as model_power_profiles_for_chip, by_model as model_power_profile,
    measured_anchor_for as measured_power_anchor_for, Cooling, ModelPowerProfile, OperatingPoint,
    PointConfidence, ALL_MODELS as ALL_MODEL_POWER_PROFILES,
};

/// W7.4 (2026-05-07): Live-status classification for a chip silicon
/// table as a whole. Distinct from `ProfileSource`, which labels the
/// **numeric values** in a single row. `ChipStatus` labels the
/// **driver-readiness state** of the entire chip family — does
/// dcentrald have register addresses, has it driven the chip on real
/// hardware, etc.
///
/// Authority hierarchy (highest first):
///   `LiveConfirmed` (5) > `RegisterMappedFromRE` (3) >
///   `NamedOnly` (1) > `Speculation` (0).
///
/// `RegisterMappedFromRE` means register addresses + PLL formula
/// + cores-per-chip have been lifted verbatim from a reverse-
///   engineered binary or vendor extraction, but the chip family has
///   **never been driven on real hardware** in dcentrald. Consumers
///   MUST refuse to mine on this chip family by default; the
///   `experimental_chips` Cargo feature flag is the lab override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChipStatus {
    /// Chip name surfaced in firmware / RE / datasheets but no
    /// register addresses, no PLL formula, no cores-per-chip
    /// recovered. Lowest authority. Routing here MUST refuse to mine
    /// and surface a diagnostic.
    NamedOnly,
    /// Like `NamedOnly` but with extra speculative numeric values
    /// from comparable chips. Reserved for future use.
    Speculation,
    /// Register addresses + PLL formula + cores-per-chip are recovered
    /// from RE (cgminer ELF, datasheet, or vendor extraction), but
    /// the chip has NEVER been driven by dcentrald on real hardware.
    /// Numeric profile rows can still be `Reconstructed`. Lab/research
    /// builds only — the `experimental_chips` feature flag is the
    /// gate. Production registries (no feature flag) skip these
    /// modules entirely.
    RegisterMappedFromRE,
    /// dcentrald has driven the chip family on a real hardware unit
    /// (first-hash logged, nonces accepted by a pool). Highest
    /// authority. Routes through the production registry, no feature
    /// flag required.
    LiveConfirmed,
}

impl ChipStatus {
    /// Authority rank (higher = higher authority).
    pub fn rank(&self) -> u8 {
        match self {
            ChipStatus::LiveConfirmed => 5,
            ChipStatus::RegisterMappedFromRE => 3,
            ChipStatus::NamedOnly => 1,
            ChipStatus::Speculation => 0,
        }
    }

    /// `true` only when the chip family has been hashed on real
    /// hardware. Use in driver dispatch to refuse mining on
    /// register-mapped-only chips by default.
    pub fn is_production_ready(&self) -> bool {
        matches!(self, ChipStatus::LiveConfirmed)
    }
}

/// W7.4 (2026-05-07): Voltage-controller family for a chip silicon
/// table. Mirrors `dcentrald-asic::drivers::PicType` and
/// `dcentrald-hal::platform::config::PicType`, but lives here so the
/// silicon-profiles crate can stay HAL-free (zero kernel/sysfs
/// imports). Consumers in `dcentrald-asic` translate via a `From`
/// adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PicKind {
    /// PIC16F1704 — S9 hash boards (8-bit DAC, ~8-9.4 V range).
    Pic16F1704,
    /// dsPIC33EP16GS202 — S17/S19/S19j Pro hash boards (16-bit DSP,
    /// millivolt precision, ~12-15 V range, framed protocol).
    DsPic33Ep,
    /// No PIC microcontroller — S21/S21 Pro hash boards (TAS5782M
    /// audio DACs repurposed as voltage regulators).
    NoPic,
    /// Voltage controller identity unknown — chip family is
    /// register-mapped-from-RE only and the controller has not been
    /// confirmed live. Routes that can branch on this MUST refuse
    /// voltage commands.
    Unknown,
}

/// W7.4 (2026-05-07): A single PLL-table row pairing a target
/// frequency in MHz with its 4-byte register encoding (BM1366+
/// family byte-segmented PLL register, see
///  §4.2).
///
/// Byte layout (big-endian):
///   - Byte 0: VCO_SCALE (0x40 if VCO < 2400 MHz; 0x50 if ≥ 2400 MHz)
///   - Byte 1: FBDIV (feedback divider)
///   - Byte 2: REFDIV (reference divider)
///   - Byte 3: POSTDIV encoded as `((POSTDIV1-1) << 4) | (POSTDIV2-1)`
///
/// For BM1387/BM1397/BM1398 (raw POSTDIV / single u32 / bit 30 PLLEN)
/// the encoding is fundamentally different — those chips don't share
/// this table type. Table consumers MUST dispatch on chip ID before
/// computing the PLL register.
pub type PllRegEntry = (u16, [u8; 4]);

/// Provenance label for a single profile row.
///
/// Source-class hierarchy (highest authority first), per
/// `plans/wave4-profile-import-infrastructure.md` §A:
///   `LiveConfirmed` (5) > `OperatorConfirmed` (4) > `VendorExtracted` (3) >
///   `Reconstructed` (2) > `Datasheet` (1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    /// Lifted verbatim from a live `cgminer-API profiles` capture.
    LiveConfirmed,
    /// Linear-extrapolated from the cadence proven by live-confirmed rows.
    /// Must be re-verified on next live API session.
    Reconstructed,
    /// Operator-confirmed from direct observation but not in the
    /// captured `profiles` API output (see source doc anchor rows).
    OperatorConfirmed,
    /// Pulled verbatim from a stock vendor firmware (Bitmain `levels.json`,
    /// WhatsMiner UCI, VNish cgminer ELF resource blob, etc.). What
    /// wave-4 catalogs deliver.
    VendorExtracted,
    /// Datasheet-only — never observed live. Lowest authority.
    Datasheet,
}

impl ProfileSource {
    /// Rank within the source-class hierarchy. Higher = higher authority.
    /// Used by `ProfileRegistry::merge` to resolve same-key conflicts.
    pub fn rank(&self) -> u8 {
        match self {
            ProfileSource::LiveConfirmed => 5,
            ProfileSource::OperatorConfirmed => 4,
            ProfileSource::VendorExtracted => 3,
            ProfileSource::Reconstructed => 2,
            ProfileSource::Datasheet => 1,
        }
    }
}

/// A single silicon characterization profile row.
///
/// Field names track LuxOS's `ProfileSpec` Rust struct so pyasic-compat
/// clients consuming `/api/profiles` see identical shape.
///
/// ** W7-D**: `wall_watts` and `hashrate_ths` are `Option<...>` so
/// vendor-extracted profiles (e.g. Bitmain `levels.json`, which only
/// exposes freq + voltage) can land cleanly without faking watt/hashrate
/// numbers. Per W5-D round-trip blocker: vendor JSONs serialize these as
/// `null`, baked profiles serialize them as `{watts}` / `{hashrate}`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Discrete index along the silicon characterization curve.
    /// Higher = more freq + more voltage + more watts + more hashrate.
    pub step: i32,

    /// Target chain frequency in MHz (identical across chains in
    /// single-voltage mode).
    pub freq_mhz: u32,

    /// Target chain voltage in volts. APW PSU + dsPIC controllers achieve
    /// only ~0.03 V granularity in practice; treat this as a target,
    /// not an absolute.
    pub voltage_v: f32,

    /// Expected wall power (AC-side) in watts, summed across all boards.
    /// `None` for vendor rows that don't expose watts (Bitmain
    /// `levels.json` only ships freq + voltage). Autotuner consumers
    /// MUST distinguish `None` (unknown — recompute via `power_model`)
    /// from `Some(0)` (asserted-zero).
    pub wall_watts: Option<u32>,

    /// Expected nameplate hashrate in TH/s, summed across all boards.
    /// `None` for vendor rows that don't expose hashrate.
    pub hashrate_ths: Option<f32>,

    /// Where this row's numbers came from.
    pub source: ProfileSource,
}

impl Profile {
    /// Wall efficiency in J/TH (lower = more efficient).
    ///
    /// Returns `None` for zero/unknown hashrate or unknown wall watts
    /// (avoids divide-by-zero and avoids fake-plausible numbers from
    /// vendor `null` values).
    pub fn watts_per_ths(&self) -> Option<f32> {
        let watts = self.wall_watts? as f32;
        let ths = self.hashrate_ths?;
        if ths <= 0.0 {
            return None;
        }
        Some(watts / ths)
    }

    /// LuxOS-style profile name. Step 0 has the special name `default`;
    /// every other step is named `<freq>MHz` (e.g. `145MHz`, `645MHz`).
    pub fn profile_name(&self) -> String {
        if self.step == 0 {
            "default".to_string()
        } else {
            format!("{}MHz", self.freq_mhz)
        }
    }

    /// Estimated heat output in BTU/h. The BM1362 turns essentially all
    /// wall power into heat (mining hardware is a near-100% resistive
    /// heater because the work products themselves are zero-mass).
    /// 1 watt ≈ 3.412 BTU/h.
    ///
    /// Returns `None` when `wall_watts` is unknown (vendor-extracted
    /// row with no watts data).
    pub fn heat_btu_per_hour(&self) -> Option<f32> {
        Some(self.wall_watts? as f32 * 3.412)
    }
}

/// A complete silicon characterization table for one chip family.
#[derive(Debug, Clone, Copy)]
pub struct SiliconTable {
    /// Chip family identifier (e.g. `"BM1362"`).
    pub chip_family: &'static str,

    /// The discrete profile rows, ordered by `step`.
    pub profiles: &'static [Profile],

    /// Index of the nameplate / `default` profile (typically `step == 0`).
    pub default_step: i32,

    /// Step where the silicon shows its best J/TH efficiency.
    pub sweet_spot_step: i32,

    /// W7.4 (2026-05-07): Driver-readiness state of this chip family.
    /// `LiveConfirmed` for chips that have been hashed on real hardware
    /// (BM1387/BM1362/BM1366/BM1368/BM1397/BM1398); `RegisterMappedFromRE`
    /// for chips whose register addresses are RE'd but no live unit yet
    /// (BM1360/BM1373); `NamedOnly` for chips known only as a string in
    /// vendor binaries (BM1491). Distinct from the per-row
    /// `Profile.source` provenance label.
    pub live_status: ChipStatus,
}

impl SiliconTable {
    /// Look up a profile by step index. Returns `None` if the step is
    /// outside this silicon's valid range.
    pub fn by_step(&self, step: i32) -> Option<&'static Profile> {
        self.profiles.iter().find(|p| p.step == step)
    }

    /// Look up a profile by its LuxOS-style name (`"default"` for step 0,
    /// `"<freq>MHz"` for everything else).
    ///
    /// Returns `None` if the name doesn't match any row.
    pub fn by_name(&self, name: &str) -> Option<&'static Profile> {
        self.profiles.iter().find(|p| p.profile_name() == name)
    }

    /// The default / nameplate profile.
    pub fn default_profile(&self) -> Option<&'static Profile> {
        self.by_step(self.default_step)
    }

    /// The most efficient profile in this table (lowest J/TH).
    pub fn sweet_spot_profile(&self) -> Option<&'static Profile> {
        self.by_step(self.sweet_spot_step)
    }

    /// Lowest valid step.
    pub fn min_step(&self) -> i32 {
        self.profiles.iter().map(|p| p.step).min().unwrap_or(0)
    }

    /// Highest valid step.
    pub fn max_step(&self) -> i32 {
        self.profiles.iter().map(|p| p.step).max().unwrap_or(0)
    }

    /// Returns the row with the lowest J/TH efficiency (best efficiency).
    /// Computed at call time, not pre-baked, so a table rebuild that adds
    /// new live-confirmed rows updates the answer automatically.
    pub fn computed_sweet_spot(&self) -> Option<&'static Profile> {
        self.profiles
            .iter()
            .filter(|p| p.watts_per_ths().is_some())
            .min_by(|a, b| {
                a.watts_per_ths()
                    .unwrap()
                    .partial_cmp(&b.watts_per_ths().unwrap())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profile(step: i32, watts: u32, ths: f32) -> Profile {
        Profile {
            step,
            freq_mhz: 545,
            voltage_v: 13.8,
            wall_watts: Some(watts),
            hashrate_ths: Some(ths),
            source: ProfileSource::LiveConfirmed,
        }
    }

    #[test]
    fn watts_per_ths_returns_none_for_zero_hashrate() {
        let p = sample_profile(0, 1000, 0.0);
        assert!(p.watts_per_ths().is_none());
    }

    #[test]
    fn watts_per_ths_computes_efficiency() {
        let p = sample_profile(0, 3126, 105.8);
        let eff = p.watts_per_ths().unwrap();
        // 3126 / 105.8 = 29.546...
        assert!((eff - 29.55).abs() < 0.01);
    }

    #[test]
    fn profile_name_step_zero_is_default_word() {
        let p = sample_profile(0, 1000, 50.0);
        assert_eq!(p.profile_name(), "default");
    }

    #[test]
    fn profile_name_nonzero_step_uses_frequency() {
        let mut p = sample_profile(-9, 1000, 50.0);
        p.freq_mhz = 320;
        assert_eq!(p.profile_name(), "320MHz");
    }

    #[test]
    fn heat_btu_per_hour_uses_canonical_conversion() {
        let p = sample_profile(0, 1000, 50.0);
        let btu = p.heat_btu_per_hour().expect("watts known");
        // 1000 W * 3.412 = 3412 BTU/h.
        assert!((btu - 3412.0).abs() < 0.01);
    }

    #[test]
    fn heat_btu_per_hour_returns_none_when_watts_unknown() {
        // W7-D round-trip: vendor row with null wall_watts must not
        // synthesize a fake BTU number.
        let p = Profile {
            step: 0,
            freq_mhz: 545,
            voltage_v: 1.34,
            wall_watts: None,
            hashrate_ths: None,
            source: ProfileSource::VendorExtracted,
        };
        assert!(p.heat_btu_per_hour().is_none());
        assert!(p.watts_per_ths().is_none());
    }
}
