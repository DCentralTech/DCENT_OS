/// <reference types="cypress" />

const networkInfo = {
  hostname: 's9-network',
  mac: '00:11:22:33:44:55',
  primary_interface: 'eth0',
  ipv4_cidr: '203.0.113.42/24',
  ipv4: '203.0.113.42',
  ipv6: 'fe80::211:22ff:fe33:4455',
  gateway: '203.0.113.1',
  dns: '1.1.1.1, 8.8.8.8',
  link_state: 'up',
  dhcp: true,
  warnings: [],
};

function visitNetworkSettings() {
  cy.visit('/#/settings', {
    onBeforeLoad(win) {
      win.localStorage.setItem('dcentos_system_tab', 'network');
    },
  });
}

describe('Settings network hostname', () => {
  it('renders network-info fields and saves hostname through the dedicated route', () => {
    cy.intercept('GET', '/api/network/info', {
      statusCode: 200,
      body: networkInfo,
    }).as('networkInfo');
    cy.intercept('POST', '/api/network/hostname', req => {
      expect(req.body).to.deep.equal({ hostname: 'rack-01' });
      req.reply({
        statusCode: 200,
        body: {
          status: 'ok',
          persisted: true,
          hostname: 'rack-01',
          note: 'Saved to daemon config. The active OS hostname updates after restart.',
        },
      });
    }).as('saveHostname');

    visitNetworkSettings();
    cy.wait('@networkInfo');
    cy.get('[data-testid="settings-network-hostname-current"]').should('contain.text', 's9-network');
    cy.contains('203.0.113.42');
    cy.contains('Static IP: configure via your router');

    cy.get('[data-testid="settings-network-hostname-input"]').clear().type('Rack-01');
    cy.get('[data-testid="settings-network-hostname-save"]').click();
    cy.wait('@saveHostname');
    cy.get('[data-testid="settings-network-hostname-current"]').should('contain.text', 'rack-01');
  });

  it('falls back to shared config on older daemons', () => {
    cy.intercept('GET', '/api/network/info', {
      statusCode: 200,
      body: networkInfo,
    }).as('networkInfo');
    cy.intercept('POST', '/api/network/hostname', {
      statusCode: 404,
      body: { error: 'not_found' },
    }).as('missingDedicatedRoute');
    cy.intercept('POST', '/api/config/shared', req => {
      expect(req.body).to.deep.equal({ network: { hostname: 'legacy-01' } });
      req.reply({ statusCode: 200, body: { status: 'ok', persisted: true } });
    }).as('legacySharedConfig');

    visitNetworkSettings();
    cy.wait('@networkInfo');
    cy.get('[data-testid="settings-network-hostname-input"]').clear().type('legacy-01');
    cy.get('[data-testid="settings-network-hostname-save"]').click();
    cy.wait('@missingDedicatedRoute');
    cy.wait('@legacySharedConfig');
  });
});
