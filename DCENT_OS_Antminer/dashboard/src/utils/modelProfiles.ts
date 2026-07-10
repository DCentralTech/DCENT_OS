// Dashboard-side model profiles — KPI/BTU/mode rendering constants.
//
// Source-of-truth is the firmware's `ModelExtendedSpec` table in
// `DCENT_OS_Antminer/dcentrald/dcentrald/src/model.rs`. Dashboard values
// MUST mirror the firmware constants. Prefer sourcing from the live API
// (`/api/system/info`) when available; these profiles are the fallback
// for offline rendering, setup wizard preview, and profitability
// estimates before the miner reports `systemInfo`.
//
// Convention: modes are "heater" | "standard"
// | "hacker" (rendered as "Space Heater" / "Mining" / "Hacker"). Never
// introduce new mode keys here; mode UI copy lives in
// `utils/constants.ts :: MODE_DESCRIPTIONS`.
//
// Convention: BTU/h is always displayed —
// every miner is a heater. Use `rated_btu_per_hour` (also exposed as a
// function for arbitrary wall-watts).
//
// Convention: hardcoded
// dashboard lists drift from the firmware truth. Each profile here has a
// review-anchor comment pointing to the firmware variant it mirrors; bump
// both sides together on any firmware spec update.

import type { SupportTier } from '../api/generated/capability';

// BTU/h per watt (IEEE / U.S. convention: 1 W = 3.412142 BTU/h). This is the
// ONE canonical BTU/h-per-watt constant per the Terminology Lexicon (TERM-3
// §3.3) — identical to thermal.ts :: WATTS_TO_BTU. The precomputed
// ratedBtuPerHour integers below are unchanged by the extra precision (the
// rounded result is identical for every model profile).
const BTU_PER_WATT = 3.412142;

/**
 * Rendering-oriented model profile. Mirrors a subset of the firmware
 * `ModelExtendedSpec` (`DCENT_OS_Antminer/dcentrald/dcentrald/src/model.rs`).
 */
export interface ModelProfile {
  /** Canonical firmware model_key (matches `Model::model_key()` in Rust). */
  modelKey: string;
  /** Human display name for chrome / About / wizard. */
  displayName: string;
  /** Short human platform label (e.g. "Zynq am2", "Amlogic A113D"). */
  platformDisplay: string;
  /** Firmware platform key (matches `Model::platform_key()`). */
  platformKey: string;
  /** ASIC chip label (e.g. "BM1362"). */
  chip: string;
  /** Populated chains on a stock unit. */
  chainCount: number;
  /** ASIC chips per populated chain, when the firmware/research table proves it. */
  chipCountPerChain: number | null;
  /** Rated full-unit hashrate in TH/s (Bitmain datasheet). */
  ratedHashrateTh: number;
  /** Rated full-unit wall power in watts (Bitmain datasheet). */
  ratedPowerW: number;
  /**
   * Thermal design label — the marketing wattage figure surfaced in the
   * heater pages. Separate from `ratedPowerW` so future variants can
   * distinguish "rated" vs "ceiling" numbers if needed.
   */
  thermalDesign: string;
  /** Default conservative mining frequency (MHz), when promotion evidence exists. */
  defaultFrequencyMhz: number | null;
  /** Default shared-rail voltage (volts; dashboards prefer V over mV), when known. */
  defaultVoltageV: number | null;
  /** Primary expected PIC firmware byte, rendered as a hex string. */
  picFwByte: string;
  /**
   * Pre-computed BTU/h at rated power. Convenience field; same as
   * `wattsToBtuPerHour(ratedPowerW)` rounded to an integer.
   */
  ratedBtuPerHour: number;
  /**
   * Shared support tier for fallback profile rendering. Beta is the current
   * accepted-share-proven public-beta roster: S9 (am1-s9) and S19j Pro Zynq
   * (am2). Experimental means the rated specs are datasheet /
   * reverse-engineering figures for a model outside the public-beta install
   * set, so the dashboard must surface an honest development note.
   */
  supportTier: SupportTier;
  /**
   * Per-mode target copy, consistent with `MODE_DESCRIPTIONS` in
   * `utils/constants.ts`. Mode keys MUST stay `heater | standard | hacker`.
   */
  modes: {
    heater: {
      /** Power target fraction of rated wall watts. */
      powerFraction: number;
      summary: string;
    };
    standard: {
      powerFraction: number;
      summary: string;
    };
    hacker: {
      powerFraction: number;
      summary: string;
    };
  };
}

/** Convert watts to BTU/h. Use for ad-hoc rendering outside a fixed profile. */
export function wattsToBtuPerHour(watts: number): number {
  return Math.round(watts * BTU_PER_WATT);
}

