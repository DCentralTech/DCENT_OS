/// <reference types="cypress" />

function seedStandard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'Palette Miner',
  }));
  win.localStorage.setItem('dcentos-current-page', 'dashboard');
  win.localStorage.setItem('dcentos-nav-standard', 'dashboard');
}

const destinations: Array<[query: string, label: string, hash: string]> = [
  ['dash', 'Dashboard', '#/dashboard'],
  ['pool', 'Pools', '#/pools'],
  ['earn', 'Earnings', '#/earnings'],
  ['temp', 'Temp & Fans', '#/temperature'],
  ['mini', 'Mining', '#/tuning'],
  ['logs', 'Logs', '#/logs'],
  ['evid', 'Evidence', '#/evidence'],
  ['sett', 'Settings', '#/settings'],
  ['ener', 'Energy Tools', '#/energy'],
  ['inte', 'Integrations', '#/integrations'],
  ['flee', 'Fleet View', '#/fleet'],
  ['offg', 'Off-Grid', '#/offgrid'],
  ['prof', 'Silicon Profiles', '#/profiles'],
  ['syst', 'System', '#/system'],
  ['auto', 'Autotuner', '#/autotuner'],
  ['gene', 'Settings / General', '#/settings/general'],
  ['secu', 'Settings / Security', '#/settings/security'],
  ['netw', 'Settings / Network', '#/settings/network'],
  ['back', 'Settings / Backup', '#/settings/backup'],
  ['appe', 'Settings / Appearance', '#/settings/appearance'],
];

function runPaletteSearch(query: string, label: string) {
  cy.get('button[aria-label="Search pages and glossary"]').click();
  cy.get('input[aria-label="Command palette search"]').should('be.focused');
  cy.wait(80);
  cy.get('input[aria-label="Command palette search"]').type(query, { delay: 10 });
  cy.contains('.cp-palette-opt', label).should('be.visible');
  cy.get('input[aria-label="Command palette search"]').type('{enter}');
}

describe('Standard command palette', () => {
  it('opens from the top bar and Ctrl+K', () => {
    cy.visit('/#/dashboard', { onBeforeLoad: seedStandard });

    cy.get('button[aria-label="Search pages and glossary"]').should('contain.text', 'Ctrl+K').click();
    cy.get('input[aria-label="Command palette search"]').should('be.focused');
    cy.get('body').type('{esc}');

    cy.get('body').type('{ctrl}k');
    cy.get('input[aria-label="Command palette search"]').should('be.focused');
  });

  destinations.forEach(([query, label, hash]) => {
    it(`reaches ${hash} with "${query}" and Enter`, () => {
      cy.visit('/#/dashboard', { onBeforeLoad: seedStandard });
      runPaletteSearch(query, label);
      cy.hash().should('eq', hash);
    });
  });

  it('marks the recovery shortcut as confirm-required', () => {
    cy.visit('/#/dashboard', { onBeforeLoad: seedStandard });

    cy.get('button[aria-label="Search pages and glossary"]').click();
    cy.get('input[aria-label="Command palette search"]').should('be.focused');
    cy.wait(80);
    cy.get('input[aria-label="Command palette search"]').type('danger', { delay: 10 });
    cy.contains('.cp-palette-opt', 'System / Danger Zone').should('be.visible');
    cy.get('input[aria-label="Command palette search"]').type('{enter}');
    cy.contains('.cp-palette-confirm-title', 'Confirm System / Danger Zone').should('be.visible');
  });
});
