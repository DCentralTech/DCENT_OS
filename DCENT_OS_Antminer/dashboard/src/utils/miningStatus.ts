// Canonical mining-state selectors.
//
// Omega P0-7 (C-8): `isMining` used to be re-derived ≥3 inconsistent ways from
// the SAME `/api/status` payload — `utils/health.ts` used `hashrate_ghs > 0`
// (drives the topbar miner chip + favicon), `KitDashboardPage` used
// `hashrate_5s_ghs > 0 || chains.some(c => c.hashrate_ghs > 0)` (drives the
// `is-mining` grid class), and other surfaces differed again. Off one sample
// the topbar could read "Mining" while the per-chain grid read "Standby".
//
// This module is the SINGLE honest definition. Every whole-miner "is the miner
// mining?" derivation must call `selectIsMining(status)` so all consumers agree.
//
// The definition is the union of the prior derivations (so it is never *less*
// truthful than any surface was before): the miner is mining when the recent
// (5 s) hashrate is positive, OR the aggregate hashrate is positive, OR any
// individual hashboard is reporting hashrate this sample. A just-started or
// winding-down miner therefore reads consistently everywhere instead of
// flickering between "Mining" and "Standby" depending on which field a given
// component happened to read.

import type { StatusResponse } from '../api/types';

/**
 * Canonical "is the miner currently mining?" predicate.
 *
 * Honest, whole-miner definition derived strictly from live telemetry:
 * returns `true` iff the recent (5 s) hashrate, the aggregate hashrate, or any
 * single chain's hashrate is positive. Null/undefined status (no sample yet) is
 * `false` — "no telemetry" is never "mining".
 */
export function selectIsMining(status: StatusResponse | null | undefined): boolean {
  if (!status) {
    return false;
  }
  // Recent (5 s window) hashrate — the most responsive truthful signal.
  if ((status.hashrate_5s_ghs ?? 0) > 0) {
    return true;
  }
  // Longer-window aggregate hashrate.
  if ((status.hashrate_ghs ?? 0) > 0) {
    return true;
  }
  // Per-chain fallback: at least one hashboard is producing hashrate.
  return (status.chains ?? []).some(chain => (chain.hashrate_ghs ?? 0) > 0);
}
