/// <reference types="cypress" />
/// <reference types="cypress-axe" />

import type { Result as AxeViolation } from 'axe-core';
import {
  HACKER_PRIMARY_PAGES,
  HEATER_PAGES,
  STANDARD_PRIMARY_PAGES,
  STANDARD_SETTINGS_SUBPAGES,
} from '../../src/utils/router';

type Mode = 'heater' | 'standard' | 'hacker';

interface RouteCase {
  mode: Mode;
  page: string;
  root: string;
}

const AXE_OPTIONS = {
  runOnly: {
    type: 'tag',
    values: ['wcag2a', 'wcag2aa'],
  },
  includedImpacts: ['serious', 'critical'],
};

const NON_BLOCKING_AXE_RULES = new Set([
  // Kept aligned with layout_smoke.cy.ts: contrast cleanup is tracked as a
  // visual pass. This gate blocks structural serious and critical issues.
  'color-contrast',
]);

const ROUTES: RouteCase[] = [
  ...Array.from(STANDARD_PRIMARY_PAGES).map((page) => ({
    mode: 'standard' as const,
    page,
    root: '.mode-standard',
  })),
  ...STANDARD_SETTINGS_SUBPAGES.map(({ id }) => ({
    mode: 'standard' as const,
    page: id,
    root: '.mode-standard',
  })),
  ...Array.from(HEATER_PAGES).map((page) => ({
    mode: 'heater' as const,
    page,
    root: '.mode-basic',
  })),
  ...Array.from(HACKER_PRIMARY_PAGES).map((page) => ({
    mode: 'hacker' as const,
    page,
    root: '[data-testid="mode-hacker-dashboard"]',
  })),
];

function seedDashboardState(win: Window, route: RouteCase) {
  win.localStorage.clear();
  win.sessionStorage.setItem('hacker-gate-dismissed', '1');
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: route.mode,
      minerName: 'Axe route miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', route.page);
  win.localStorage.setItem(`dcentos-nav-${route.mode}`, route.page);
}

function visitRoute(route: RouteCase) {
  cy.visit(`/#/${route.page}`, {
    onBeforeLoad(win) {
      seedDashboardState(win, route);
    },
  });

  cy.get('main#main-content', { timeout: 10_000 }).should('exist');
  cy.get(route.root, { timeout: 10_000 }).should('exist');
  if (route.page === 'heater-settings') {
    cy.contains('Night Mode', { timeout: 10_000 }).should('be.visible');
    cy.get('.toggle-switch', { timeout: 10_000 }).should('have.attr', 'aria-checked');
  }
}

function assertNoBlockingA11y(label: string) {
  cy.checkA11y(undefined, AXE_OPTIONS, (violations: AxeViolation[]) => {
    const blocking = violations.filter((violation) => !NON_BLOCKING_AXE_RULES.has(violation.id));
    const summary = blocking.map((violation) => {
      const node = violation.nodes[0];
      const failure = node?.failureSummary ? ` (${node.failureSummary})` : '';
      const html = node?.html ? ` html=${node.html}` : '';
      return `${violation.id}: ${violation.help} at ${node?.target.join(' ') ?? 'unknown target'}${failure}${html}`;
    });

    expect(summary, `${label} serious/critical accessibility violations`).to.deep.equal([]);
  }, true);
}

describe('Route-walking accessibility audit', () => {
  ROUTES.forEach((route) => {
    it(`has no serious or critical structural axe violations on ${route.mode}/${route.page}`, () => {
      cy.viewport(1280, 900);
      visitRoute(route);
      cy.injectAxe();
      assertNoBlockingA11y(`${route.mode}/${route.page}`);
    });
  });
});
