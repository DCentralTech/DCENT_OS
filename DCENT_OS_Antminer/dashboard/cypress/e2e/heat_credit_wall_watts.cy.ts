// Cypress e2e — HeatingValueSummary uses wall watts (W8.6, wave 8)
//
// Background (W8.6): heat-credit math is an economic surface. Mixing
// `getDisplayPowerWatts()` (board-level fallback) with `getWallWatts()`
// elsewhere causes a ~10% discrepancy because PSU losses dissipate as heat
// AND show up on the electricity bill — using board-level watts
// double-distorts the credit. This test pins HeatingValueSummary to the
// wall-watts path.
//
// Stubs:
//   GET /api/system/info  — minimal Heater-mode fixture
//   GET /api/stats        — power.watts=1000 (board), wall_watts=1180 (wall)
//                           — a 180 W gap is a common APW3 PSU loss profile
//   GET /api/home/status  — null wall_watts so we exercise the fallback
//                           through `getWallWatts(stats.power)`
//   GET /api/status       — uptime 1 h, hashrate 13.5 TH/s, satoshis 0
//
// Assertion:
//   The "Electricity cost" row in HeatingValueSummary shows
//   $1180 W × 1 h × $0.10/kWh = $0.118 ≈ "$0.12", NOT
//   $1000 W × 1 h × $0.10/kWh = $0.10.
//   With heating mode ON (default), the displayed cost is shown crossed-out
//   but the underlying value is still the wall-watts cost.

/// <reference types="cypress" />

const FAKE_SERIAL = 'S9-LAB-W8.6';
const MODEL = 'Antminer S9';

function stubCommonRoutes() {
  cy.intercept('GET', '/api/system/health', {
    statusCode: 200,
    body: { mode: 'native', alive: true, blockers: [] },
  });
  cy.intercept('GET', '/api/competitive/readiness', {
    statusCode: 200,
    body: { features: [] },
  });
  cy.intercept('GET', '/api/pools/failover', {
    statusCode: 200,
    body: { primary: null, backup: null, donation: null },
  });
  cy.intercept('GET', '/api/competitive/manifest', {
    statusCode: 200,
    body: { entries: [] },
  });
  cy.intercept('GET', '/api/autotuner/status', {
    statusCode: 200,
    body: { enabled: false, live_runtime: false, stale: true, age_s: 0 },
  });
}

function stubHeatFixture() {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.9.0',
      model: MODEL,
      hostname: FAKE_SERIAL,
      mac: '00:11:22:33:44:55',
      uptime_s: 3600, // 1 hour
      chip_type: 'bm1387',
      chip_count: 189,
      chain_count: 3,
      mode: 'heater',
      hashrate_ghs: 13_500, // 13.5 TH/s
      api_version: '2.0',
      board: 'BHB42601',
      soc: 'zynq',
      hardware: {
        miner_serial: FAKE_SERIAL,
        control_board: 's9',
        hb_type: 'BHB42601',
        chip_type: 'bm1387',
        psu_model: 'APW3',
      },
    },
  }).as('systemInfo');

  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      uptime_s: 3600,
      pool: { url: 'stratum+tcp://solo.ckpool.org:3333', status: 'connected' },
      power: {
        // Board-level (post-PSU output to ASICs): 1000 W
        watts: 1000,
        // Wall reading (what the breaker sees, what the bill is): 1180 W
        // 180 W = APW3 PSU losses, dissipated as heat in the room.
        wall_watts: 1180,
        source: 'pmbus',
        calibrated: false,
        efficiency_jth: 87.4,
      },
      chains: [],
    },
  }).as('stats');

  cy.intercept('GET', '/api/home/status', {
    statusCode: 200,
    body: {
      // No wall_watts in heater status — forces fallback through
      // getWallWatts(stats.power), which is the W8.6 contract.
      power_watts: null,
      wall_watts: null,
      hashrate_ghs: 13_500,
      sats_today: 0,
      target_temp_c: 22,
      room_temp_c: 19,
    },
  }).as('heaterStatus');

  cy.intercept('GET', '/api/status', {
    statusCode: 200,
    body: {
      hashrate_ghs: 13_500,
      uptime_s: 3600,
      accepted: 1,
      rejected: 0,
      pool: { url: 'stratum+tcp://solo.ckpool.org:3333', status: 'connected' },
    },
  });
}

function seedHeaterMode(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'heater',
      minerName: 'Cypress miner',
      // Wall-watts × hours × rate = 1.180 kW × 1 h × $0.10 = $0.118
      electricityRate: 0.1,
      btcPrice: 100_000,
      btcPriceAuto: false,
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'heater-history');
  win.localStorage.setItem('dcentos-nav-heater', 'heater-history');
}

describe('W8.6 — heat credit uses wall watts everywhere economic', () => {
  beforeEach(() => {
    stubCommonRoutes();
    stubHeatFixture();
  });

  it('HeatingValueSummary computes electricity cost from wall watts, not board watts', () => {
    // We visit the History view because that's where HeatingValueSummary is
    // wired in (per W8 wave-9 audit). On environments where the component is
    // also embedded in BasicDashboard, `data-testid` is unique enough.
    cy.visit('/#/heater-history', {
      onBeforeLoad(win) {
        seedHeaterMode(win);
      },
    });

    cy.wait('@stats');
    cy.wait('@heaterStatus');

    // The displayed cost label format is "-$0.12" (with optional line-through
    // when heatingMode=true). The underlying number is what we care about.
    cy.contains(/Electricity cost/i)
      .parent()
      .within(() => {
        // Wall-watts cost = 1.180 kW × 1 h × $0.10 = $0.118 → rounds to "$0.12"
        cy.contains(/\$0\.12/).should('exist');
        // Board-watts cost would be 1.000 kW × 1 h × $0.10 = "$0.10".
        // That value MUST NOT appear here — if it does, we regressed.
        cy.contains(/^-?\$0\.10$/).should('not.exist');
      });
  });
});