export function isModelProfileProven(profile: ModelProfile): boolean {
  return profile.supportTier === 'stable' || profile.supportTier === 'beta';
}

/**
 * Mirror of `DCENT_OS_Antminer/dcentrald/dcentrald/src/model.rs` S19jProAm2
 * — verify on every firmware update.
 * Constants sourced from
 *  (2026-04-20).
 * NOTE: "S19j Pro" label covers both am2-Zynq (this profile) AND Amlogic
 * variant (future); chip ID disambiguates (see `detect_model_from_platform`).
 */
export const S19J_PRO_AM2_PROFILE: ModelProfile = {
  modelKey: 's19jproam2',
  displayName: 'Antminer S19j Pro',
  platformDisplay: 'Zynq am2',
  platformKey: 'zynq-bm3-am2',
  chip: 'BM1362',
  chainCount: 3,
  chipCountPerChain: 126,
  ratedHashrateTh: 104,
  ratedPowerW: 3068,
  thermalDesign: '3068W',
  defaultFrequencyMhz: 545,
  defaultVoltageV: 13.7,
  picFwByte: '0x89',
  ratedBtuPerHour: wattsToBtuPerHour(3068), // 10,467 BTU/h
  supportTier: 'beta', // S19j Pro Zynq am2 - accepted-share proven (.109/.25 roster + beta gate)
  modes: {
    heater: {
      powerFraction: 0.6,
      summary:
        'Home fan cap request with a conservative 60% power target. Acoustic proof needs live RPM. BTU/h shown live.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        'Rated 104 TH/s at 3068 W on safe voltages (13.7 V, 545 MHz). Pool-tuned efficiency.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'Raw FPGA access, autotuner exposed, per-chip freq/voltage. Handle with care.',
    },
  },
};

// ────────────────────────────────────────────────────────────────────────────
//  (2026-05-13): additional per-platform profiles for the new
// `PlatformOverviewCard`. Constants mirror `dcentrald/src/model.rs`'s
// `MODEL_TABLE` + extended-spec entries, sourced from
// hardware reference and per-platform knowledge-base research.
//
// Convention: each profile carries `modelKey` (firmware `Model::model_key()`),
// `platformKey` (firmware `Model::platform_key()`), an operator-facing
// `platformDisplay`, and per-mode summaries. ratedBtuPerHour is computed from
// ratedPowerW so the BTU/h ALWAYS-ON display rule
// holds for every platform without a code path for special-casing.
// ────────────────────────────────────────────────────────────────────────────

/** Antminer S9 — Zynq am1, BM1387, 3 chains × 63 chips. The original DCENT_OS dev platform. */
export const S9_AM1_PROFILE: ModelProfile = {
  modelKey: 's9',
  displayName: 'Antminer S9',
  platformDisplay: 'Zynq am1',
  platformKey: 'zynq-bm1-am1',
  chip: 'BM1387',
  chainCount: 3,
  chipCountPerChain: 63,
  ratedHashrateTh: 14,
  ratedPowerW: 1372,
  thermalDesign: '1372W',
  defaultFrequencyMhz: 650,
  defaultVoltageV: 9.1,
  picFwByte: '0x03',
  ratedBtuPerHour: wattsToBtuPerHour(1372), // ~4,682 BTU/h
  supportTier: 'beta', // am1-s9 - sustained cold-boot accepted-share proven + beta gate
  modes: {
    heater: {
      powerFraction: 0.5,
      summary:
        'Low-power home heater profile. Fan noise must be verified from live RPM/acoustic telemetry.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '14 TH/s at 1372W on PIC v0x03 firmware. 650 MHz / 9.1 V — silicon binning available via autotuner.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'Direct FPGA access, per-chip freq scan, AXI IIC dev tools. PIC heartbeat probe + I2C scanner exposed.',
    },
  },
};

/** Antminer S17 — Zynq am2-s17p, BM1397, 3 chains × 48 chips. */
export const S17_AM1_PROFILE: ModelProfile = {
  modelKey: 's17',
  displayName: 'Antminer S17',
  platformDisplay: 'Zynq am2-s17p',
  platformKey: 'am2-s17p',
  chip: 'BM1397',
  chainCount: 3,
  chipCountPerChain: 48,
  ratedHashrateTh: 56,
  ratedPowerW: 2520,
  thermalDesign: '2520W',
  defaultFrequencyMhz: 593,
  defaultVoltageV: 8.5,
  picFwByte: '0x82',
  ratedBtuPerHour: wattsToBtuPerHour(2520), // ~8,599 BTU/h
  supportTier: 'experimental', // S17 is outside the public-beta install set; specs are datasheet/RE only
  modes: {
    heater: {
      powerFraction: 0.55,
      summary:
        '~1.4 kW home heater profile, fans pinned to home-mode ceiling. dsPIC voltage tracked.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '56 TH/s at 2520W. dsPIC33EP voltage (~8.5 V), BM1397 at 593 MHz.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'Kernel FPGA driver path; dsPIC framed protocol exposed. Watch the 7s warm boot.',
    },
  },
};

