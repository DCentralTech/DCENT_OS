/// <reference types="cypress" />

const donationConfig = {
  enabled: true,
  percent: 2,
  pool_url: 'stratum+tcp://pool.d-central.tech:3333',
  worker: 'DungeonMaster',
  password: 'x',
  fallback_enabled: true,
  fallback_pool_url: 'stratum+tcp://stratum.braiins.com:3333',
  fallback_worker: 'DungeonMaster',
  fallback_password: 'x',
  cycle_duration_s: 3600,
};

function seedSettingsPage(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Cypress miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'settings');
  win.localStorage.setItem('dcentos-nav-standard', 'settings');
  win.localStorage.setItem('dcentos_system_tab', 'general');
}

describe('donation config route', () => {
  it('uses the dedicated donation route without showing fallback copy', () => {
    cy.intercept('GET', '/api/config/donation', {
      statusCode: 200,
      body: { status: 'ok', config: donationConfig, restart_required: false },
    }).as('getDonationConfig');
    cy.intercept('POST', '/api/config/donation', {
      statusCode: 200,
      body: {
        status: 'ok',
        config: { ...donationConfig, enabled: false },
        restart_required: true,
      },
    }).as('postDonationConfig');
    cy.intercept('GET', '/api/donation/info', { statusCode: 404, body: {} });

    cy.visit('/#/settings', { onBeforeLoad: seedSettingsPage });

    cy.wait('@getDonationConfig');
    cy.contains('Donation percent', { timeout: 10_000 }).scrollIntoView().should('be.visible');
    cy.contains('does not expose donation settings yet').should('not.exist');

    cy.contains('button', 'Disable').scrollIntoView().click();
    cy.wait('@postDonationConfig').its('request.body.enabled').should('eq', false);
    cy.contains('does not expose donation settings yet').should('not.exist');
  });
});
