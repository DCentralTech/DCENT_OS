/// <reference types="cypress" />

function setupNeeded() {
  cy.intercept('GET', '/api/setup/status', {
    statusCode: 200,
    body: {
      needs_setup: true,
      device_ready: false,
      mining_ready: false,
      resume_requires_auth: false,
      steps: ['safety', 'circuit', 'solar_provider', 'password', 'mode', 'pool', 'complete'],
      progress: {
        safety: false,
        circuit: false,
        password: false,
        mode: false,
        pool: false,
        complete: false,
      },
      auth: {
        password_set: false,
        token_issued: false,
        password_opt_out: false,
      },
    },
  }).as('setupStatus');
  cy.intercept('GET', '/api/system/info', {
    statusCode: 200,
    body: { status: 'ok', board: 'am2-s19jpro-zynq' },
  }).as('systemInfo');
}

describe('First-boot wizard - Quick Start path', () => {
  it('reaches Review through the quick rail and applies only asked-for setup writes', () => {
    setupNeeded();

    cy.intercept('POST', '/api/setup/skip-safety', {
      statusCode: 200,
      body: { status: 'ok', safety_opt_out: true },
    }).as('skipSafety');
    cy.intercept('POST', '/api/setup/step1-safety', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('setupSafety');
    cy.intercept('POST', '/api/setup/step2-circuit', {
      statusCode: 200,
      body: { status: 'ok', persisted: true },
    }).as('setupCircuit');
    cy.intercept('POST', '/api/setup/step4-mode', {
      statusCode: 200,
      body: { status: 'ok', persisted: true },
    }).as('setupMode');
    cy.intercept('POST', '/api/setup/step5-pool', {
      statusCode: 200,
      body: { status: 'ok', persisted: true },
    }).as('setupPool');
    cy.intercept('POST', '/api/config/donation', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('donation');
    cy.intercept('POST', '/api/config/psu-override', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('psuOverride');
    cy.intercept('POST', '/api/setup/step-economics', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('economics');
    cy.intercept('POST', '/api/setup/quiet-hours', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('quietHours');
    cy.intercept('POST', '/api/setup/skip-password', {
      statusCode: 200,
      body: { status: 'ok', password_opt_out: true },
    }).as('skipPassword');
    cy.intercept('POST', '/api/setup/complete', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('complete');
    cy.intercept('POST', '/api/action/reboot', {
      statusCode: 200,
      body: { status: 'ok' },
    }).as('reboot');

    cy.visit('/', { onBeforeLoad: win => { win.localStorage.clear(); } });

    cy.contains('button', 'Quick Start').click();

    cy.get('.wiz-rail').within(() => {
      cy.contains('.wiz-step-label', 'Welcome').should('be.visible');
      cy.contains('.wiz-step-label', 'Pool').should('be.visible');
      cy.contains('.wiz-step-label', 'Password').should('be.visible');
      cy.contains('.wiz-step-label', 'Review').should('be.visible');
      cy.contains('.wiz-step-label', 'Network').should('not.exist');
      cy.contains('.wiz-step-label', 'Donation').should('not.exist');
    });

    cy.contains('h2', 'Pool').should('be.visible');
    cy.get('.wiz-footer').contains('button', /Continue/).click();
    cy.contains('h2', 'Set a dashboard password').should('be.visible');
    cy.get('.wiz-footer').contains('button', /Review/).click();

    cy.contains('h2', 'Review').should('be.visible');
    cy.contains('Donation').should('be.visible');
    cy.contains('2.0% voluntary').should('be.visible');
    cy.contains('Deferred - finish later:').should('be.visible');
    cy.contains('Donation stays at the miner default').should('be.visible');

    cy.window().then(win => {
      const saved = JSON.parse(win.localStorage.getItem('dcentos-wizard-state') || '{}');
      expect(saved.setupPath).to.eq('quick');
      expect(saved.currentStepId).to.eq('review');
    });

    cy.get('.wiz-review-ack input[type="checkbox"]').check({ force: true });
    cy.contains('button', 'Save idle setup & reboot').click();

    cy.wait('@skipSafety');
    cy.wait('@setupMode').its('request.body').should('deep.equal', { mode: 'standard' });
    cy.wait('@skipPassword');
    cy.wait('@complete');

    cy.get('@setupSafety.all').should('have.length', 0);
    cy.get('@setupCircuit.all').should('have.length', 0);
    cy.get('@setupPool.all').should('have.length', 0);
    cy.get('@donation.all').should('have.length', 0);
    cy.get('@psuOverride.all').should('have.length', 0);
    cy.get('@economics.all').should('have.length', 0);
    cy.get('@quietHours.all').should('have.length', 0);
  });

  it('migrates legacy persisted wizard state to guided setup', () => {
    setupNeeded();

    cy.visit('/', {
      onBeforeLoad(win) {
        win.localStorage.clear();
        win.localStorage.setItem('dcentos-wizard-state', JSON.stringify({
          currentStep: 2,
          minerName: 'Legacy miner',
          donation: { enabled: true, percent: 2 },
        }));
      },
    });

    cy.contains('h2', 'Set a dashboard password').should('be.visible');
    cy.get('.wiz-rail').within(() => {
      cy.contains('.wiz-step-label', 'Network').should('be.visible');
      cy.contains('.wiz-step-label', 'Mode').should('be.visible');
      cy.contains('.wiz-step-label', 'Donation').should('be.visible');
    });
    cy.window().then(win => {
      const saved = JSON.parse(win.localStorage.getItem('dcentos-wizard-state') || '{}');
      expect(saved.setupPath).to.eq('guided');
      expect(saved.currentStepId).to.eq('password');
    });
  });

  it('surfaces Quick Start deferrals in the post-setup checklist', () => {
    cy.intercept('GET', '/api/setup/status', {
      statusCode: 200,
      body: {
        needs_setup: false,
        device_ready: true,
        mining_ready: false,
        safety_opt_out: true,
        safety_decision_made: true,
        steps: ['pool', 'complete'],
        progress: {
          safety: false,
          circuit: false,
          password: true,
          mode: true,
          pool: false,
          complete: true,
        },
        current: {
          hostname: '',
          mode: 'standard',
          power_source: '',
          pool: { url: '', worker: '' },
        },
      },
    }).as('readinessStatus');
    cy.intercept('GET', '/api/config/power-calibration', {
      statusCode: 200,
      body: {
        enabled: false,
        calibrated: false,
        multiplier: 1,
        reference_wall_watts: null,
        estimated_wall_watts: null,
        estimated_unit_watts: null,
        updated_at_ms: null,
        current_reported_wall_watts: 0,
        current_reported_unit_watts: 0,
        power_source: 'unavailable',
      },
    }).as('powerCalibration');

    cy.visit('/#/settings', {
      onBeforeLoad(win) {
        win.localStorage.clear();
        win.localStorage.setItem('dcentos-settings', JSON.stringify({
          setupComplete: true,
          mode: 'standard',
          minerName: 'My Miner',
        }));
      },
    });

    cy.wait('@readinessStatus');
    cy.wait('@powerCalibration');
    cy.contains('.cp-nextsteps', '4 tasks remaining').should('be.visible');
    cy.contains('.cp-nextsteps-item', 'Configure your payout pool').should('be.visible');
    cy.contains('.cp-nextsteps-item', 'Declare power source and safe limit').should('be.visible');
    cy.contains('.cp-nextsteps-item', 'Name this miner').should('be.visible');
    cy.contains('.cp-nextsteps-item', 'Calibrate wall power').should('be.visible');
    cy.contains('.cp-nextsteps-item', 'Confirm deployment safety details').should('not.exist');

    cy.contains('.cp-nextsteps-item', 'Name this miner').within(() => {
      cy.contains('button', 'Dismiss').click();
    });
    cy.contains('.cp-nextsteps', '3 tasks remaining').should('be.visible');
    cy.reload();
    cy.contains('.cp-nextsteps-item', 'Name this miner').should('not.exist');
  });
});
