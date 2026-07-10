/// <reference types="cypress" />
/// <reference types="cypress-axe" />

type MiningSyncEvent =
  | 'share_accepted'
  | 'share_rejected'
  | 'lucky_share'
  | 'nonce_burst'
  | 'job_received'
  | 'clean_job';

interface MiningSyncOverrides {
  chain_id?: number;
  count?: number;
  intensity?: number;
  difficulty?: number;
  target_difficulty?: number;
  timestamp_ms?: number;
}

const FX_SETTINGS_KEY = 'dcentos-fx-settings';

type ReducedMotionPreference = 'reduce' | 'no-preference';

function emulateReducedMotion(preference: ReducedMotionPreference) {
  return cy.then(() => Cypress.automation('remote:debugger:protocol', {
    command: 'Emulation.setEmulatedMedia',
    params: {
      features: [{ name: 'prefers-reduced-motion', value: preference }],
    },
  }));
}

function dcentFxAnimations(win: Window) {
  return win.document
    .getAnimations()
    .filter((animation) => {
      const target = animation.effect instanceof KeyframeEffect
        ? animation.effect.target
        : null;
      return target instanceof Element && target.closest('[class*="dcfx-"]');
    });
}

function seedDashboard(win: Window, options: { vitality?: 'full' | 'calm' } = {}) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Reward feedback miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'dashboard');
  win.localStorage.setItem('dcentos-nav-standard', 'dashboard');
  if (options.vitality === 'calm') {
    win.localStorage.setItem(FX_SETTINGS_KEY, JSON.stringify({
      enabled: true,
      vitality: 'calm',
      titleTicker: true,
    }));
  }
}

function visitDashboard(options: { vitality?: 'full' | 'calm' } = {}) {
  cy.visit('/#/dashboard', {
    onBeforeLoad(win) {
      seedDashboard(win, options);
    },
  });
  cy.get('main#main-content', { timeout: 10_000 }).should('exist');
  cy.get('[data-testid="kit-stats-kpi-grid"]', { timeout: 10_000 }).should('exist');
  cy.get('[data-testid="kit-hashboard-strip"]', { timeout: 10_000 }).should('exist');
  cy.get('[data-testid="dcfx-layer"]', { timeout: 10_000 }).should('exist');
}

function emitMiningSync(event: MiningSyncEvent, overrides: MiningSyncOverrides = {}) {
  cy.window().then((win) => {
    const helper = win.__dcentCypressWs;
    expect(helper, 'Cypress WebSocket helper').to.exist;
    helper!.emitJson({
      type: 'mining_sync',
      event,
      timestamp_ms: Date.now(),
      chain_id: 6,
      count: 1,
      intensity: event === 'lucky_share' ? 1 : 0.82,
      ...overrides,
    });
  });
}

function burstNonce(count: number) {
  cy.window().then((win) => {
    const helper = win.__dcentCypressWs;
    expect(helper, 'Cypress WebSocket helper').to.exist;
    for (let i = 0; i < count; i += 1) {
      helper!.emitJson({
        type: 'mining_sync',
        event: 'nonce_burst',
        timestamp_ms: Date.now(),
        chain_id: 6,
        count: 1,
        intensity: 0.45 + (i % 5) * 0.1,
      });
    }
  });
}

describe('Reward feedback', () => {
  afterEach(() => {
    emulateReducedMotion('no-preference');
  });

  it('renders and clears real WebSocket share, lucky, and nonce effects', () => {
    visitDashboard();

    emitMiningSync('share_accepted');
    cy.get('[data-transport="ws-live"]', { timeout: 2_000 }).should('contain.text', 'LIVE');
    cy.get('.dcfx-share-flash', { timeout: 1_000 }).should('exist');
    cy.get('.dcfx-share-flash', { timeout: 2_500 }).should('not.exist');

    emitMiningSync('share_rejected', { intensity: 0.7 });
    cy.get('.dcfx-share-reject', { timeout: 1_000 }).should('exist');
    cy.get('.dcfx-share-reject', { timeout: 2_500 }).should('not.exist');

    emitMiningSync('lucky_share', {
      difficulty: 12_482,
      target_difficulty: 512,
    });
    cy.get('.dcfx-moment-lucky', { timeout: 1_000 })
      .should('contain.text', 'Lucky share')
      .and('contain.text', '12,482 achieved / 512 target');
    cy.get('.dcfx-moment-lucky .dcfx-dot').should('have.length', 12);
    cy.get('.dcfx-record-caption').should('contain.text', 'Session best: 12,482');
    cy.injectAxe();
    cy.checkA11y('[data-testid="dcfx-layer"]');
    cy.get('.dcfx-moment-lucky', { timeout: 7_000 }).should('not.exist');

    burstNonce(200);
    cy.get('.dcfx-chain-shimmer', { timeout: 1_000 }).should('exist');
    cy.window().then((win) => {
      const dcfxAnimations = dcentFxAnimations(win);
      expect(dcfxAnimations.length, 'active dcfx animations after nonce storm').to.be.at.most(3);
    });
    cy.get('.dcfx-chain-shimmer', { timeout: 2_500 }).should('not.exist');
  });

  it('does not mount celebration effects during pure polling', () => {
    visitDashboard();

    cy.get('[data-transport="rest-polling"]', { timeout: 10_000 }).should('contain.text', 'POLLING');
    cy.get('.dcfx-share-flash').should('not.exist');
    cy.get('.dcfx-share-reject').should('not.exist');
    cy.get('.dcfx-moment-lucky').should('not.exist');
    cy.get('.dcfx-chain-shimmer').should('not.exist');

    cy.wait(15_000);
    cy.get('.dcfx-share-flash').should('not.exist');
    cy.get('.dcfx-share-reject').should('not.exist');
    cy.get('.dcfx-moment-lucky').should('not.exist');
    cy.get('.dcfx-chain-shimmer').should('not.exist');
  });

  it('keeps calm vitality as a static lucky-share surface', () => {
    visitDashboard({ vitality: 'calm' });

    cy.get('html').should('have.attr', 'data-vitality', 'calm');
    emitMiningSync('lucky_share', {
      difficulty: 2_048,
      target_difficulty: 512,
    });
    cy.get('.dcfx-moment-lucky', { timeout: 1_000 })
      .should('contain.text', 'Lucky share')
      .and('contain.text', '2,048 achieved / 512 target');
    cy.get('.dcfx-particles').should('have.css', 'opacity', '0');
  });

  it('suppresses calm reward animations when reduced motion is requested', () => {
    emulateReducedMotion('reduce');
    visitDashboard({ vitality: 'calm' });

    cy.get('html').should('have.attr', 'data-vitality', 'calm');
    emitMiningSync('lucky_share', {
      difficulty: 2_048,
      target_difficulty: 512,
    });
    cy.get('.dcfx-moment-lucky', { timeout: 1_000 })
      .should('contain.text', 'Lucky share')
      .and('contain.text', '2,048 achieved / 512 target');
    cy.get('.dcfx-particles').should('have.css', 'opacity', '0');
    cy.window().then((win) => {
      expect(win.matchMedia('(prefers-reduced-motion: reduce)').matches).to.eq(true);
      expect(dcentFxAnimations(win), 'active dcfx animations with reduced motion').to.have.length(0);
    });
  });
});
