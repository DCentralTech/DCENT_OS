/// <reference types="cypress" />

// Cypress e2e — first-boot wizard "full-skip freedom" (circuit/safety).
//
// The EXACT parallel of wizard_skip_password_freedom.cy.ts. Covers the
// operator requirement (memory rule
// ): the FULL wizard must
// be skippable straight to the Standard dashboard + logs — not just the
// password but ALSO the circuit/breaker/safety-ack step — with a
// persistent dismissible warning and a Settings page that can complete
// the deferred check later. W1-A's silent api.setupSafety() auto-ack is
// replaced by an HONEST api.skipSafety() opt-out.
//
// All backend traffic is stubbed via cy.intercept (no live miner).
//
// Manual run:
//   cd DCENT_OS_Antminer/dashboard
//   npm run build && npx cypress run --spec \
//     cypress/e2e/wizard_skip_safety_freedom.cy.ts

function setupStatus(opts: {
  passwordOptOut?: boolean;
  passwordSet?: boolean;
  safetyOptOut?: boolean;
  safetyDecisionMade?: boolean;
}) {
  const passwordOptOut = opts.passwordOptOut ?? false;
  const passwordSet = opts.passwordSet ?? false;
  const safetyOptOut = opts.safetyOptOut ?? false;
  // Once acknowledged, the firmware reconciliation clears safety_opt_out
  // and safety_decision_made stays true.
  const safetyDecisionMade =
    opts.safetyDecisionMade ?? (safetyOptOut || false);
  const passwordDecisionMade = passwordOptOut || passwordSet;
  const onboardingDone =
    passwordDecisionMade && (safetyOptOut || safetyDecisionMade);
  cy.intercept('GET', '/api/setup/status', {
    statusCode: 200,
    body: {
      needs_setup: !onboardingDone,
      device_ready: onboardingDone,
      mining_ready: false,
      resume_requires_auth: passwordSet,
      password_opt_out: passwordOptOut,
      password_decision_made: passwordDecisionMade,
      safety_opt_out: safetyOptOut,
      safety_decision_made: safetyDecisionMade,
      steps: ['safety', 'circuit', 'solar_provider', 'password', 'mode', 'pool', 'complete'],
      progress: {
        safety: safetyOptOut || safetyDecisionMade,
        circuit: false,
        password: passwordDecisionMade,
        mode: onboardingDone,
        pool: false,
        complete: onboardingDone,
      },
      auth: {
        password_set: passwordSet,
        token_issued: false,
        password_opt_out: passwordOptOut,
      },
    },
  }).as('setupStatus');
}

