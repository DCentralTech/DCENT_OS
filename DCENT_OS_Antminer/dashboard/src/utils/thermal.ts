// BTU/watt conversion, noise estimation, profitability calculations

// 1 Watt = 3.412142 BTU/h — the ONE canonical BTU/h-per-watt constant per the
// Terminology Lexicon (TERM-3 §3.3). Pinned to full precision so the constant
// is identical across both firmwares and across every OS file that converts
// watts→BTU (modelProfiles.ts BTU_PER_WATT mirrors this value).
const WATTS_TO_BTU = 3.412142;

export function wattsToBtu(watts: number): number {
  return Math.round(watts * WATTS_TO_BTU);
}

export function btuToWatts(btu: number): number {
  return Math.round(btu / WATTS_TO_BTU);
}

// Legacy S9-only PWM-to-dB estimate. Do not use as acoustic proof on AM2/XIL;
// live UI should prefer backend tach/RPM-backed noise fields.
export function estimateNoise(pwm: number): number {
  if (pwm <= 0) return 0;
  if (pwm <= 10) return 30 + pwm * 0.5;
  return Math.min(70, 35 + (pwm - 10) * 0.28);
}

// Noise level category
export function noiseLevel(db: number): 'silent' | 'whisper' | 'quiet' | 'moderate' | 'loud' | 'very-loud' {
  if (db <= 0) return 'silent';
  if (db <= 35) return 'whisper';
  if (db <= 45) return 'quiet';
  if (db <= 55) return 'moderate';
  if (db <= 65) return 'loud';
  return 'very-loud';
}

// ─── Heating Value Helpers ──────────────────────────────────

// Estimate heating offset value: what you'd pay for equivalent electric heat
// watts_to_btu(power) / 3412.14 (kW equivalent) * hours * electricity rate
export function estimateHeatingOffset(
  powerWatts: number,
  hoursRunning: number,
  electricityRate: number, // $/kWh
): number {
  const kw = powerWatts / 1000;
  return kw * hoursRunning * electricityRate;
}

// ─── Heat Credit v2 ─────────────────────────────────────────
// Replaces the binary "is heater on?" toggle with a continuous
// climate-zone × heating-season × displaced-fraction model.
//
// Reasoning:
//   The whole-cost-offset toggle (true → 100% credit, false → 0%) is
//   too coarse. A real home in Quebec running a miner in July gets
//   ~zero heating credit; the same home in February gets nearly the
//   full credit. The seasonal model:
//     credit_per_day = displaced_fraction
//                    × heating_season_active   (0..1, fraction of year)
//                    × wall_kW × 24h × $/kWh
//   yields a smooth, honest, climate-aware annualized daily credit.
//
//   `heating_season_active` is the fraction of the year the user
//   actually heats their home. It can be derived from a months
//   slider (e.g. 7/12 = 0.583) or from HDD presets (cold zones
//   approach 0.75; warm zones approach 0.10).

export interface SeasonalHeatCreditInputs {
  wall_watts: number;            // measured wall power
  displaced_fraction: number;    // 0..1, how much of the heat actually displaces other heating
  heating_season_active: number; // 0..1, fraction of year the home is being heated
  kwh_rate: number;              // $/kWh of the displaced heater (defaults to user $/kWh)
}

// Returns the per-day heat-credit value in USD, annualized over the
// year and amortized to a single 24-hour day.
export function seasonalHeatCredit({
  wall_watts,
  displaced_fraction,
  heating_season_active,
  kwh_rate,
}: SeasonalHeatCreditInputs): number {
  if (wall_watts <= 0 || kwh_rate <= 0) return 0;
  const df = Math.max(0, Math.min(1, displaced_fraction));
  const hs = Math.max(0, Math.min(1, heating_season_active));
  const kw = wall_watts / 1000;
  return df * hs * kw * 24 * kwh_rate;
}

// Convert a months count (0..12) to a heating_season_active fraction.
export function monthsToFraction(months: number): number {
  return Math.max(0, Math.min(12, months)) / 12;
}

// Heating zone presets — bundled offline so the dashboard works
// without network access. HDD = Heating Degree Days (base 18 C).
// season_months_default is a reasonable starting point for the
// month-range slider; the user can override.
export interface HeatingZone {
  id: string;
  name: string;
  hdd: number;                     // typical annual HDD (base 18 C)
  season_months_default: number;   // typical heating season length, months
  default_displaced_fraction: number; // typical realistic displacement
  notes?: string;
}

