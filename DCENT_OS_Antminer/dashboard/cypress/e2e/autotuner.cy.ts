// Cypress e2e — AutoTunerPanel (W15-B, wave 15)
//
// Stubs:
//   GET  /api/autotuner/status                  → mock runtime
//   GET  /api/stats                             → 3 chains
//   GET  /api/system/info                       → S9 fixture
//   GET  /api/profiles/silicon                  → 1 imported profile
//   POST /api/autotuner/increment_hashrate_target → applied ack
//   PUT  /api/profiles/silicon/active           → ok
//
// Operator flow:
//   1. Visit #/autotuner
//   2. Assert mode header + 3 chain rows render with freq/voltage/icons
//   3. Click "Hashrate ↑" → assert POST fires + toast appears
//   4. Pick a profile from chain 0 dropdown → assert PUT body shape

/// <reference types="cypress" />

const FAKE_SERIAL = 'S9-LAB-W15B';
const HB_TYPE = 'BHB42601';
const MODEL = 'Antminer S9';
const PROFILE_ID = `antminer_s9__${HB_TYPE}__bm1387__operator_confirmed`;

function stubFleet() {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.5.0',
      model: MODEL,
      hostname: FAKE_SERIAL,
      mac: '00:11:22:33:44:55',
      uptime_s: 3600,
      chip_type: 'bm1387',
      chip_count: 189,
      chain_count: 3,
      mode: 'standard',
      hashrate_ghs: 12000,
      api_version: '2.0',
      board: HB_TYPE,
      soc: 'zynq',
      hardware: { miner_serial: FAKE_SERIAL, control_board: 's9', hb_type: HB_TYPE, chip_type: 'bm1387', psu_model: 'APW3' },
    },
  }).as('systemInfo');

  cy.intercept('GET', '/api/autotuner/status', {
    statusCode: 200,
    body: {
      enabled: true,
      live_runtime: true,
      stale: false,
      age_s: 2,
      source: 'runtime',
      state: 'background_adjust',
      phase: 'background_adjust',
      percent_complete: 100,
      completed_chips: 189,
      active_chips: 189,
      total_chips: 189,
      target_chains: 3,
      tuned_chains: 3,
      failed_chains: 0,
      tuned_chain_ids: [6, 7, 8],
      failed_chain_ids: [],
      last_update_s: Math.floor(Date.now() / 1000),
      message: 'Holding 12.0 TH/s',
      transitions: 3,
      policy: {
        requested_preset: 'balanced_home',
        effective_preset: 'balanced_home',
        active_objective: 'hashrate_target',
      },
    },
  }).as('autotunerStatus');

  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      hashrate_ghs: 12000,
      hashrate_ths: 12,
      uptime_s: 3600,
      fans: { fan1: 3000, fan2: 3000 },
      power: { watts: 1300, efficiency_jth: 108 },
      chains: [
        { id: 6, chips: 63, frequency_mhz: 650, voltage_mv: 9100, voltage_v: 9.1, temp_c: 55, hashrate_ghs: 4000, hashrate_ths: 4, errors: 0, status: 'ok', accepted: 12, rejected: 0, hw_errors: 0 },
        { id: 7, chips: 63, frequency_mhz: 650, voltage_mv: 9100, voltage_v: 9.1, temp_c: 56, hashrate_ghs: 4000, hashrate_ths: 4, errors: 0, status: 'ok', accepted: 11, rejected: 0, hw_errors: 0 },
        { id: 8, chips: 63, frequency_mhz: 650, voltage_mv: 9100, voltage_v: 9.1, temp_c: 54, hashrate_ghs: 4000, hashrate_ths: 4, errors: 0, status: 'ok', accepted: 13, rejected: 0, hw_errors: 0 },
      ],
    },
  }).as('stats');

  cy.intercept('GET', '/api/profiles/silicon', {
    statusCode: 200,
    body: [
      {
        id: PROFILE_ID,
        miner_model: 'antminer_s9',
        hashboard: HB_TYPE,
        chip: 'bm1387',
        source_class: 'operator_confirmed',
        preset_count: 8,
      },
    ],
  }).as('profileList');
}

describe('AutoTunerPanel — W15-B', () => {
  it('renders mode header and per-chain rows from /api/stats + /api/autotuner/status', () => {
    stubFleet();
    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@stats', '@systemInfo', '@profileList']);

    cy.get('[data-testid="autotuner-panel"]').should('exist');
    cy.get('[data-testid="autotuner-current-mode"]').should('contain.text', 'Hashrate');

    // 3 chain rows
    cy.get('[data-testid="autotuner-chain-6"]').should('exist');
    cy.get('[data-testid="autotuner-chain-7"]').should('exist');
    cy.get('[data-testid="autotuner-chain-8"]').should('exist');

    // freq/voltage rendered
    cy.get('[data-testid="chain-6-freq"]').should('contain.text', '650 MHz');
    cy.get('[data-testid="chain-6-voltage"]').should('contain.text', '9.10 V');

    // status icon present (tuned chain → ok)
    cy.get('[data-testid="autotuner-chain-6"]').find('[data-testid="status-icon-ok"]').should('exist');
  });

  it('fires POST /api/autotuner/increment_hashrate_target on Hashrate ↑ click', () => {
    stubFleet();
    cy.intercept('POST', '/api/autotuner/increment_hashrate_target', {
      statusCode: 200,
      body: {
        status: 'increment_hashrate_target',
        mode: { mode: 'hashrate_target', ths: 13 },
        runtime: { status: 'applied', applied_runtime: true, message: 'Live runtime accepted' },
      },
    }).as('hashUp');

    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@stats']);

    cy.get('[data-testid="autotuner-hashrate-up"]').click();
    cy.wait('@hashUp');
    cy.get('[data-testid="autotuner-toast-ok"]').should('contain.text', 'applied');
  });

  it('fires PUT /api/profiles/silicon/active when a chain profile is selected', () => {
    stubFleet();
    cy.intercept('PUT', '/api/profiles/silicon/active', (req) => {
      expect(req.body).to.deep.include({
        model: MODEL,
        hashboard: HB_TYPE,
        profile_id: PROFILE_ID,
      });
      req.reply({
        statusCode: 200,
        body: {
          status: 'ok',
          model: MODEL,
          hashboard: HB_TYPE,
          profile_id: PROFILE_ID,
          note: 'Profile registered',
        },
      });
    }).as('setActive');

    cy.visit('/#/autotuner');
    cy.wait(['@autotunerStatus', '@stats', '@profileList']);

    cy.get('[data-testid="chain-6-profile-select"]').select(PROFILE_ID);
    cy.wait('@setActive');
    cy.get('[data-testid="autotuner-toast-ok"]').should('exist');
  });
});
