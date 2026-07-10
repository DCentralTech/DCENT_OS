/// <reference types="cypress" />

function seedPoolsPage(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Cypress miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'pools');
  win.localStorage.setItem('dcentos-nav-standard', 'pools');
}

describe('API error envelope rendering', () => {
  it('renders daemon suggestions from pool validation errors', () => {
    cy.intercept('POST', '/api/pools', {
      statusCode: 400,
      body: {
        error: 'Pool URL rejected by daemon',
        code: 'pool_validation',
        suggestion: 'Use stratum+tcp://host:port without a path.',
      },
    }).as('savePools');

    cy.visit('/#/pools', { onBeforeLoad: seedPoolsPage });
    cy.contains('Pool Configuration', { timeout: 10_000 }).should('be.visible');

    cy.get('input[placeholder="stratum+tcp://pool.example.com:3333"]')
      .first()
      .clear()
      .type('stratum+tcp://pool.example.com:3333');
    cy.get('input[placeholder="bc1q...worker1"]')
      .first()
      .clear()
      .type('bc1qexample.worker1');

    cy.contains('button', 'Save Pools').click();
    cy.wait('@savePools');
    cy.contains('[role="alert"]', 'Use stratum+tcp://host:port without a path.')
      .should('be.visible');
  });
});