export const HEATING_ZONES: HeatingZone[] = [
  {
    id: 'quebec-hydro',
    name: 'Quebec / Hydro-Québec',
    hdd: 4500,
    season_months_default: 7,
    default_displaced_fraction: 0.85,
    notes: 'Cold continental, very long heating season',
  },
  {
    id: 'alberta',
    name: 'Alberta / Prairies',
    hdd: 5200,
    season_months_default: 7,
    default_displaced_fraction: 0.85,
    notes: 'Cold continental, gas heat dominant',
  },
  {
    id: 'ontario',
    name: 'Ontario',
    hdd: 4000,
    season_months_default: 6,
    default_displaced_fraction: 0.80,
  },
  {
    id: 'bc-coastal',
    name: 'BC Coastal',
    hdd: 2900,
    season_months_default: 6,
    default_displaced_fraction: 0.70,
  },
  {
    id: 'maritimes',
    name: 'Atlantic Canada',
    hdd: 4200,
    season_months_default: 7,
    default_displaced_fraction: 0.85,
  },
  {
    id: 'nordic',
    name: 'Nordic / Scandinavia',
    hdd: 5500,
    season_months_default: 8,
    default_displaced_fraction: 0.90,
  },
  {
    id: 'germany-france-north',
    name: 'Northern Europe',
    hdd: 3300,
    season_months_default: 6,
    default_displaced_fraction: 0.75,
  },
  {
    id: 'uk-ireland',
    name: 'UK / Ireland',
    hdd: 2800,
    season_months_default: 7,
    default_displaced_fraction: 0.75,
  },
  {
    id: 'us-midwest',
    name: 'US Midwest',
    hdd: 3700,
    season_months_default: 6,
    default_displaced_fraction: 0.80,
  },
  {
    id: 'us-northeast',
    name: 'US Northeast',
    hdd: 3500,
    season_months_default: 6,
    default_displaced_fraction: 0.80,
  },
  {
    id: 'us-mountain',
    name: 'US Mountain West',
    hdd: 3400,
    season_months_default: 6,
    default_displaced_fraction: 0.78,
  },
  {
    id: 'us-pacific-nw',
    name: 'US Pacific Northwest',
    hdd: 2600,
    season_months_default: 6,
    default_displaced_fraction: 0.70,
  },
  {
    id: 'us-northern-plains',
    name: 'US Northern Plains',
    hdd: 4500,
    season_months_default: 7,
    default_displaced_fraction: 0.85,
  },
  {
    id: 'us-mid-atlantic',
    name: 'US Mid-Atlantic',
    hdd: 2400,
    season_months_default: 5,
    default_displaced_fraction: 0.65,
  },
  {
    id: 'us-southeast',
    name: 'US Southeast',
    hdd: 1500,
    season_months_default: 4,
    default_displaced_fraction: 0.45,
  },
  {
    id: 'texas',
    name: 'Texas',
    hdd: 1200,
    season_months_default: 3,
    default_displaced_fraction: 0.35,
  },
  {
    id: 'california-coastal',
    name: 'California Coastal',
    hdd: 1500,
    season_months_default: 4,
    default_displaced_fraction: 0.45,
  },
  {
    id: 'california-central',
    name: 'California Central',
    hdd: 1900,
    season_months_default: 4,
    default_displaced_fraction: 0.55,
  },
  {
    id: 'arizona-desert',
    name: 'Arizona / Desert SW',
    hdd: 900,
    season_months_default: 3,
    default_displaced_fraction: 0.25,
  },
  {
    id: 'florida',
    name: 'Florida',
    hdd: 500,
    season_months_default: 2,
    default_displaced_fraction: 0.15,
  },
  {
    id: 'hawaii-tropical',
    name: 'Hawaii / Tropical',
    hdd: 100,
    season_months_default: 0,
    default_displaced_fraction: 0.0,
    notes: 'No appreciable heating load',
  },
  {
    id: 'mexico-highlands',
    name: 'Mexico Highlands',
    hdd: 1200,
    season_months_default: 4,
    default_displaced_fraction: 0.40,
  },
  {
    id: 'mexico-coastal',
    name: 'Mexico Coastal',
    hdd: 200,
    season_months_default: 1,
    default_displaced_fraction: 0.05,
  },
  {
    id: 'australia-south',
    name: 'Australia South',
    hdd: 1800,
    season_months_default: 5,
    default_displaced_fraction: 0.55,
  },
  {
    id: 'australia-north',
    name: 'Australia North',
    hdd: 200,
    season_months_default: 1,
    default_displaced_fraction: 0.05,
  },
  {
    id: 'nz',
    name: 'New Zealand',
    hdd: 2400,
    season_months_default: 6,
    default_displaced_fraction: 0.70,
  },
  {
    id: 'japan-honshu',
    name: 'Japan / Korea',
    hdd: 2300,
    season_months_default: 5,
    default_displaced_fraction: 0.65,
  },
  {
    id: 'middle-east',
    name: 'Middle East',
    hdd: 800,
    season_months_default: 2,
    default_displaced_fraction: 0.20,
  },
  {
    id: 'south-asia',
    name: 'South Asia',
    hdd: 400,
    season_months_default: 1,
    default_displaced_fraction: 0.10,
  },
  {
    id: 'custom',
    name: 'Custom',
    hdd: 0,
    season_months_default: 6,
    default_displaced_fraction: 0.50,
    notes: 'Manual configuration',
  },
];