/**
 * Antminer T17 -- X17 PIC16 path, BM1397, 3 chains x 30 chips.
 * Mirrors `lookup_model("t17")` + `RuntimeProfile::X17T17`.
 */
export const T17_X17_PROFILE: ModelProfile = {
  modelKey: 't17',
  displayName: 'Antminer T17',
  platformDisplay: 'Zynq x17-t17',
  platformKey: 'x17-t17-pic16-30',
  chip: 'BM1397',
  chainCount: 3,
  chipCountPerChain: 30,
  ratedHashrateTh: 40,
  ratedPowerW: 2200,
  thermalDesign: '2200W',
  defaultFrequencyMhz: null,
  defaultVoltageV: null,
  picFwByte: 'PIC16',
  ratedBtuPerHour: wattsToBtuPerHour(2200), // ~7,507 BTU/h
  supportTier: 'experimental', // Outside the public-beta install set; promotion gate remains hardware-led
  modes: {
    heater: {
      powerFraction: 0.55,
      summary:
        '~1.2 kW home heater profile. BM1397/T17 platform identity is registered for planning.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '40 TH/s at 2200W stock target. Feature in development for public install workflows.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'PIC16 T17 profile prepared for per-board evidence capture.',
    },
  },
};

/** Antminer S19 Pro — Zynq am2, BM1398, 3 chains × 114 chips. */
export const S19_PRO_AM2_PROFILE: ModelProfile = {
  modelKey: 's19pro',
  displayName: 'Antminer S19 Pro',
  platformDisplay: 'Zynq am2',
  platformKey: 'zynq-bm3-am2',
  chip: 'BM1398',
  chainCount: 3,
  chipCountPerChain: 114,
  ratedHashrateTh: 110,
  ratedPowerW: 3250,
  thermalDesign: '3250W',
  defaultFrequencyMhz: 525,
  defaultVoltageV: 13.8,
  picFwByte: '0x71',
  ratedBtuPerHour: wattsToBtuPerHour(3250), // ~11,089 BTU/h
  supportTier: 'experimental', // S19 Pro .129 cold-boot bring-up only - no accepted-share proof for this row
  modes: {
    heater: {
      powerFraction: 0.6,
      summary:
        'Conservative 60% power — ~1950W steady heater for larger spaces. Fans home-capped.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '110 TH/s at 3250W. dsPIC voltage @ ~13.8 V, BM1398 at 525 MHz.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'APW PSU bypass available; per-chain dsPIC heartbeat & EEPROM read tools exposed.',
    },
  },
};

/**
 * Antminer T19 -- CVITEK/CV183x class, BM1398.
 * Stock target comes from the in-repo autotuner target matrix; chip geometry
 * stays unresolved because the daemon model table and silicon profile keep it
 * hardware-gated.
 */
export const T19_CV183X_PROFILE: ModelProfile = {
  modelKey: 't19',
  displayName: 'Antminer T19',
  platformDisplay: 'CVITEK CV183x',
  platformKey: 'cv183x-bm1398-t19',
  chip: 'BM1398',
  chainCount: 3,
  chipCountPerChain: null,
  ratedHashrateTh: 84,
  ratedPowerW: 3150,
  thermalDesign: '3150W',
  defaultFrequencyMhz: null,
  defaultVoltageV: null,
  picFwByte: 'PIC1704',
  ratedBtuPerHour: wattsToBtuPerHour(3150), // ~10,748 BTU/h
  supportTier: 'experimental', // Outside the public-beta install set; promotion gate remains hardware-led
  modes: {
    heater: {
      powerFraction: 0.55,
      summary:
        '~1.7 kW home heater profile. BM1398/T19 platform identity is registered for planning.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '84 TH/s at 3150W stock target. Chip geometry and tuning defaults are in development.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'CV183x/PIC1704 T19 profile prepared for platform evidence capture.',
    },
  },
};

/**
 * Antminer S21 — Amlogic A113D am3-aml, BM1368, 3 chains × 108 chips.
 * NoPic (TAS5782M DAC voltage). First Amlogic platform with sustained mining.
 */
