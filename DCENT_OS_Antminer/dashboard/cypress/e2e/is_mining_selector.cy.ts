// Unit conformance test — canonical selectIsMining selector (Omega P0-7 / C-8).
//
// `isMining` was previously derived ≥3 inconsistent ways from ONE /api/status
// payload: utils/health.ts used `hashrate_ghs > 0` (topbar chip + favicon),
// KitDashboardPage used `hashrate_5s_ghs > 0 || chains.some(c => c.hashrate_ghs
// > 0)` (the `is-mining` grid class), and other surfaces differed again. Off a
// single sample the topbar could read "Mining" while the per-chain grid read
// "Standby". `utils/miningStatus.ts::selectIsMining` is now the single honest
// definition every whole-miner consumer shares.
//
// Pure-logic spec: imports the selector and asserts with chai `expect` — no
// `cy.visit` / live server needed (runs in plain `cypress run`, mirroring the
// pattern in model_profiles_sync.cy.ts). The build's `tsc` (tsconfig include
// = ["src"]) type-checks the selector itself; this spec pins its behaviour.

/// <reference types="cypress" />

import { selectIsMining } from '../../src/utils/miningStatus';
import type { ChainState, StatusResponse } from '../../src/api/types';

// Minimal builders — the selector only reads hashrate_5s_ghs / hashrate_ghs /
// chains[].hashrate_ghs, so we cast partials rather than constructing a full
// (pool/fans/...) StatusResponse. Keeps the test focused on the contract.
const asStatus = (o: Partial<StatusResponse>): StatusResponse => o as StatusResponse;
const chain = (hashrate_ghs: number): ChainState => ({ hashrate_ghs } as ChainState);

describe('selectIsMining — canonical whole-miner mining predicate (P0-7 / C-8)', () => {
  it('treats no telemetry as NOT mining', () => {
    expect(selectIsMining(null), 'null status').to.eq(false);
    expect(selectIsMining(undefined), 'undefined status').to.eq(false);
  });

  it('is false when every hashrate signal is zero', () => {
    expect(
      selectIsMining(asStatus({ hashrate_ghs: 0, hashrate_5s_ghs: 0, chains: [chain(0), chain(0)] })),
      'all-zero sample',
    ).to.eq(false);
    // Defensive: missing optional fields must not throw and must read false.
    expect(selectIsMining(asStatus({})), 'empty status object').to.eq(false);
  });

  it('is true on the legacy health.ts signal (aggregate hashrate > 0)', () => {
    expect(
      selectIsMining(asStatus({ hashrate_ghs: 13500, hashrate_5s_ghs: 0, chains: [] })),
      'aggregate hashrate positive',
    ).to.eq(true);
  });

  it('is true on the legacy KitDashboard signals (5 s hashrate OR any chain hashing)', () => {
    // 5 s-only: old health.ts said "Standby", old KitDashboard said "Mining" —
    // they now collapse to the SAME canonical result.
    expect(
      selectIsMining(asStatus({ hashrate_ghs: 0, hashrate_5s_ghs: 96200, chains: [] })),
      'recent (5 s) hashrate positive while aggregate is 0',
    ).to.eq(true);
    // Per-chain-only: aggregate + 5 s both 0 but one board is hashing.
    expect(
      selectIsMining(asStatus({ hashrate_ghs: 0, hashrate_5s_ghs: 0, chains: [chain(0), chain(31650)] })),
      'a single chain reporting hashrate',
    ).to.eq(true);
  });

  it('returns ONE value for the just-started sample the prior derivations disagreed on', () => {
    // The exact bug: 5 s hashrate + a chain hashing, but the longer-window
    // aggregate has not caught up yet. Old health.ts (hashrate_ghs > 0) → false,
    // old KitDashboard (hashrate_5s_ghs > 0 || chains.some) → true. The single
    // selector must give one answer to both consumers.
    const justStarted = asStatus({ hashrate_ghs: 0, hashrate_5s_ghs: 200, chains: [chain(150)] });

    const legacyHealth = (justStarted.hashrate_ghs ?? 0) > 0; // false
    const legacyKit =
      (justStarted.hashrate_5s_ghs ?? 0) > 0 ||
      (justStarted.chains ?? []).some(c => (c.hashrate_ghs ?? 0) > 0); // true
    expect(legacyHealth, 'sanity: the legacy derivations genuinely disagreed').to.not.eq(legacyKit);

    const canonical = selectIsMining(justStarted);
    expect(canonical, 'canonical selector resolves the disagreement to a single value').to.eq(true);
    // And it is never LESS truthful than either prior surface (union semantics).
    expect(canonical, 'canonical ⊇ legacy health.ts').to.eq(legacyHealth || canonical);
    expect(canonical, 'canonical ⊇ legacy KitDashboard').to.eq(legacyKit);
  });
});
