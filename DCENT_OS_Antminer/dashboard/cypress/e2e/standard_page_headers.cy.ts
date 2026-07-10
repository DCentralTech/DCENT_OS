/// <reference types="cypress" />

function seedStandard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'Header Miner',
  }));
}

function stubPagePayloads() {
  cy.intercept('GET', '/api/autotuner/visibility', {
    runtime: { available: false, phase: 'idle', source: 'mock', stale: false, age_s: 0 },
    telemetry: { recording: false, run_count: 0 },
    simulation: { available: false },
    saved_profiles: { available: false, expected_chains: 0, reason: 'mock empty', entries: [] },
    rollback: { available: false, reason: 'mock empty', backup_profiles: [] },
    control_actions: false,
    hardware_writes: false,
    source: 'mock',
    limitations: [],
  });
  cy.intercept('GET', '/api/history/audit*', { events: [] });
  cy.intercept('GET', '/api/system/boot_timeline', { canonical: [], observed: [] });
  cy.intercept('GET', '/api/diagnostics/failure_modes', { count: 0, modes: [] });
  cy.intercept('GET', '/api/diagnostics/shares/local_rejects*', { rejects: [] });
  cy.intercept('GET', '/api/hardware/pic_info', { variants: [] });
  cy.intercept('GET', '/api/hardware/psu_catalog', { count: 0, models: [] });
  cy.intercept('GET', '/api/cgminer/catalog', { count: 0, commands: [] });
  cy.intercept('GET', '/api/re/catalog/index', { catalogs: [] });
  cy.intercept('GET', '/api/diagnostics/recovery_actions', {
    actions: [],
    uninstall_steps: [],
    note: 'mock empty',
  });
}

const routes: Array<[route: string, title: string]> = [
  ['dashboard', 'Dashboard'],
  ['pools', 'Pools And Shares'],
  ['earnings', 'Profitability'],
  ['temperature', 'Thermals And Cooling'],
  ['tuning', 'Tuning'],
  ['logs', 'Logs And Events'],
  ['evidence', 'Proof And Catalog Evidence'],
  ['settings', 'Settings'],
  ['energy', 'Energy Tools'],
  ['integrations', 'Integrations'],
  ['fleet', 'Fleet View'],
  ['offgrid', 'Off-Grid'],
  ['profiles', 'Silicon Profiles'],
  ['system', 'System'],
  ['autotuner', 'Autotuner'],
];

describe('Standard page headers', () => {
  routes.forEach(([route, title]) => {
    it(`renders one semantic header on #/${route}`, () => {
      stubPagePayloads();
      cy.visit(`/#/${route}`, { onBeforeLoad: seedStandard });

      cy.get('.standard-page-header h1')
        .should('have.length', 1)
        .and('be.visible')
        .and('have.text', title);
      cy.get('.standard-page-header .cp-status-pill').should('be.visible');
      cy.get('.standard-page-scroll h1').should('have.length', 1);
    });
  });
});
