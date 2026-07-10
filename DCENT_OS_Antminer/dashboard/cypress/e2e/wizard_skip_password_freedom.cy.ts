/// <reference types="cypress" />

// Cypress e2e — first-boot wizard "freedom-fix".
//
// Covers the operator requirement: it must be possible to SKIP the wizard
// completely and reach the dashboard WITHOUT being forced to set a password
// — strongly suggested, never forced — with a dismissible, self-clearing
// security advisory and a Settings page that can ACTUALLY set a password.
//
// All backend traffic is stubbed via cy.intercept (no live miner).
//
// Manual run:
//   cd DCENT_OS_Antminer/dashboard
//   npm run build && npx cypress run --spec \
//     cypress/e2e/wizard_skip_password_freedom.cy.ts

function setupNeeded(passwordOptOut = false, passwordSet = false) {
  cy.intercept('GET', '/api/setup/status', {
    statusCode: 200,
    body: {
      needs_setup: !(passwordOptOut || passwordSet),
      device_ready: passwordOptOut || passwordSet,
      mining_ready: false,
      resume_requires_auth: passwordSet,
      password_opt_out: passwordOptOut,
      password_decision_made: passwordOptOut || passwordSet,
      steps: ['safety', 'circuit', 'solar_provider', 'password', 'mode', 'pool', 'complete'],
      progress: {
        safety: passwordOptOut || passwordSet,
        circuit: false,
        password: passwordOptOut || passwordSet,
        mode: passwordOptOut || passwordSet,
        pool: false,
        complete: passwordOptOut || passwordSet,
      },
      auth: {
        password_set: passwordSet,
        token_issued: false,
        password_opt_out: passwordOptOut,
      },
    },
  }).as('setupStatus');
}

describe('First-boot wizard — freedom-fix (skip without a password)', () => {
  it('skips the entire wizard from Welcome with NO password and reaches the dashboard', () => {
    // Fresh unit: setup is needed.
    setupNeeded(false, false);

    cy.intercept('POST', '/api/setup/step1-safety', { statusCode: 200, body: { status: 'ok' } }).as('safetyAutoAck');
    cy.intercept('POST', '/api/setup/skip-safety', { statusCode: 200, body: { status: 'ok', safety_opt_out: true } }).as('skipSafety');
    cy.intercept('POST', '/api/setup/step4-mode', { statusCode: 200, body: { status: 'ok', persisted: true } }).as('mode');
    cy.intercept('POST', '/api/setup/skip-password', { statusCode: 200, body: { status: 'ok', password_opt_out: true } }).as('skipPassword');
    cy.intercept('POST', '/api/setup/complete', { statusCode: 200, body: { status: 'ok' } }).as('complete');

    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });

    // The Welcome skip link is the freedom path. It must NOT detour into
    // a password step.
    cy.contains('button', /Skip.*I know.*doing/i).click();

    // Inline non-destructive confirm appears with equal-weight choices.
    cy.contains('Open the dashboard now?').should('be.visible');
    cy.contains('button', 'Continue with setup').should('be.visible');
    cy.contains('button', 'Skip — open dashboard').click();

    // The real terminal path: skip-safety → mode → skip-password → complete.
    cy.wait('@skipSafety');
    cy.wait('@mode');
    cy.wait('@skipPassword');
    cy.wait('@complete');

    // Critically: NO /api/auth/setup call (no password forced).
    cy.get('@safetyAutoAck.all').should('have.length', 0);
    cy.get('@skipPassword.all').should('have.length', 1);
  });

  it('shows a dismissible (warning, not critical) "no owner password" advisory that self-clears', () => {
    // Unit that already opted out of a password.
    setupNeeded(true, false);
    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });

    // The advisory is surfaced (warning tone — amber, not red).
    cy.contains('No owner password is set', { timeout: 10000 }).should('be.visible');
    cy.get('[data-severity="warning"]').should('exist');
    cy.get('[data-severity="critical"]').should('not.exist');

    // Dismiss it — and the dismissal must persist (localStorage key).
    cy.contains('No owner password is set')
      .parents('[data-severity="warning"]')
      .find('button[aria-label="Dismiss alert"]')
      .click();
    cy.window().then((win) => {
      expect(
        win.localStorage.getItem('dcentos-dismissed:security:no-owner-password'),
      ).to.eq('1');
    });
    cy.contains('No owner password is set').should('not.exist');
  });

  it('self-clears the advisory once a password is set (password supersedes opt-out)', () => {
    // Backend now reports password_set=true → the issue is no longer
    // emitted, so the advisory must not appear at all.
    setupNeeded(false, true);
    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });
    cy.contains('No owner password is set').should('not.exist');
  });

  it('Settings "Set Password" ACTUALLY creates the backend credential (pre-existing bug fix)', () => {
    // Opted-out unit, setup done — operator goes to Settings to add a
    // password. The OLD bug: this only touched the local store and never
    // called /api/auth/setup, leaving the advisory unresolvable.
    setupNeeded(true, false);
    cy.intercept('POST', '/api/auth/setup', {
      statusCode: 200,
      body: { status: 'ok', session_token: 'tok-test-123' },
    }).as('authSetup');

    cy.visit('/', {
      onBeforeLoad(win) {
        win.localStorage.clear();
        win.localStorage.setItem(
          'dcentos-settings',
          JSON.stringify({ setupComplete: true, mode: 'standard', minerName: 'Cypress miner' }),
        );
        win.localStorage.setItem('dcentos-current-page', 'settings');
        win.localStorage.setItem('dcentos-nav-standard', 'settings');
        win.localStorage.setItem('dcentos_system_tab', 'security');
      },
    });

    // The opted-out context line is shown on the Security section.
    cy.get('#security', { timeout: 10000 }).should('exist');
    cy.contains('You chose to run without an owner password').should('be.visible');

    // Set a password — must POST /api/auth/setup with the password body.
    cy.get('#security input[type="password"]').first().type('supersecret8');
    cy.get('#security input[type="password"]').eq(1).type('supersecret8');
    cy.get('#security').contains('button', 'Set Password').click();

    cy.wait('@authSetup').its('request.body').should('deep.include', {
      password: 'supersecret8',
    });
  });
});
