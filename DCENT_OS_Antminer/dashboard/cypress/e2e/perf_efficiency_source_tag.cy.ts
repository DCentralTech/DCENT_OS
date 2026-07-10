// Cypress e2e — W9.4 J/TH source-tag rendering on EarningsPage.
//
// Background (W9.4): J/TH efficiency was never measured on a live target,
// so the autotuner's "Efficiency" mode was tuning against a modeled
// baseline that may diverge from what an external wattmeter reports.
// `POST /api/perf/calibrate` accepts an operator-supplied wall watts +
// hashrate snapshot and bakes it into the active profile as
// `OperatorConfirmed`. `GET /api/perf/efficiency` then surfaces the J/TH
// number with a source enum so the dashboard can render the headline
// honestly: green = operator-confirmed, amber = PMBus-derived, grey
// italic = model-only.
//
// This test pins the visual states of `data-testid="efficiency-jth-value"`
// and `data-testid="efficiency-jth-source-tag"` against three scenarios:
//   1) source=operator    → green color, no italic, "Operator wattmeter"
//   2) source=pmbus       → amber color, "PSU PMBus"
//   3) source=model       → italic + grey, "Modeled (no wattmeter)"

/// <reference types="cypress" />

const FAKE_SERIAL = 'S9-LAB-W9.4';

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
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.9.0',
      model: 'Antminer S9',
      hostname: FAKE_SERIAL,
      mac: '00:11:22:33:44:55',
      uptime_s: 3600,
      chip_type: 'bm1387',
      chip_count: 189,
      chain_count: 3,
      mode: 'standard',
      hashrate_ghs: 13_500,
      api_version: '2.0',
    },
  });
  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      uptime_s: 3600,
      pool: { url: 'stratum+tcp://solo.ckpool.org:3333', status: 'connected' },
      power: {
        watts: 1000,
        wall_watts: 1180,
        source: 'pmbus',
        calibrated: false,
        efficiency_jth: 87.4,
      },
      chains: [],
    },
  });
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
  cy.intercept('GET', '/api/history', { statusCode: 200, body: { samples: [] } });
  cy.intercept('GET', '/api/history/shares', { statusCode: 200, body: { events: [] } });
}

function seedStandardMode(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Cypress miner',
      electricityRate: 0.1,
      btcPrice: 100_000,
      btcPriceAuto: false,
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'earnings');
}

describe('W9.4 — EarningsPage J/TH source tag', () => {
  beforeEach(() => {
    stubCommonRoutes();
  });

  it('renders operator-confirmed J/TH in green with "Operator wattmeter" tag', () => {
    cy.intercept('GET', '/api/perf/efficiency', {
      statusCode: 200,
      body: {
        j_per_th: 75.2,
        source: 'operator',
        confidence: 'high',
        measured_at_ms: Date.now(),
        operator_wall_watts: 1310,
        operator_hashrate_ths: 17.42,
        jth_target_active: true,
      },
    }).as('perf');

    cy.visit('/#/earnings', {
      onBeforeLoad(win) {
        seedStandardMode(win);
      },
    });
    cy.wait('@perf');

    cy.get('[data-testid="efficiency-jth-value"]')
      .should('have.attr', 'data-source', 'operator')
      .should('have.attr', 'data-confidence', 'high')
      .should('contain.text', '75.2 J/TH')
      .should('have.css', 'font-style', 'normal');

    cy.get('[data-testid="efficiency-jth-source-tag"]')
      .should('have.attr', 'data-source', 'operator')
      .should('contain.text', 'Operator wattmeter');

    // EfficiencyJTH active badge should render.
    cy.get('[data-testid="efficiency-jth-target-active"]').should('exist');
  });

  it('renders PMBus-derived J/TH in amber with "PSU PMBus" tag', () => {
    cy.intercept('GET', '/api/perf/efficiency', {
      statusCode: 200,
      body: {
        j_per_th: 92.5,
        source: 'pmbus',
        confidence: 'high',
        measured_at_ms: Date.now(),
        jth_target_active: false,
      },
    }).as('perf');

    cy.visit('/#/earnings', {
      onBeforeLoad(win) {
        seedStandardMode(win);
      },
    });
    cy.wait('@perf');

    cy.get('[data-testid="efficiency-jth-value"]')
      .should('have.attr', 'data-source', 'pmbus')
      .should('contain.text', '92.5 J/TH')
      .should('have.css', 'font-style', 'normal');

    cy.get('[data-testid="efficiency-jth-source-tag"]')
      .should('have.attr', 'data-source', 'pmbus')
      .should('contain.text', 'PSU PMBus');

    // No JTH-active badge in PMBus-only scenario.
    cy.get('[data-testid="efficiency-jth-target-active"]').should('not.exist');
  });

  it('renders modeled J/TH in italic grey with "Modeled (no wattmeter)" tag', () => {
    cy.intercept('GET', '/api/perf/efficiency', {
      statusCode: 200,
      body: {
        j_per_th: 110.0,
        source: 'model',
        confidence: 'low',
        measured_at_ms: null,
        jth_target_active: false,
      },
    }).as('perf');

    cy.visit('/#/earnings', {
      onBeforeLoad(win) {
        seedStandardMode(win);
      },
    });
    cy.wait('@perf');

    cy.get('[data-testid="efficiency-jth-value"]')
      .should('have.attr', 'data-source', 'model')
      .should('have.attr', 'data-confidence', 'low')
      .should('contain.text', '110.0 J/TH')
      .should('have.css', 'font-style', 'italic');

    cy.get('[data-testid="efficiency-jth-source-tag"]')
      .should('have.attr', 'data-source', 'model')
      .should('contain.text', 'Modeled (no wattmeter)');
  });
});