describe('First-boot wizard — full-skip freedom (no password AND no circuit check)', () => {
  it('skips the ENTIRE wizard from Welcome with NO password AND NO safety, reaching the dashboard', () => {
    // Fresh unit: setup is needed.
    setupStatus({});

    // THE GAP, FIXED: the skip path must call skip-safety, NOT the
    // silent step1-safety auto-ack.
    cy.intercept('POST', '/api/setup/step1-safety', (req) => {
      // If this fires, the silent auto-ack regressed.
      req.reply({ statusCode: 200, body: { status: 'ok' } });
    }).as('safetyAutoAck');
    cy.intercept('POST', '/api/setup/skip-safety', { statusCode: 200, body: { status: 'ok', safety_opt_out: true } }).as('skipSafety');
    cy.intercept('POST', '/api/setup/step4-mode', { statusCode: 200, body: { status: 'ok', persisted: true } }).as('mode');
    cy.intercept('POST', '/api/setup/skip-password', { statusCode: 200, body: { status: 'ok', password_opt_out: true } }).as('skipPassword');
    cy.intercept('POST', '/api/setup/complete', { statusCode: 200, body: { status: 'ok' } }).as('complete');

    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });

    // The Welcome skip link is the freedom path.
    cy.contains('button', /Skip.*I know.*doing/i).click();

    // Inline non-destructive confirm with equal-weight choices; copy now
    // mentions BOTH the password AND the circuit check.
    cy.contains('Open the dashboard now?').should('be.visible');
    cy.contains('no owner password and no circuit check').should('be.visible');
    cy.contains('button', 'Continue with setup').should('be.visible');
    cy.contains('button', 'Skip — open dashboard').click();

    // The real terminal path: skip-safety → mode → skip-password →
    // complete. CRITICALLY skip-safety (honest opt-out), NOT the silent
    // step1-safety auto-ack.
    cy.wait('@skipSafety');
    cy.wait('@mode');
    cy.wait('@skipPassword');
    cy.wait('@complete');

    cy.get('@skipSafety.all').should('have.length', 1);
    cy.get('@safetyAutoAck.all').should('have.length', 0);
  });

  it('shows BOTH a dismissible "no owner password" AND "circuit check not done" advisory that coexist', () => {
    // Unit that opted out of BOTH a password and the circuit check.
    setupStatus({ passwordOptOut: true, safetyOptOut: true });
    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });

    // Both advisories are surfaced (warning tone — amber, not red), and
    // they coexist independently.
    cy.contains('No owner password is set', { timeout: 10000 }).should('be.visible');
    cy.contains('The circuit/breaker check has not been completed').should('be.visible');
    cy.get('[data-severity="warning"]').should('exist');
    cy.get('[data-severity="critical"]').should('not.exist');

    // Dismiss ONLY the circuit advisory — its dismissal persists and the
    // password advisory must NOT be affected.
    cy.contains('The circuit/breaker check has not been completed')
      .parents('[data-severity="warning"]')
      .find('button[aria-label="Dismiss alert"]')
      .click();
    cy.window().then((win) => {
      expect(
        win.localStorage.getItem('dcentos-dismissed:safety:circuit-check-not-done'),
      ).to.eq('1');
    });
    cy.contains('The circuit/breaker check has not been completed').should('not.exist');
    // The independent no-password advisory is still visible.
    cy.contains('No owner password is set').should('be.visible');
  });

  it('self-clears the circuit advisory once the safety check is acknowledged', () => {
    // Backend now reports safety_opt_out=false + safety_decision_made=true
    // (operator completed it) → the issue is no longer emitted.
    setupStatus({ passwordSet: true, safetyOptOut: false, safetyDecisionMade: true });
    cy.visit('/', { onBeforeLoad: () => { window.localStorage.clear(); } });
    cy.contains('The circuit/breaker check has not been completed').should('not.exist');
    cy.contains('No owner password is set').should('not.exist');
  });

  it('logs + dashboard are reachable with the FULL wizard skipped (no password, no safety)', () => {
    // Both opt-outs ⇒ onboarding complete ⇒ dashboard renders, NOT the
    // wizard. The log-source manifest must be reachable (pre-setup-safe).
    setupStatus({ passwordOptOut: true, safetyOptOut: true });
    cy.intercept('GET', '/api/diagnostics/logs/manifest', {
      statusCode: 200,
      body: {
        status: 'ok',
        read_only: true,
        content_collected: false,
        sources: [{
          id: 'dcentrald',
          label: 'dcentrald log',
          path: '/tmp/dcentrald.log',
          content_endpoint: null,
          content_access: 'not_exposed_metadata_only',
          metadata_status: 'present',
          exists: true,
          size_bytes: 4096,
          modified_ms: 1782831071000,
          limitations: [],
        }],
        limitations: [],
      },
    }).as('logManifest');

    cy.visit('/', {
      onBeforeLoad(win) {
        win.localStorage.clear();
        win.localStorage.setItem(
          'dcentos-settings',
          JSON.stringify({ setupComplete: true, mode: 'standard', minerName: 'Cypress miner' }),
        );
        win.localStorage.setItem('dcentos-current-page', 'logs');
        win.localStorage.setItem('dcentos-nav-standard', 'logs');
      },
    });

    // The wizard must NOT be shown (onboarding complete via both opt-outs).
    cy.contains('DCENT_OS Setup').should('not.exist');
    // The Logs view's read-only metadata manifest is reachable.
    cy.wait('@logManifest');
    cy.contains('Log Sources').should('be.visible');
  });

  it('Settings completes the deferred circuit/safety check (advisory then self-clears)', () => {
    // Opted-out unit, setup done — operator goes to Settings to complete
    // the deferred check (parallel to the W1-A Settings password path).
    setupStatus({ passwordSet: true, safetyOptOut: true });
    cy.intercept('POST', '/api/setup/step1-safety', { statusCode: 200, body: { status: 'ok' } }).as('safetyAck');

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

    // The opted-out context line is shown on the Circuit & Safety section.
    cy.get('#circuit-safety', { timeout: 10000 }).should('exist');
    cy.contains('You chose to run without the circuit/breaker check').should('be.visible');

    // After acknowledging, the backend reports it done so the advisory
    // self-clears.
    setupStatus({ passwordSet: true, safetyOptOut: false, safetyDecisionMade: true });
    cy.get('#circuit-safety').contains('button', 'Complete the circuit & safety check').click();
    cy.wait('@safetyAck');
  });
});
