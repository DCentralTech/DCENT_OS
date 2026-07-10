/// <reference types="cypress" />

function seedCompletedSettings(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'Settings Route Miner',
  }));
}

describe('Settings routed sub-pages', () => {
  it('direct-loads and refreshes a settings sub-route', () => {
    cy.visit('/#/settings/backup', { onBeforeLoad: seedCompletedSettings });

    cy.contains('[role="tab"]', 'Backup & restore')
      .should('have.attr', 'aria-selected', 'true');
    cy.contains('.section-title', 'Backup & Restore').should('be.visible');
    cy.contains('.section-title', 'Firmware Update').should('be.visible');

    cy.reload();
    cy.contains('[role="tab"]', 'Backup & restore')
      .should('have.attr', 'aria-selected', 'true');
    cy.contains('.section-title', 'Backup & Restore').should('be.visible');
  });

  it('keeps tab clicks in the hash route', () => {
    cy.visit('/#/settings/general', { onBeforeLoad: seedCompletedSettings });

    cy.contains('[role="tab"]', 'Security').click();
    cy.hash().should('eq', '#/settings/security');
    cy.contains('[role="tab"]', 'Security')
      .should('have.attr', 'aria-selected', 'true');
    cy.contains('.section-title', 'Security').should('be.visible');
  });

  it('keeps legacy localStorage tab fallback for plain settings routes', () => {
    cy.visit('/#/settings', {
      onBeforeLoad(win) {
        seedCompletedSettings(win);
        win.localStorage.setItem('dcentos_system_tab', 'network');
      },
    });

    cy.contains('[role="tab"]', 'Network')
      .should('have.attr', 'aria-selected', 'true');
    cy.contains('.section-title', 'Network').should('be.visible');
    cy.contains('Static IP: configure via your router').should('be.visible');
  });
});
