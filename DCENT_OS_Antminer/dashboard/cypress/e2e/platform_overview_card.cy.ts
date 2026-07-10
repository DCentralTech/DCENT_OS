// Cypress e2e — PlatformOverviewCard (W31, wave 31)
//
// Stubs:
//   GET /api/system/info — Antminer S21 fixture (registered) and a synthetic
//                          "Antminer S99" fixture (unregistered) to exercise
//                          the unregistered-platform fallback path.
//   GET /api/stats        — minimal status snapshot (status != null so
//                           StandardDashboard renders the operations overview
//                           default branch).
//   GET /api/system/health, /api/competitive/readiness, /api/pools/failover
//                         — empty/200 stubs so the dashboard renders without
//                           hitting the network.
//
// Operator flow:
//   1. Visit /#/dashboard with S21 fixture.
//   2. Assert the platform overview card renders with the Amlogic A113D row
//      values (chip = BM1368, chains × chips = 3 × 108) and mode summary.
//   3. Re-visit with an unregistered model.
//   4. Assert the "Unregistered" pill renders and the live API-only details
//      still appear (chip / chains / chip count).

/// <reference types="cypress" />

const REG_SERIAL = 'S21-LAB-W31';
const UNREG_SERIAL = 'S99-LAB-W31';

function stubCommonRoutes() {
  // Wide health/competitive/pool stubs so the page renders without
  // network errors. We don't assert on their content.
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

function stubS21() {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.5.0',
      model: 'Antminer S21',
      hostname: REG_SERIAL,
      mac: '00:11:22:33:44:55',
      uptime_s: 3600,
      chip_type: 'bm1368',
      chip_count: 324,
      chain_count: 3,
      mode: 'standard',
      hashrate_ghs: 200_000,
      api_version: '2.0',
      board: 'BHB68xxx',
      soc: 'amlogic',
      hardware: {
        miner_serial: REG_SERIAL,
        control_board: 's21-amlogic',
        hb_type: 'BHB68xxx',
        chip_type: 'bm1368',
        psu_model: 'APW121215',
      },
    },
  }).as('systemInfo');

  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      hashrate_ghs: 200_000,
      hashrate_ths: 200,
      uptime_s: 3600,
      fans: { fan1: 4500, fan2: 4500 },
      power: { watts: 3500, efficiency_jth: 17.5 },
      chains: [
        { id: 0, chips: 108, frequency_mhz: 475, voltage_mv: 12500, voltage_v: 12.5, temp_c: 56, hashrate_ghs: 66_000, hashrate_ths: 66, errors: 0, status: 'ok', accepted: 30, rejected: 0, hw_errors: 0 },
        { id: 1, chips: 108, frequency_mhz: 475, voltage_mv: 12500, voltage_v: 12.5, temp_c: 57, hashrate_ghs: 67_000, hashrate_ths: 67, errors: 0, status: 'ok', accepted: 29, rejected: 0, hw_errors: 0 },
        { id: 2, chips: 108, frequency_mhz: 475, voltage_mv: 12500, voltage_v: 12.5, temp_c: 55, hashrate_ghs: 67_000, hashrate_ths: 67, errors: 0, status: 'ok', accepted: 31, rejected: 0, hw_errors: 0 },
      ],
    },
  }).as('stats');
}

function stubUnregisteredS99() {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.5.0',
      model: 'Antminer S99',
      hostname: UNREG_SERIAL,
      mac: 'aa:bb:cc:dd:ee:ff',
      uptime_s: 60,
      chip_type: 'bm1499',
      chip_count: 200,
      chain_count: 4,
      mode: 'standard',
      hashrate_ghs: 0,
      api_version: '2.0',
      board: 'unknown',
      soc: 'unknown',
      hardware: {
        miner_serial: UNREG_SERIAL,
        control_board: 'unknown',
        hb_type: 'unknown',
        chip_type: 'bm1499',
      },
    },
  }).as('systemInfo');

  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      hashrate_ghs: 0,
      hashrate_ths: 0,
      uptime_s: 60,
      fans: {},
      power: { watts: 0, efficiency_jth: 0 },
      chains: [],
    },
  }).as('stats');
}

describe('PlatformOverviewCard — W31', () => {
  it('renders the per-platform overview for a registered S21 fixture', () => {
    stubCommonRoutes();
    stubS21();
    cy.visit('/#/dashboard');
    cy.wait(['@systemInfo', '@stats']);

    cy.get('[data-testid="platform-overview-card"]').should('exist');

    cy.get('[data-testid="platform-overview-card"]')
      .should('contain.text', 'Amlogic A113D')
      .and('contain.text', 'BM1368')
      .and('contain.text', '3')
      .and('contain.text', '108')
      .and('contain.text', '324')
      .and('contain.text', '3500 W')
      .and('contain.text', 'Mining mode');
    cy.get('[data-testid="platform-overview-unregistered-pill"]').should('not.exist');

  });

  it('renders the unregistered fallback for an unknown model', () => {
    stubCommonRoutes();
    stubUnregisteredS99();
    cy.visit('/#/dashboard');
    cy.wait(['@systemInfo', '@stats']);

    cy.get('[data-testid="platform-overview-card"]').should('exist');

    cy.get('[data-testid="platform-overview-card"]')
      .should('contain.text', 'Unregistered')
      .and('contain.text', 'bm1499')
      .and('not.contain.text', 'Mining mode');
    // Mode summary block must NOT appear when the profile didn't resolve
    // (we only have rated specs in the registry).
    cy.get('[data-testid="platform-overview-mode-summary"]').should('not.exist');

    // The "Platform" row uses the registered-only data-testid. It should
    // not appear in the unregistered fallback block.
    cy.get('[data-testid="platform-row-platform"]').should('not.exist');
  });
});