export function findHeatingZone(id: string): HeatingZone | undefined {
  return HEATING_ZONES.find(z => z.id === id);
}

// Noise comparison: returns a friendly everyday comparison
export function noiseComparison(db: number): string {
  if (db <= 0) return 'Silent';
  if (db <= 30) return 'Like a whisper';
  if (db <= 35) return 'Like a quiet library';
  if (db <= 40) return 'Like a refrigerator';
  if (db <= 45) return 'Like a quiet office';
  if (db <= 50) return 'Like a conversation';
  if (db <= 55) return 'Like a window fan';
  if (db <= 60) return 'Like a dishwasher';
  if (db <= 65) return 'Like a vacuum cleaner';
  return 'Like a blender';
}

// BTU comparison: how many typical 1500W space heaters equivalent
// A standard space heater outputs ~5,120 BTU/h (1500W * 3.41214)
const STANDARD_HEATER_BTU = 5120;

export function btuComparison(btuH: number): string {
  if (btuH <= 0) return 'Off';
  const heaters = btuH / STANDARD_HEATER_BTU;
  if (heaters < 0.2) return 'Like a small desk heater';
  if (heaters < 0.5) return 'Like a portable heater on low';
  if (heaters < 0.8) return 'Like a portable heater on medium';
  if (heaters < 1.2) return 'Like a standard space heater';
  if (heaters < 1.8) return 'Like 1.5 space heaters';
  if (heaters < 2.5) return 'Like 2 space heaters';
  return `Like ${Math.round(heaters)} space heaters`;
}

export function btuHeaterCount(btuH: number): number {
  return btuH / STANDARD_HEATER_BTU;
}

// ─── Temperature Conversion ─────────────────────────────────

export function toDisplayTemp(celsius: number, unit: 'C' | 'F'): number {
  if (unit === 'F') return (celsius * 9) / 5 + 32;
  return celsius;
}

export function tempUnitSymbol(unit: 'C' | 'F'): string {
  return unit === 'F' ? '\u00B0F' : '\u00B0C';
}

// ─── Profitability ──────────────────────────────────────────

// P0-4 (C-5/C-6/D-10/D-11): canonical, network-difficulty-anchored daily-sats
// estimator. This is the client-side mirror of the backend estimator
// (rest.rs::estimate_daily_sats_network_anchored):
//
//   sats/day = hashrate_ths * 1e12 * 86_400 / (network_difficulty * 2^32)
//              * block_subsidy_sats
//
// `networkDifficulty` MUST come from the backend (heaterStatus.network_difficulty,
// fed by /api/home/status). When it is missing or non-positive we return 0
// rather than fabricating a number — the caller should fall back to the
// canonical server-reported `sats_today`. This replaces the old
// `satsPerThPerDay = 5` stub, which ignored network difficulty entirely and
// produced a value disconnected from real economics.
export function estimateDailySats(
  hashrateGhs: number,
  networkDifficulty: number | null | undefined,
  nowMs: number = Date.now(),
): number {
  if (!networkDifficulty || networkDifficulty <= 0 || hashrateGhs <= 0) return 0;
  const hashesPerSec = hashrateGhs * 1e9;                   // GH/s → H/s
  const hashesPerBlock = networkDifficulty * 4_294_967_296; // 2^32
  const blocksPerDay = (hashesPerSec * 86_400) / hashesPerBlock;
  const blockRewardSats = blockRewardAt(nowMs) * 100_000_000;
  return Math.round(blocksPerDay * blockRewardSats);
}

