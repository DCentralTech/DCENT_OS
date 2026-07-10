/// <reference types="cypress" />
/// <reference types="cypress-axe" />

const chipSnapshot = {
  report_id: 'chip-health-cypress',
  generated_at: '2026-07-04T00:00:00Z',
  report_type: 'chip_health',
  source: 'diagnostics',
  total_boards: 1,
  total_chips: 4,
  warnings: [],
  recommendations: [],
  chains: [
    {
      chain_id: 6,
      source: 'diagnostics',
      chip_count: 4,
      responding_chips: 3,
      board_temp_c: 56,
      board_hashrate_ghs: 4000,
      board_health_score: 0.74,
      frequency_mhz: 650,
      voltage_mv: 9100,
      errors: 2,
      status: 'ok',
      chipmap: {
        chain_id: 6,
        chip_count: 4,
        columns: 4,
        rows: 1,
        cells: [
          {
            index: 0,
            address: 0,
            health_score: 0.96,
            grade: 'A',
            color: 'Green',
            frequency_mhz: 650,
            nonce_count: 1200,
            crc_errors: 0,
          },
          {
            index: 1,
            address: 1,
            health_score: 0.62,
            grade: 'C',
            color: 'Yellow',
            frequency_mhz: 625,
            nonce_count: 840,
            crc_errors: 2,
          },
          {
            index: 2,
            address: 2,
            health_score: 0.24,
            grade: 'F',
            color: 'Red',
            frequency_mhz: 580,
            nonce_count: 40,
            crc_errors: 12,
          },
          {
            index: 3,
            address: 3,
            health_score: 0,
            grade: 'X',
            color: 'Gray',
            frequency_mhz: 0,
            nonce_count: 0,
            crc_errors: 0,
          },
        ],
      },
    },
  ],
};

const autotunerHealth = {
  source: 'runtime',
  live_runtime: true,
  stale: false,
  age_s: 1,
  last_update_s: 1_779_999_600,
  message: 'ok',
  total_chips: 3,
  chips: [
    {
      chain_id: 6,
      chip_index: 0,
      health_score: 96,
      trend: 0,
      estimated_days_to_warning: null,
      error_rate_pct: 0.1,
      freq_mhz: 650,
      backoff_count: 0,
      hashrate_ratio: 1,
      status: 'healthy',
    },
    {
      chain_id: 6,
      chip_index: 1,
      health_score: 68,
      trend: -1,
      estimated_days_to_warning: 14,
      error_rate_pct: 2.2,
      freq_mhz: 625,
      backoff_count: 1,
      hashrate_ratio: 0.88,
      status: 'warning',
    },
    {
      chain_id: 6,
      chip_index: 2,
      health_score: 24,
      trend: -2,
      estimated_days_to_warning: 0,
      error_rate_pct: 9.4,
      freq_mhz: 580,
      backoff_count: 4,
      hashrate_ratio: 0.32,
      status: 'failed',
    },
  ],
};

function stubChipHealth() {
  cy.intercept('GET', '/api/chips*', {
    statusCode: 200,
    body: chipSnapshot,
  }).as('chipSnapshot');
  cy.intercept('GET', '/api/autotuner/chip-health', {
    statusCode: 200,
    body: autotunerHealth,
  }).as('autotunerChipHealth');
}

function seedDashboard(win: Window, mode: 'standard' | 'hacker', page: string) {
  win.localStorage.clear();
  win.sessionStorage.setItem('hacker-gate-dismissed', '1');
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode,
      minerName: 'Chip health cypress',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', page);
  win.localStorage.setItem(`dcentos-nav-${mode}`, page);
}

describe('chip health map views', () => {
  it('shows standard health mode with source-labelled no-data cells', () => {
    stubChipHealth();

    cy.visit('/#/dashboard', {
      onBeforeLoad(win) {
        seedDashboard(win, 'standard', 'dashboard');
      },
    });

    cy.contains('.chain-card', 'Hashboard #6', { timeout: 10_000 }).click();
    cy.wait('@chipSnapshot');
    cy.get('.chipmap-mode-switch').contains('button', 'Health').click();

    cy.get('[data-testid="chip-health-legend"]')
      .should('be.visible')
      .and('have.attr', 'data-health-source', 'autotuner')
      .contains('from autotuner grading');
    cy.get('[data-testid="chip-health-cell-6-0"]')
      .should('have.attr', 'data-health-source', 'autotuner')
      .and('have.attr', 'data-health-tone', 'healthy');
    cy.get('[data-testid="chip-health-cell-6-3"]')
      .should('have.attr', 'data-health-source', 'diagnostics')
      .and('have.attr', 'data-health-tone', 'no-data');

    cy.injectAxe();
    cy.checkA11y('[data-testid="chip-health-legend"]');
  });

  it('shows hacker health mode on the advanced chip map', () => {
    stubChipHealth();

    cy.visit('/#/chipmap', {
      onBeforeLoad(win) {
        seedDashboard(win, 'hacker', 'chipmap');
      },
    });

    cy.wait('@chipSnapshot');
    cy.get('.cfm-mode-row').contains('button', /^health$/i).click();

    cy.get('[data-testid="chip-health-legend"]')
      .should('be.visible')
      .and('have.attr', 'data-health-source', 'autotuner')
      .contains('from autotuner grading');
    cy.get('[data-testid="hacker-chip-health-cell-6-2"]')
      .should('have.attr', 'data-health-source', 'autotuner')
      .and('have.attr', 'data-health-tone', 'failing');
    cy.get('[data-testid="hacker-chip-health-cell-6-3"]')
      .should('have.attr', 'data-health-source', 'diagnostics')
      .and('have.attr', 'data-health-tone', 'no-data');

    cy.injectAxe();
    cy.checkA11y('[data-testid="chip-health-legend"]');
  });
});
