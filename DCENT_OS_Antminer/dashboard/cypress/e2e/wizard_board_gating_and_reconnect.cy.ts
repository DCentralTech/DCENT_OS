/// <reference types="cypress" />

// Cypress e2e — first-boot wizard BUG 5 (board-gated PSU Override) + BUG 6
// (bounded reconnect poll — never infinite-loop).
//
// Both bugs surfaced on the live .138 install: DCENT_OS installed onto a
// BraiinsOS Antminer S9 (am1-s9, Zynq, BM1387), the operator opted out of a
// password, set a pool, completed setup — but:
//   BUG 5: the wizard showed the S9 operator the am2/am3 "PSU Override"
//          step (Loki / bare-APW3 / stock-APW12 — an APW12-SMBus concept the
//          S9 does NOT have; the S9 uses PIC16F1704 voltage control).
//   BUG 6: the "Applying configuration" reconnect poll looped forever and
//          never returned to the dashboard (api.reboot() was 403'd with no
//          token → daemon never disconnected; and when bring-up crashed the
//          :8080 API it never recovered → the poll could never observe
//          needs_setup=false on a successful call).
//
// All backend traffic is stubbed via cy.intercept (no live miner).
//
// Manual run:
//   cd DCENT_OS_Antminer/dashboard
//   npm run build && npx cypress run --spec \
//     cypress/e2e/wizard_board_gating_and_reconnect.cy.ts

// STEPS order in SetupWizard.tsx (kit rail is 1:1 with this):
//   0 welcome · 1 network · 2 password · 3 mode · 4 pool · 5 circuit ·
//   6 power · 7 psu_override · 8 donation · 9 home · 10 calibration ·
//   11 name · 12 review
const STEP_POWER = 6;
const STEP_REVIEW = 12;

function stubSystemInfo(board: string) {
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: {
      firmware: 'DCENTos',
      version: '0.9.0',
      model: 'Antminer (BM1387)',
      hostname: 'dcentos',
      mac: '00:11:22:33:44:55',
      uptime_s: 42,
      chip_type: 'BM1387',
      chip_count: 0,
      chain_count: 0,
      mode: 'standard',
      hashrate_ghs: 0,
      api_version: '1.0.0',
      board, // ← the canonical board id the daemon derives (am1-s9 for an S9)
      soc: 'Zynq XC7Z010',
    },
  }).as('systemInfo');
}

function stubSetupNeeded() {
  cy.intercept('GET', '/api/setup/status', {
    statusCode: 200,
    body: {
      needs_setup: true,
      device_ready: false,
      mining_ready: false,
      steps: ['safety', 'circuit', 'password', 'mode', 'pool', 'complete'],
      progress: { safety: false, circuit: false, password: false, mode: false, pool: false, complete: false },
      auth: { password_set: false, token_issued: false, password_opt_out: false },
    },
  }).as('setupStatus');
}

// Seed the wizard onto the Power step (grid selected + a declared circuit) so
// the next Continue click lands on psu_override (S9 skips it) / shows it (am2).
// currentStep != 0 means the setupStatus hydrate effect leaves it alone.
function seedWizardOnPowerStep(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-wizard-state',
    JSON.stringify({
      currentStep: STEP_POWER,
      network: 'eth',
      minerName: 'Cypress S9',
      mode: 'standard',
      powerSource: 'grid',
      circuitVoltage: 240,
      circuitAmperage: 20,
      circuitDerate: 0.8,
      pool: { url: 'stratum+tcp://stratum.braiins.com:3333', worker: 'x.worker', password: 'x' },
      donation: { enabled: true, percent: 2 },
      password: '',
      confirmPassword: '',
      safetyConfirmed: false,
      safetyOptedOut: false,
      psuOverrideEnabled: null,
      psuHardwareVariant: null,
    }),
  );
}

describe('Setup wizard — BUG 5: PSU Override step is board-gated', () => {
  it('am1-s9 (S9) auto-skips the APW/Loki PSU Override step → Continue lands on Donation', () => {
    stubSetupNeeded();
    stubSystemInfo('am1-s9');

    cy.visit('/', { onBeforeLoad: seedWizardOnPowerStep });
    cy.wait('@systemInfo');

    // We are on the Power step.
    cy.contains('.wiz-h2', 'Power source', { timeout: 10000 }).should('be.visible');

    // Advance. On the S9 the psu_override step auto-skips, so we must NOT see
    // the "PSU Override" heading — we land on Donation.
    cy.contains('button', 'Continue →').click();

    cy.contains('.wiz-h2', 'PSU Override').should('not.exist');
    cy.contains('.wiz-h2', 'Optional donation', { timeout: 10000 }).should('be.visible');
  });

  it('am2-s19jpro (Zynq) DOES show the PSU Override step (no regression)', () => {
    stubSetupNeeded();
    stubSystemInfo('am2-s19jpro');

    cy.visit('/', { onBeforeLoad: seedWizardOnPowerStep });
    cy.wait('@systemInfo');

    cy.contains('.wiz-h2', 'Power source', { timeout: 10000 }).should('be.visible');
    cy.contains('button', 'Continue →').click();

    // The am2 board keeps the PSU Override step visible.
    cy.contains('.wiz-h2', 'PSU Override', { timeout: 10000 }).should('be.visible');
  });

  it('unknown board (info fetch fails) keeps the step visible — safe default', () => {
    stubSetupNeeded();
    cy.intercept('GET', '/api/system/info', { statusCode: 500, body: {} }).as('systemInfoFail');

    cy.visit('/', { onBeforeLoad: seedWizardOnPowerStep });

    cy.contains('.wiz-h2', 'Power source', { timeout: 10000 }).should('be.visible');
    cy.contains('button', 'Continue →').click();

    // Unknown board → do NOT hide the (skippable) step.
    cy.contains('.wiz-h2', 'PSU Override', { timeout: 10000 }).should('be.visible');
  });
});