export function estimateSatsPerSecond(
  hashrateGhs: number,
  networkDifficulty: number | null | undefined,
  nowMs: number = Date.now(),
): number {
  return estimateDailySats(hashrateGhs, networkDifficulty, nowMs) / 86400;
}

export function estimateDailyCost(watts: number, electricityRate: number): number {
  // watts * 24h / 1000 * $/kWh
  return (watts * 24) / 1000 * electricityRate;
}

export function estimateDailyProfit(
  hashrateGhs: number,
  watts: number,
  btcPrice: number,
  electricityRate: number,
  networkDifficulty: number | null | undefined,
): { sats: number; revenue: number; cost: number; profit: number } {
  const sats = estimateDailySats(hashrateGhs, networkDifficulty);
  const revenue = (sats / 100000000) * btcPrice;
  const cost = estimateDailyCost(watts, electricityRate);
  return { sats, revenue, cost, profit: revenue - cost };
}

// ─── W8.3: Halving-Aware Projections ───────────────────────────
//
// Halving epochs (Unix ms) and the post-halving block reward.
// Mirrored from `dcentrald-autotuner/src/profitability.rs::HALVING_EPOCHS_SEC`.
// These are estimates — the actual halving fires on a block height, not a
// wall clock — but they are within a few weeks of the real event and good
// enough for "show the cliff" UX.
const HALVING_EPOCHS_MS: ReadonlyArray<{ epochMs: number; rewardBtc: number }> = [
  { epochMs: 1_713_484_800_000, rewardBtc: 3.125 },     // 2024-04-19
  { epochMs: 1_839_000_000_000, rewardBtc: 1.5625 },    // ~2028-04-15
  { epochMs: 1_965_000_000_000, rewardBtc: 0.78125 },   // ~2032-04-15
  { epochMs: 2_091_000_000_000, rewardBtc: 0.390625 },  // ~2036-04-15
];

export function blockRewardAt(nowMs: number = Date.now()): number {
  let reward = 3.125;
  for (const { epochMs, rewardBtc } of HALVING_EPOCHS_MS) {
    if (nowMs >= epochMs) {
      reward = rewardBtc;
    } else {
      break;
    }
  }
  return reward;
}

/** Returns the next halving's `{ epochMs, rewardBtc }` or `null` if past 2036. */
export function nextHalving(
  nowMs: number = Date.now(),
): { epochMs: number; rewardBtc: number } | null {
  for (const h of HALVING_EPOCHS_MS) {
    if (h.epochMs > nowMs) return h;
  }
  return null;
}

export function daysToHalving(nowMs: number = Date.now()): number | null {
  const h = nextHalving(nowMs);
  if (!h) return null;
  return Math.max(0, (h.epochMs - nowMs) / 86_400_000);
}

/**
 * 4-year cumulative revenue with halving cliff.
 *
 * Earns `dailyRevenue` for `days_to_halving` days, then `dailyRevenue *
 * (next_reward / current_reward)` for the rest of the 4-year window.
 * If no halving falls in the next 4 years, integrates `dailyRevenue` for
 * 1461 days flat.
 *
 * Used by EarningsPage and the install wizard's earnings preview to
 * surface the cliff explicitly so users don't price their ROI plan
 * against a reward that vanishes mid-window.
 */
export function fourYearRevenueWithHalving(
  dailyRevenue: number,
  nowMs: number = Date.now(),
): { revenueUsd: number; preDays: number; postDays: number; postFactor: number } {
  const FOUR_YEARS_DAYS = 365.25 * 4;
  const cur = blockRewardAt(nowMs);
  const next = nextHalving(nowMs);
  const dth = daysToHalving(nowMs);
  const postFactor = next && cur > 0 ? next.rewardBtc / cur : 1.0;
  if (dth === null || dth >= FOUR_YEARS_DAYS) {
    return {
      revenueUsd: dailyRevenue * FOUR_YEARS_DAYS,
      preDays: FOUR_YEARS_DAYS,
      postDays: 0,
      postFactor,
    };
  }
  const preDays = dth;
  const postDays = FOUR_YEARS_DAYS - dth;
  return {
    revenueUsd: dailyRevenue * preDays + dailyRevenue * postFactor * postDays,
    preDays,
    postDays,
    postFactor,
  };
}
