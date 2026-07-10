/// <reference types="cypress" />

function seedHeater(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'heater',
    minerName: 'Heater Journey Miner',
    electricityRate: 0.12,
    electricityRateCalibrated: true,
    btcPrice: 100000,
    btcPriceAuto: false,
  }));
  win.localStorage.setItem('dcentos-current-page', 'heater-home');
  win.localStorage.setItem('dcentos-nav-heater', 'heater-home');
}

function stubCommonHeaterRoutes() {
  cy.intercept('GET', '/api/system/health', {
    statusCode: 200,
    body: { mode: 'native', alive: true, blockers: [] },
  });
  cy.intercept('GET', '/api/dashboard/health', {
    statusCode: 200,
    body: { status: 'ok' },
  });
  cy.intercept('GET', '/api/competitive/readiness', {
    statusCode: 200,
    body: { features: [] },
  });
  cy.intercept('GET', '/api/pools/failover', {
    statusCode: 200,
    body: { primary: null, backup: null, donation: null },
  });
  cy.intercept('GET', '/api/autotuner/status', {
    statusCode: 200,
    body: { enabled: false, live_runtime: false, stale: true, age_s: 0 },
  });
  cy.intercept('GET', '/api/pool/sv2/status', {
    statusCode: 200,
    body: { enabled: false, active: false },
  });
  cy.intercept('GET', '/api/pool/sv2/messages', {
    statusCode: 200,
    body: { messages: [] },
  });
  cy.intercept('GET', '/api/network/block', {
    statusCode: 200,
    body: { height: 850000, source: 'fixture' },
  });
  cy.intercept('GET', '/api/pools', {
    statusCode: 200,
    body: {
      pools: [{
        id: 0,
        url: 'stratum+tcp://pool.example:3333',
        worker: 'bc1qheater.worker',
        password: 'x',
        status: 'mining',
        priority: 0,
        difficulty: 512,
        accepted: 12,
        rejected: 0,
        last_share_s: 12,
        stratum_active: true,
      }],
    },
  });
  cy.intercept('GET', '/api/home/presets', {
    statusCode: 200,
    body: {
      presets: [
        { name: 'quiet', watts: 650, label: 'Quiet' },
        { name: 'balanced', watts: 900, label: 'Balanced' },
        { name: 'boost', watts: 1180, label: 'Boost' },
      ],
    },
  });
  cy.intercept('GET', '/api/home/night-mode', {
    statusCode: 200,
    body: { enabled: false, active: false, start: '22:00', end: '07:00' },
  });
}

function stubHeaterTelemetry(withProvenance = true) {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'dcentos',
      version: '0.9.0',
      model: 'Antminer S9',
      hostname: 'heater-journey',
      mac: '00:11:22:33:44:55',
      uptime_s: 3600,
      chip_type: 'bm1387',
      chip_count: 189,
      chain_count: 3,
      mode: 'heater',
      hashrate_ghs: 13500,
      api_version: '2.0',
      hardware: {
        miner_serial: 'S9-HEATER-JOURNEY',
        control_board: 's9',
        chip_type: 'bm1387',
        capabilities: { sleep_wake_supported: false },
      },
    },
  });
  cy.intercept('GET', '/api/status', {
    statusCode: 200,
    body: {
      hashrate_ghs: 13500,
      uptime_s: 3600,
      accepted: 12,
      rejected: 0,
      pool: { url: 'stratum+tcp://pool.example:3333', status: 'mining' },
      chains: [{ id: 0, status: 'active', temp_c: 55 }, { id: 1, status: 'active', temp_c: 54 }],
    },
  });
  cy.intercept('GET', '/api/stats', {
    statusCode: 200,
    body: {
      uptime_s: 3600,
      power: {
        watts: 1080,
        wall_watts: 1180,
        source: withProvenance ? 'pmbus' : 'static_model_fallback',
        source_detail: withProvenance ? 'pmbus_measured' : 'static_power_fallback_from_miner_state',
        live_power_available: withProvenance,
        calibrated: false,
        efficiency_jth: 87.4,
      },
      chains: [],
    },
  });
  cy.intercept('GET', '/api/home/status', {
    statusCode: 200,
    body: {
      power_watts: 1080,
      wall_watts: 1180,
      btu_h: 4027,
      source: withProvenance ? 'pmbus' : 'static_model_fallback',
      power_source_detail: withProvenance ? 'pmbus_measured' : 'static_power_fallback_from_miner_state',
      live_power_available: withProvenance,
      power_modeled: !withProvenance,
      power_note: withProvenance ? 'PMBus measured power' : 'legacy-daemon fallback',
      noise_db: 48,
      noise_source: 'tach_estimate',
      airflow_cfm: 130,
      preset: 'balanced',
      room_temp_c: 20,
      cost_today_usd: 1.12,
      sats_today: 21,
      sats_today_calibrated: true,
      network_difficulty: 83100000000000,
    },
  }).as('heaterStatus');
  cy.intercept('GET', '/api/home/history', {
    statusCode: 200,
    body: {
      interval_s: 300,
      history: [
        {
          timestamp: Math.floor(Date.now() / 1000) - 600,
          hashrate_ghs: 13200,
          temp_c: 54,
          power_watts: 1180,
          power_source: withProvenance ? 'pmbus' : 'static_model_fallback',
          power_source_detail: withProvenance ? 'pmbus_measured' : 'static_power_fallback_from_miner_state',
          live_power_available: withProvenance,
          fan_rpm: 4200,
        },
      ],
    },
  });
}

describe('Heater journey', () => {
  beforeEach(() => {
    stubCommonHeaterRoutes();
  });

  it('walks home to history to settings with quiet-boot and power provenance visible', () => {
    stubHeaterTelemetry(true);
    cy.visit('/#/heater-home', { onBeforeLoad: seedHeater });
    cy.wait('@heaterStatus');

    cy.contains('Cut power before noise').should('be.visible');
    cy.contains('Live wall power').should('be.visible');
    cy.contains('1,180').should('be.visible');

    cy.contains('button', 'History').click();
    cy.hash().should('eq', '#/heater-history');
    cy.contains('Heat & Earnings History').should('be.visible');
    cy.contains('HEAT OUTPUT').should('be.visible');
    cy.contains('PMBus measured power').scrollIntoView().should('be.visible');

    cy.contains('button', 'Settings').click();
    cy.hash().should('eq', '#/heater-settings');
    cy.contains('Temperature & rate').should('be.visible');
    cy.contains('Power Budget').should('be.visible');
  });

  it('keeps legacy power fallback labelled instead of calling it live', () => {
    stubHeaterTelemetry(false);
    cy.visit('/#/heater-home', { onBeforeLoad: seedHeater });
    cy.wait('@heaterStatus');

    cy.contains('Heat output estimate').should('be.visible');
    cy.contains('Modeled fallback estimate').should('be.visible');
    cy.contains('PMBus measured power').should('not.exist');
  });
});
