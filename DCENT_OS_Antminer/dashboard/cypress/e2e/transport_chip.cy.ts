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

describe('Transport chip honesty', () => {
  it('shows REST polling when the WebSocket is open but silent', () => {
    cy.visit('/#/dashboard', { onBeforeLoad: seedDashboard });

    cy.get('main#main-content', { timeout: 10_000 }).should('exist');
    cy.get('[data-transport="rest-polling"]', { timeout: 10_000 })
      .should('contain.text', 'POLLING');
    cy.get('[data-transport="ws-live"]').should('not.exist');
  });
});
