/// <reference types="cypress" />

function seedDashboard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Cypress miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'dashboard');
  win.localStorage.setItem('dcentos-nav-standard', 'dashboard');
}

describe('rolling metrics chart source', () => {
  it('labels the hero chart when it uses daemon 1m rolling metrics', () => {
    cy.intercept('GET', '/api/metrics/rolling', {
      statusCode: 200,
      body: {
        now_ms: Date.now(),
        total_samples: 60,
        w5s: {
          window_s: 5,
          sample_count: 5,
          avg_hashrate_ths: 96.2,
          avg_wall_watts: 3180,
          wall_power_sample_count: 5,
          wall_power_measured_sample_count: 0,
          wall_power_modeled_sample_count: 5,
          wall_power_unavailable_sample_count: 0,
          avg_max_chip_temp_c: 62,
          avg_error_rate: 0.001,
          avg_max_fan_pwm: 28,
          accepted_shares: 1,
          rejected_shares: 0,
        },
        w1m: {
          window_s: 60,
          sample_count: 60,
          avg_hashrate_ths: 95.4,
          avg_wall_watts: 3180,
          wall_power_sample_count: 60,
          wall_power_measured_sample_count: 0,
          wall_power_modeled_sample_count: 60,
          wall_power_unavailable_sample_count: 0,
          avg_max_chip_temp_c: 62,
          avg_error_rate: 0.001,
          avg_max_fan_pwm: 28,
          accepted_shares: 5,
          rejected_shares: 0,
        },
        w5m: {
          window_s: 300,
          sample_count: 60,
          avg_hashrate_ths: 94.9,
          avg_wall_watts: 3175,
          wall_power_sample_count: 60,
          wall_power_measured_sample_count: 0,
          wall_power_modeled_sample_count: 60,
          wall_power_unavailable_sample_count: 0,
          avg_max_chip_temp_c: 62,
          avg_error_rate: 0.001,
          avg_max_fan_pwm: 28,
          accepted_shares: 20,
          rejected_shares: 1,
        },
      },
    }).as('rollingMetrics');

    cy.visit('/#/dashboard', {
      onBeforeLoad(win) {
        seedDashboard(win);
      },
    });

    cy.wait('@rollingMetrics');
    cy.contains('Hashrate 1m avg (TH/s)', { timeout: 10_000 }).should('be.visible');
    cy.contains('1m avg from daemon rolling metrics').should('be.visible');
  });
});
