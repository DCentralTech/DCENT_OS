// Cypress conformance test — model profiles ↔ silicon-profile crate sync (W5.7)
//
//: hardcoded firmware lists
// in the dashboard drift from the firmware source-of-truth. The dashboard
// ships an embedded `MODEL_PROFILES` snapshot in `utils/modelProfiles.ts`
// which is the LAST-KNOWN-GOOD fallback when the daemon API is unreachable.
// The source of truth is the `dcentrald-silicon-profiles` registry, served
// at `GET /api/profiles/silicon`.
//
// This test asserts the API response chip-id set is a SUPERSET of (or equal
// to) the chip set the dashboard snapshot relies on. A divergence here means
// the silicon-profiles crate has gained chips the dashboard doesn't know
// about (chip rendering will fall back to "Unregistered") OR the dashboard
// snapshot was bumped without a matching backend change (a regression).
//
// Default CI checks the QA mock fixture. A live-daemon conformance check is
// added only when `CYPRESS_LIVE_DAEMON=1` is set.

/// <reference types="cypress" />

import { FIXTURES } from '../../src/dev/mockFixtures';

// Chip IDs the dashboard expects every silicon-profile registry to have an
// entry for. Mirrors `MODEL_PROFILES` in `utils/modelProfiles.ts`. Must be
// updated together with the static map.
//
// Note: the dashboard `ModelProfile.chip` is the marketing chip label
// (e.g. "BM1387"); the silicon-profiles API returns lowercase ids
// (e.g. "bm1387"). Compare in lowercase to avoid case-skew false positives.
const EXPECTED_CHIP_IDS = [
  'bm1387', // S9
  'bm1397', // S17
  'bm1398', // S19 Pro
  'bm1362', // S19j Pro am2
  'bm1368', // S21
  'bm1366', // S19k Pro
];

interface SiliconProfileSummary {
  id: string;
  miner_model: string;
  hashboard: string;
  chip: string;
  source_class: string;
  preset_count: number;
}

function expectExpectedChipsPresent(rows: SiliconProfileSummary[], label: string): void {
  const apiChips = new Set<string>();
  for (const row of rows) {
    if (row && typeof row.chip === 'string') {
      apiChips.add(row.chip.toLowerCase());
    }
  }

  const missing = EXPECTED_CHIP_IDS.filter(id => !apiChips.has(id));
  expect(
    missing,
    `${label} is missing chip(s) the dashboard ships profiles for: ${missing.join(', ')}`,
  ).to.have.length(0);
}

function mockSiliconProfileFixture(): SiliconProfileSummary[] {
  const rows = FIXTURES['/api/profiles/silicon'];
  expect(rows, 'QA mock /api/profiles/silicon fixture').to.be.an('array');
  return rows as SiliconProfileSummary[];
}

describe('Model profiles sync (dashboard snapshot ↔ /api/profiles/silicon)', () => {
  const liveDaemon = Cypress.env('CYPRESS_LIVE_DAEMON');

  it('keeps the default mock silicon-profile fixture aligned with the dashboard chip set', () => {
    expectExpectedChipsPresent(mockSiliconProfileFixture(), 'mock /api/profiles/silicon fixture');
  });

  if (liveDaemon) {
    it('exposes every chip the dashboard renders profiles for from the live daemon', () => {
      cy.request<SiliconProfileSummary[]>('/api/profiles/silicon').then(res => {
        expect(res.status, '/api/profiles/silicon HTTP status').to.eq(200);
        expect(res.body, 'response body').to.be.an('array');

        expectExpectedChipsPresent(res.body, 'silicon-profiles registry');

        // Surface NEW chips the daemon knows about that the dashboard doesn't —
        // not a hard failure (additive backend changes are allowed) but worth
        // logging so the next dashboard pass picks them up.
        const expectedSet = new Set(EXPECTED_CHIP_IDS);
        const novel: string[] = [];
        for (const row of res.body) {
          const id = typeof row?.chip === 'string' ? row.chip.toLowerCase() : '';
          if (id && !expectedSet.has(id)) novel.push(id);
        }
        if (novel.length > 0) {
          cy.log(
            `Daemon reports chip(s) the dashboard snapshot doesn't yet cover: ${novel.join(
              ', ',
            )}. Add to MODEL_PROFILES in utils/modelProfiles.ts.`,
          );
        }
      });
    });
  }
});