export const S21_AM3_AML_PROFILE: ModelProfile = {
  modelKey: 's21',
  displayName: 'Antminer S21',
  platformDisplay: 'Amlogic A113D',
  platformKey: 'amlogic-a113d-bm1368',
  chip: 'BM1368',
  chainCount: 3,
  chipCountPerChain: 108,
  ratedHashrateTh: 200,
  ratedPowerW: 3500,
  thermalDesign: '3500W',
  defaultFrequencyMhz: 475,
  defaultVoltageV: 12.5,
  picFwByte: 'NoPic',
  ratedBtuPerHour: wattsToBtuPerHour(3500), // ~11,942 BTU/h
  supportTier: 'experimental', // S21 family is not a beta-gate model
  modes: {
    heater: {
      powerFraction: 0.5,
      summary:
        '~1750W quiet heater on Amlogic NoPic platform. TAS5782M DAC voltage controlled.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '200 TH/s at 3500W. BM1368 at 475 MHz, TAS5782M voltage @ 12.5 V (audio-DAC pressed into PSU duty).',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'aarch64 native, /dev/ttyS2 single UART, GPIO sysfs (no kernel module). 14-step Mujina init.',
    },
  },
};

/**
 * Antminer S19 XP -- Amlogic am3-aml, BM1366, 3 chains x 110 chips.
 * Mirrors `lookup_model("s19xp")`, `dcentrald_s19xp.toml`, and the BM1366
 * operating-points profile. Registered here so it renders as an Experimental
 * feature instead of falling through to the unregistered dashboard state.
 */
export const S19_XP_AM3_PROFILE: ModelProfile = {
  modelKey: 's19xp',
  displayName: 'Antminer S19 XP',
  platformDisplay: 'Amlogic am3-aml',
  platformKey: 'amlogic-a113d-bm1366-s19xp',
  chip: 'BM1366',
  chainCount: 3,
  chipCountPerChain: 110,
  ratedHashrateTh: 140,
  ratedPowerW: 3010,
  thermalDesign: '3010W',
  defaultFrequencyMhz: 500,
  defaultVoltageV: 12.8,
  picFwByte: 'NoPic',
  ratedBtuPerHour: wattsToBtuPerHour(3010), // ~10,270 BTU/h
  supportTier: 'experimental', // Outside the public-beta install set; promotion gate remains hardware-led
  modes: {
    heater: {
      powerFraction: 0.55,
      summary:
        '~1.7 kW home heater profile. BM1366/S19 XP identity is registered for planning.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '140 TH/s at 3010W stock target. BM1366 110-chip profile is an Experimental feature.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'BM1366 NoPic S19 XP profile prepared for serial bring-up evidence capture.',
    },
  },
};

/**
 * Antminer S19k Pro — Amlogic am3-aml, BM1366, 3 chains × 77 chips.
 * Probed live on .78 (2026-04-29); APW121215f (fw=0x76).
 */
export const S19K_PRO_AM3_PROFILE: ModelProfile = {
  modelKey: 's19k',
  displayName: 'Antminer S19k Pro',
  platformDisplay: 'Amlogic am3-aml',
  platformKey: 'amlogic-a113d-bm1366',
  chip: 'BM1366',
  chainCount: 3,
  chipCountPerChain: 77,
  ratedHashrateTh: 120,
  ratedPowerW: 2760,
  thermalDesign: '2760W',
  defaultFrequencyMhz: 525,
  defaultVoltageV: 12.5,
  picFwByte: '0x76',
  ratedBtuPerHour: wattsToBtuPerHour(2760), // ~9,418 BTU/h
  supportTier: 'experimental', // S19k Pro probed read-only on .78 only - no accepted-share proof
  modes: {
    heater: {
      powerFraction: 0.6,
      summary:
        '~1650W dialed-down heater profile. NoPic platform; APW121215f telemetry feed.',
    },
    standard: {
      powerFraction: 1.0,
      summary:
        '120 TH/s at 2760W. BM1366 at 525 MHz, BHB56902 hashboards.',
    },
    hacker: {
      powerFraction: 1.0,
      summary:
        'BHB56902 EEPROM probe (0x05 0x11 preamble), APW121215f fw 0x76 telemetry exposed.',
    },
  },
};

/**
 * All registered dashboard model profiles. Keyed by `modelKey` (matches
 * firmware `Model::model_key()`). Additive only — do NOT rename or restructure.
 *
 * : extended from 1 to 6 profiles to cover the production fleet.
 * Phase-0 platform-matrix pass: registered S19 XP / T17 / T19 as explicit
 * Experimental feature profiles so mixed-fleet operators see a product-grade
 * in-development state instead of the unregistered fallback.
 */
