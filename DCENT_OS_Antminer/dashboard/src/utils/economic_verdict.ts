// Per-platform economic verdict for "is this miner economically alive?"
//
// Three states:
//   profitable        -> profit > $0.20/day with NO heat credit applied
//   heat-credit-only  -> profit-with-credit > $0.20/day AND profit-without < 0
//   uneconomic        -> both negative
//
// The $0.20/day threshold is intentional. Below 20c/day a unit is
// effectively a hobby/space-heater amenity rather than a paying
// machine — that distinction is what users want to see at a glance.
//
// Inputs are deliberately minimal so the verdict is deterministic and
// easy to test. We compute revenue and electricity directly here
// instead of importing the daily-profit estimator so the verdict
// stays a pure function and the per-platform calibration can be
// reasoned about as a closed system.

export type EconomicVerdict = 'profitable' | 'heat-credit-only' | 'uneconomic';

export interface VerdictInputs {
  th_s: number;             // hashrate in TH/s (NOT GH/s)
  j_per_th: number;         // efficiency in joules per terahash
  kwh_rate: number;         // $/kWh
  btc_price: number;        // $/BTC
  heat_credit_per_day: number; // $/day already discounted by season + displaced fraction
}

export interface VerdictBreakdown {
  verdict: EconomicVerdict;
  daily_revenue_usd: number;
  daily_electricity_usd: number;
  daily_profit_no_credit_usd: number;
  daily_profit_with_credit_usd: number;
  reason: string;
}

// Roughly 5 sats / day / TH/s at current ~80T network difficulty.
// Same constant the rest of the dashboard uses (estimateDailySats).
const SATS_PER_TH_PER_DAY = 5;

// Threshold above which a result is considered "actually profitable"
// rather than rounding noise.
export const VERDICT_PROFIT_THRESHOLD_USD_PER_DAY = 0.20;

export function dailyRevenueUsd(th_s: number, btc_price: number): number {
  if (th_s <= 0 || btc_price <= 0) return 0;
  const sats = th_s * SATS_PER_TH_PER_DAY;
  return (sats / 100_000_000) * btc_price;
}

export function dailyElectricityUsd(
  th_s: number,
  j_per_th: number,
  kwh_rate: number,
): number {
  if (th_s <= 0 || j_per_th <= 0 || kwh_rate <= 0) return 0;
  // power_W = TH/s × J/TH    (since J/s = W and the /s cancels)
  const power_w = th_s * j_per_th;
  return (power_w / 1000) * 24 * kwh_rate;
}

export function verdictBreakdown(inputs: VerdictInputs): VerdictBreakdown {
  const revenue = dailyRevenueUsd(inputs.th_s, inputs.btc_price);
  const electricity = dailyElectricityUsd(
    inputs.th_s,
    inputs.j_per_th,
    inputs.kwh_rate,
  );
  const heatCredit = Math.max(0, inputs.heat_credit_per_day);

  const profitNoCredit = revenue - electricity;
  const profitWithCredit = revenue - electricity + heatCredit;

  let verdict: EconomicVerdict;
  let reason: string;

  if (profitNoCredit > VERDICT_PROFIT_THRESHOLD_USD_PER_DAY) {
    verdict = 'profitable';
    reason = 'Profitable on Bitcoin alone — heat is a bonus.';
  } else if (
    profitWithCredit > VERDICT_PROFIT_THRESHOLD_USD_PER_DAY &&
    profitNoCredit < 0
  ) {
    verdict = 'heat-credit-only';
    reason =
      'Profitable only when displaced heating is counted. Net negative as a pure miner.';
  } else if (
    profitWithCredit > VERDICT_PROFIT_THRESHOLD_USD_PER_DAY &&
    profitNoCredit >= 0 &&
    profitNoCredit <= VERDICT_PROFIT_THRESHOLD_USD_PER_DAY
  ) {
    // Borderline: barely positive on its own, only meaningfully positive
    // once the heat credit is added. Treat as heat-credit-only because
    // mining alone is below the meaningful-profit threshold.
    verdict = 'heat-credit-only';
    reason =
      'Marginal on Bitcoin alone; heat displacement carries it past the daily threshold.';
  } else {
    verdict = 'uneconomic';
    reason =
      'Net negative even with heat credit applied. Treat as a heater that happens to mine.';
  }

  return {
    verdict,
    daily_revenue_usd: revenue,
    daily_electricity_usd: electricity,
    daily_profit_no_credit_usd: profitNoCredit,
    daily_profit_with_credit_usd: profitWithCredit,
    reason,
  };
}

// Convenience wrapper that drops the breakdown.
export function verdictFor(inputs: VerdictInputs): EconomicVerdict {
  return verdictBreakdown(inputs).verdict;
}

// Per-platform calibration table. Each entry is the canonical
// efficiency for a class of hardware so the verdict UI can reason
// about "what does this look like at $0.10/kWh on an S9?" without
// the user having to type J/TH manually.
//
// Sources: J/TH numbers match dcentrald-silicon-profiles defaults
// and the per-model intel summarized in DCENT_OS_Antminer/.
export interface PlatformProfile {
  id: string;
  label: string;
  th_s_typical: number;
  j_per_th_typical: number;
  notes?: string;
}

export const PLATFORM_PROFILES: PlatformProfile[] = [
  { id: 's9',         label: 'S9 (BM1387)',          th_s_typical: 13.5,  j_per_th_typical: 85,   notes: 'Classic space heater' },
  { id: 's17',        label: 'S17 (BM1397)',         th_s_typical: 53,    j_per_th_typical: 45 },
  { id: 's19pro',     label: 'S19 Pro (BM1398)',     th_s_typical: 110,   j_per_th_typical: 29.5 },
  { id: 's19jpro',    label: 'S19j Pro (BM1362)',    th_s_typical: 100,   j_per_th_typical: 30 },
  { id: 's19kpro',    label: 'S19k Pro (BM1366)',    th_s_typical: 120,   j_per_th_typical: 23 },
  { id: 's21',        label: 'S21 (BM1368)',         th_s_typical: 200,   j_per_th_typical: 17.5 },
  { id: 's21pro',     label: 'S21 Pro',              th_s_typical: 234,   j_per_th_typical: 15 },
  { id: 'whatsminer', label: 'WhatsMiner M30S+',     th_s_typical: 100,   j_per_th_typical: 34 },
  { id: 'bitaxe',     label: 'BitAxe Hex Supra',     th_s_typical: 4.4,   j_per_th_typical: 22 },
];

export function profileForPlatform(id: string): PlatformProfile | undefined {
  return PLATFORM_PROFILES.find(p => p.id === id);
}

// Approximate the kWh rate at which the verdict flips between
// states for a given hardware profile, ignoring heat credit. Useful
// for "you need rates below $0.18/kWh for this to be profitable"
// callouts.
export function breakEvenKwhRate(
  th_s: number,
  j_per_th: number,
  btc_price: number,
): number | null {
  if (th_s <= 0 || j_per_th <= 0 || btc_price <= 0) return null;
  const revenue = dailyRevenueUsd(th_s, btc_price);
  const power_w = th_s * j_per_th;
  if (power_w <= 0) return null;
  const kwh_per_day = (power_w / 1000) * 24;
  if (kwh_per_day <= 0) return null;
  return Math.max(0, revenue / kwh_per_day);
}
