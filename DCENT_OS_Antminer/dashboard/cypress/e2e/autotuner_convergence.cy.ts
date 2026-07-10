/// <reference types="cypress" />

const baseStatus = {
  enabled: true,
  live_runtime: true,
  stale: false,
  age_s: 1,
  source: 'runtime',
  state: 'characterizing',
  phase: 'characterizing',
  percent_complete: 0,
  completed_chips: 1,
  active_chips: 2,
  total_chips: 2,
  target_chains: 1,
  tuned_chains: 0,
  failed_chains: 0,
  tuned_chain_ids: [],
  failed_chain_ids: [],
  estimated_remaining_s: null,
  avg_frequency_mhz: 610,
  efficiency_jth: 95.2,
  policy: {
    requested_preset: 'efficiency',
    effective_preset: 'efficiency',
    active_objective: 'efficiency',
  },
  dispatcher_limits: [],
  last_update_s: 1_779_999_600,
  message: 'Characterizing chain 6',
};

const stats = {
  hashrate_ghs: 4000,
  hashrate_ths: 4,
  uptime_s: 3600,
  thermal: { fan_pwm_pct: 30 },
  chains: [
    {
      id: 6,
      chips: 2,
      frequency_mhz: 610,
      voltage_mv: 9100,
      voltage_v: 9.1,
      temp_c: 55,
      hashrate_ghs: 4000,
      hashrate_ths: 4,
      errors: 0,
      status: 'ok',
      accepted: 12,
      rejected: 0,
      hw_errors: 0,
    },
  ],
};

const systemInfo = {
  firmware: 'dcentos',
  version: '0.5.0',
  model: 'Antminer S9',
  hostname: 's9-autotuner-cypress',
  mac: '00:11:22:33:44:55',
  uptime_s: 3600,
  chip_type: 'bm1387',
  chip_count: 2,
  chain_count: 1,
  mode: 'standard',
  hashrate_ghs: 4000,
  api_version: '2.0',
  board: 'BHB42601',
  soc: 'zynq',
  hardware: {
    miner_serial: 'S9-AUTOTUNER',
    control_board: 's9',
    hb_type: 'BHB42601',
    chip_type: 'bm1387',
    psu_model: 'APW3',
  },
};

const noRunsTelemetry = {
  live_runtime: true,
  recording: false,
  runs: [],
  last_update_s: 1_779_999_600,
  message: 'No completed autotuner characterization telemetry runs captured yet',
  source: 'runtime',
};

const convergingTelemetry = {
  live_runtime: true,
  recording: true,
  runs: [
    {
      started_at: 1_779_990_000,
      duration_s: 120,
      completed: false,
      samples: [
        {
          elapsed_s: 10,
          chain_id: 6,
          board_temp_c: 55.2,
          tuner_state: 'characterizing',
          difficulty: 512,
          chips: [
            { chip_index: 0, nonces: 100, errors: 0, freq_mhz: 600, decision: 'hold' },
            { chip_index: 1, nonces: 80, errors: 2, freq_mhz: 620, decision: 'lower_freq' },
          ],
        },
        {
          elapsed_s: 20,
          chain_id: 6,
          board_temp_c: 55.8,
          tuner_state: 'verifying',
          difficulty: 512,
          chips: [
            { chip_index: 0, nonces: 120, errors: 0, freq_mhz: 610, decision: 'raise_freq' },
            { chip_index: 1, nonces: 100, errors: 0, freq_mhz: 610, decision: 'hold' },
          ],
        },
      ],
    },
  ],
  last_update_s: 1_779_999_620,
  message: 'Autotuner characterization telemetry recording in progress',
  source: 'runtime',
};

function stubAutotuner(statusBody: Record<string, unknown>, telemetryBody: Record<string, unknown>) {
  cy.intercept('GET', '/api/autotuner/status', {
    statusCode: 200,
    body: statusBody,
  }).as('autotunerStatus');
  cy.intercept('GET', '/api/autotuner/telemetry', {
    statusCode: 200,
    body: telemetryBody,
  }).as('autotunerTelemetry');
  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: stats,
  }).as('stats');
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: systemInfo,
  }).as('systemInfo');
  cy.intercept('GET', '/api/profiles/silicon', {
    statusCode: 200,
    body: [],
  }).as('profileList');
}

describe('autotuner convergence timeline', () => {
  it('renders an honest OFF state without fake rows', () => {
    stubAutotuner({
      ...baseStatus,
      enabled: false,
      live_runtime: false,
      state: 'idle',
      phase: 'idle',
      message: 'Autotuner idle.',
    }, convergingTelemetry);

    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@autotunerTelemetry']);
    cy.get('[data-testid="autotuner-convergence-off"]').should('contain.text', 'Autotuner OFF');
    cy.get('[data-testid="autotuner-convergence-table"]').should('not.exist');
    cy.get('[data-transport="rest-polling"]', { timeout: 10_000 }).should('contain.text', 'POLLING');
  });

  it('renders no-runs empty state when telemetry has not recorded a run', () => {
    stubAutotuner({
      ...baseStatus,
      state: 'idle',
      phase: 'idle',
      message: 'Autotuner enabled but idle.',
    }, noRunsTelemetry);

    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@autotunerTelemetry']);
    cy.get('[data-testid="autotuner-convergence-empty"]')
      .should('contain.text', 'No tuning runs recorded yet')
      .and('contain.text', 'CSV export');
    cy.get('[data-testid="autotuner-convergence-table"]').should('not.exist');
  });

  it('renders real converging rows without inventing an ETA', () => {
    stubAutotuner(baseStatus, convergingTelemetry);

    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@autotunerTelemetry']);
    cy.get('[data-testid="autotuner-convergence-table"]')
      .should('have.attr', 'data-row-count', '2');
    cy.get('[data-testid="autotuner-convergence-row-1"]')
      .should('contain.text', 'Step 1')
      .and('contain.text', '610 MHz')
      .and('contain.text', '180 n')
      .and('contain.text', '2 err')
      .and('contain.text', 'lower_freq');
    cy.get('[data-testid="autotuner-convergence-timeline"]')
      .should('contain.text', 'Step 2, target not yet reached.')
      .and('not.contain.text', 'Estimated remaining');
  });
});