describe('Setup wizard — BUG 6: reconnect poll never infinite-loops', () => {
  // Seed onto the Review step with the safety checkbox unset; the operator
  // opted out of a password (no token), so api.reboot() will 403.
  function seedWizardOnReviewStep(win: Window) {
    win.localStorage.clear();
    win.localStorage.setItem(
      'dcentos-wizard-state',
      JSON.stringify({
        currentStep: STEP_REVIEW,
        network: 'eth',
        minerName: 'Cypress S9',
        mode: 'standard',
        powerSource: null,
        circuitVoltage: null,
        circuitAmperage: null,
        circuitDerate: 0.8,
        pool: { url: 'stratum+tcp://stratum.braiins.com:3333', worker: 'x.worker', password: 'x' },
        donation: { enabled: true, percent: 2 },
        password: '',
        confirmPassword: '',
        safetyConfirmed: false,
        safetyOptedOut: true,
        psuOverrideEnabled: null,
        psuHardwareVariant: null,
      }),
    );
  }

  function stubApplyChain(opts: { complete?: boolean } = {}) {
    const stubComplete = opts.complete ?? true;
    cy.intercept('POST', '/api/setup/skip-safety', { statusCode: 200, body: { status: 'ok', safety_opt_out: true } }).as('skipSafety');
    cy.intercept('POST', '/api/setup/step4-mode', { statusCode: 200, body: { status: 'ok', persisted: true } }).as('mode');
    cy.intercept('POST', '/api/setup/step5-pool', { statusCode: 200, body: { status: 'ok', persisted: true } }).as('pool');
    cy.intercept('POST', '/api/config/donation', { statusCode: 200, body: { status: 'ok' } });
    cy.intercept('POST', '/api/setup/skip-password', { statusCode: 200, body: { status: 'ok', password_opt_out: true } }).as('skipPassword');
    if (stubComplete) {
      cy.intercept('POST', '/api/setup/complete', { statusCode: 200, body: { status: 'ok' } }).as('complete');
    }
    // The .138 failure mode: api.reboot() is 403'd (operator opted out of a
    // password → no token).
    cy.intercept('POST', '/api/action/reboot', { statusCode: 403, body: 'forbidden' }).as('reboot');
  }

  it('redirects to the dashboard when the daemon never disconnects but reports setup-done (opt-out reboot 403)', () => {
    stubSystemInfo('am1-s9');
    stubApplyChain({ complete: false });

    // First the wizard sees needs_setup=true; AFTER complete, the poll sees
    // setup-done — the daemon never went down (reboot was 403'd), so the
    // ORIGINAL gate would only have exited via the slow attempts>=5 fallback.
    // We assert it exits promptly on the setup-done signal.
    let completed = false;
    cy.intercept('POST', '/api/setup/complete', (req) => {
      completed = true;
      req.reply({ statusCode: 200, body: { status: 'ok' } });
    }).as('complete');
    cy.intercept('GET', '/api/setup/status', (req) => {
      req.reply({
        statusCode: 200,
        body: {
          needs_setup: !completed,
          completed_at: completed ? '2026-06-05T00:00:00Z' : null,
          steps: ['safety', 'circuit', 'password', 'mode', 'pool', 'complete'],
          progress: { safety: true, circuit: false, password: true, mode: true, pool: true, complete: completed },
          auth: { password_set: false, token_issued: false, password_opt_out: true },
        },
      });
    }).as('setupStatus');

    cy.visit('/', { onBeforeLoad: seedWizardOnReviewStep });
    cy.wait('@systemInfo');

    cy.contains('.wiz-h2', 'Review', { timeout: 10000 }).should('be.visible');
    cy.get('.wiz-review-ack input[type="checkbox"]').check();
    cy.contains('button', /Save idle setup & reboot|Start mining|Apply & restart/).click();

    // The honest "Applying configuration" overlay appears…
    cy.contains('Applying configuration', { timeout: 10000 }).should('be.visible');
    cy.wait('@complete');

    // …and within the bounded window the wizard hands off to the dashboard
    // (the overlay disappears — onComplete unmounted the wizard).
    cy.contains('Applying configuration', { timeout: 30000 }).should('not.exist');
  });

  it('still redirects to the dashboard within the bounded window when :8080 NEVER recovers', () => {
    stubSystemInfo('am1-s9');
    stubApplyChain();

    // The worst case from the live install: mining bring-up CRASHED the
    // daemon, so /api/setup/status errors on EVERY poll and never recovers.
    // The original gate could never observe needs_setup=false on a success →
    // infinite loop. The HARD BOUND must still hand off to the dashboard.
    cy.intercept('GET', '/api/setup/status', { forceNetworkError: true }).as('setupStatusDead');

    cy.visit('/', { onBeforeLoad: seedWizardOnReviewStep });
    cy.wait('@systemInfo');

    cy.contains('.wiz-h2', 'Review', { timeout: 10000 }).should('be.visible');
    cy.get('.wiz-review-ack input[type="checkbox"]').check();
    cy.contains('button', /Save idle setup & reboot|Start mining|Apply & restart/).click();

    cy.contains('Applying configuration', { timeout: 10000 }).should('be.visible');
    cy.wait('@complete');

    // HARD BOUND: failed-fetch browser cadence is roughly five seconds in
    // Electron, so 12 checks keeps the handoff around one minute. Give the bound
    // generous headroom; the key assertion is it EXITS rather than looping
    // forever. The overlay must disappear (handed off to the dashboard).
    cy.contains('Applying configuration', { timeout: 90000 }).should('not.exist');
  });
});