export const MODEL_PROFILES: Readonly<Record<string, ModelProfile>> = {
  s9: S9_AM1_PROFILE,
  s17: S17_AM1_PROFILE,
  t17: T17_X17_PROFILE,
  s19pro: S19_PRO_AM2_PROFILE,
  t19: T19_CV183X_PROFILE,
  s19jproam2: S19J_PRO_AM2_PROFILE,
  s19xp: S19_XP_AM3_PROFILE,
  s21: S21_AM3_AML_PROFILE,
  s19k: S19K_PRO_AM3_PROFILE,
};

const MODEL_PROFILE_ALIASES: Readonly<Record<string, ModelProfile>> = {
  antminers9: S9_AM1_PROFILE,
  antminers17: S17_AM1_PROFILE,
  antminert17: T17_X17_PROFILE,
  antminers19pro: S19_PRO_AM2_PROFILE,
  antminert19: T19_CV183X_PROFILE,
  s19jpro: S19J_PRO_AM2_PROFILE,
  antminers19jpro: S19J_PRO_AM2_PROFILE,
  s19xpair: S19_XP_AM3_PROFILE,
  s19xpnopic: S19_XP_AM3_PROFILE,
  antminers19xp: S19_XP_AM3_PROFILE,
  s19kpro: S19K_PRO_AM3_PROFILE,
  antminers19kpro: S19K_PRO_AM3_PROFILE,
  antminers21: S21_AM3_AML_PROFILE,
};

/**
 * Normalize a free-form model string (from `/api/system/info`, VNish
 * firmware reports, or user input) to a firmware `model_key`. Mirrors
 * the Rust `normalize_model_token` helper — lowercase, alphanumeric only.
 */
function normalizeModelKey(raw: string | undefined | null): string {
  if (!raw) return '';
  return raw
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9+]/g, '');
}

/**
 * Look up a full `ModelProfile` by free-form model string. Returns
 * `undefined` for unregistered models — callers should fall back to the
 * existing `MODEL_DEFAULTS` in `EarningsPage.tsx` or to API-reported fields.
 *
 *  extends the alias resolution to cover the full registered fleet.
 * Marketing strings ("Antminer S9", "S19 Pro", "S21") and chip-id-disambiguated
 * variants ("s19jproam2") all resolve here.
 */
export function getModelProfile(model: string | undefined | null): ModelProfile | undefined {
  const key = normalizeModelKey(model);
  if (!key) return undefined;

  // Direct model_key hit (e.g. "s19jproam2", "s9").
  const direct = MODEL_PROFILES[key];
  if (direct) return direct;

  const alias = MODEL_PROFILE_ALIASES[key];
  if (alias) return alias;

  if (key.startsWith('antminer')) return undefined;

  // S19j Pro: am2 (Zynq) is the beta profile. The Amlogic variant
  // is hardware-gated (W22) so we keep am2 as the default rendering.
  if (key.includes('s19jpro')) {
    return S19J_PRO_AM2_PROFILE;
  }

  // S19k Pro family ("antminer s19k pro" → "antminers19kpro" → contains s19k).
  if (key.includes('s19kpro') || key === 's19k') {
    return S19K_PRO_AM3_PROFILE;
  }

  if (key.includes('s19xp')) {
    return S19_XP_AM3_PROFILE;
  }

  // S19 Pro (NOT S19j Pro / S19k Pro — those are matched above first).
  if (key.includes('s19pro')) {
    return S19_PRO_AM2_PROFILE;
  }

  if (key === 't19' || key.includes('t19')) {
    return T19_CV183X_PROFILE;
  }

  // S21 family — base model only for now (S21 Pro / S21 XP are future).
  if ((key === 's21' || key.includes('s21')) && !key.includes('pro') && !key.includes('xp')) {
    return S21_AM3_AML_PROFILE;
  }

  if (key === 't17' || (key.includes('t17') && !key.includes('+') && !key.includes('e'))) {
    return T17_X17_PROFILE;
  }

  // S17 base (S17 Pro / S17+ are silicon-binning variants — out of scope).
  if (key === 's17' || (key.includes('s17') && !key.includes('+') && !key.includes('e') && !key.includes('pro'))) {
    return S17_AM1_PROFILE;
  }

  // S9 base + variants ("antminers9", "antminers9j"). S9+ has its own model_key
  // ("s9+") which the firmware table reports separately; we map only base S9.
  if (key === 's9' || (key.startsWith('antminers9') && !key.includes('s9+') && !key.includes('s9j'))) {
    return S9_AM1_PROFILE;
  }

  return undefined;
}
