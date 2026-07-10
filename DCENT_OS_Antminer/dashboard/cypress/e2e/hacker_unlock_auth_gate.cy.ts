/// <reference types="cypress" />

function protectedSetupStatus() {
  cy.intercept('GET', '/api/setup/status', {
    statusCode: 200,
    body: {
      needs_setup: false,
      resume_requires_auth: true,
      auth: { password_set: true, token_issued: false, password_opt_out: false },
      progress: { complete: true, password: true },
    },
  }).as('setupStatus');
}

function visitHackerMode() {
  cy.visit('/', {
    onBeforeLoad(win) {
      win.sessionStorage.clear();
      win.localStorage.setItem('dcentos-settings', JSON.stringify({
        mode: 'hacker',
        setupComplete: true,
      }));
      win.localStorage.setItem('dcentos-current-page', 'dashboard');
      win.localStorage.setItem('dcentos-nav-hacker', 'dashboard');
    },
  });
  cy.wait('@setupStatus');
  cy.contains('ADVANCED MODE').should('be.visible');
}

describe('Hacker mode authorization gate', () => {
  it('renders the daemon rejection for a wrong password', () => {
    protectedSetupStatus();
    cy.intercept('POST', '/api/auth/session', {
      statusCode: 401,
      body: 'owner password rejected by daemon',
    }).as('authSession');

    visitHackerMode();

    cy.get('#auth-password').type('wrong-password');
    cy.contains('button', 'Authenticate').click();

    cy.wait('@authSession').its('request.body').should('deep.include', {
      password: 'wrong-password',
      label: 'dashboard',
    });
    cy.contains('owner password rejected by daemon').should('be.visible');
    cy.contains('Server-enforced authorization').should('be.visible');
  });

  it('unlocks after the daemon returns a session token', () => {
    protectedSetupStatus();
    cy.intercept('POST', '/api/auth/session', {
      statusCode: 200,
      body: { session_token: 'session-ok' },
    }).as('authSession');

    visitHackerMode();

    cy.get('#auth-password').type('correct-password');
    cy.contains('button', 'Authenticate').click();

    cy.wait('@authSession');
    cy.contains('Server-enforced authorization').should('not.exist');
    cy.window().its('sessionStorage').invoke('getItem', 'dcentos-session-token').should('eq', 'session-ok');
  });
});
