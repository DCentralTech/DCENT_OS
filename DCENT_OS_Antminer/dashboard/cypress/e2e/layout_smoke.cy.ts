/// <reference types="cypress" />

type ViewportCase = {
  name: string;
  width: number;
  height: number;
};

type RouteCase = {
  name: string;
  page: string;
  sentinel: string;
};

type ModeCase = {
  name: string;
  mode: 'heater' | 'standard' | 'hacker';
  page: string;
  root: string;
  sentinel: string;
};

type AxeViolation = {
  id: string;
  impact?: string;
  help: string;
  nodes: Array<{
    target: string[];
    failureSummary?: string;
  }>;
};

type AxeResult = {
  violations: AxeViolation[];
};

type WindowWithAxe = Window & {
  axe?: {
    run: (context: Document, options: Record<string, unknown>) => Promise<AxeResult>;
  };
};

const VIEWPORTS: ViewportCase[] = [
  { name: 'phone', width: 390, height: 844 },
  { name: 'tablet', width: 768, height: 1024 },
  { name: 'desktop', width: 1280, height: 800 },
];

const ROUTES: RouteCase[] = [
  { name: 'dashboard', page: 'dashboard', sentinel: '[data-testid="platform-overview-card"]' },
  { name: 'autotuner', page: 'autotuner', sentinel: '[data-testid="autotuner-panel"]' },
  { name: 'system', page: 'system', sentinel: '[data-testid="restore-to-stock-trigger"]' },
];

const MODE_CASES: ModeCase[] = [
  {
    name: 'basic heater',
    mode: 'heater',
    page: 'heater-home',
    root: '[data-testid="mode-basic-dashboard"]',
    sentinel: '.live-asic-visual--heater',
  },
  {
    name: 'standard',
    mode: 'standard',
    page: 'dashboard',
    root: '.mode-standard',
    sentinel: '.standard-top-intelligence-grid',
  },
  {
    name: 'advanced hacker',
    mode: 'hacker',
    page: 'dashboard',
    root: '[data-testid="mode-hacker-dashboard"]',
    sentinel: '.live-asic-visual--hacker',
  },
];

const AXE_OPTIONS = {
  runOnly: {
    type: 'tag',
    values: ['wcag2a', 'wcag2aa'],
  },
};

const NON_BLOCKING_AXE_RULES = new Set([
  // The UI/UX pass owns visual contrast fixes. This smoke gate still fails on
  // structural serious/critical issues while reporting contrast separately.
  'color-contrast',
]);

function seedDashboardState(win: Window, page: string, mode: ModeCase['mode'] = 'standard') {
  win.localStorage.clear();
  win.sessionStorage.setItem('hacker-gate-dismissed', '1');
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode,
      minerName: 'Cypress miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', page);
  win.localStorage.setItem(`dcentos-nav-${mode}`, page);
}

function visitRoute(route: RouteCase) {
  cy.visit(`/#/${route.page}`, {
    onBeforeLoad(win) {
      seedDashboardState(win, route.page);
    },
  });

  cy.get('main#main-content', { timeout: 10_000 }).should('exist');
  cy.get(route.sentinel, { timeout: 10_000 }).should('exist');
}

function visitModeRoute(route: ModeCase) {
  cy.visit(`/?cy-mode=${route.mode}&cy-page=${route.page}#/${route.page}`, {
    onBeforeLoad(win) {
      seedDashboardState(win, route.page, route.mode);
    },
  });

  cy.get('main#main-content', { timeout: 10_000 }).should('exist');
  cy.get(route.root, { timeout: 10_000 }).should('be.visible');
  cy.get(route.sentinel, { timeout: 10_000 }).should('be.visible');
}

function assertNoHorizontalOverflow(label: string) {
  cy.document().then((doc) => {
    const root = doc.documentElement;
    const scrollWidth = Math.max(root.scrollWidth, doc.body.scrollWidth);
    const overflowPx = scrollWidth - root.clientWidth;

    expect(overflowPx, `${label} horizontal overflow`).to.be.lte(4);
  });
}

function assertElementInsideViewport(selector: string, label: string) {
  cy.get('body').then(($body) => {
    if ($body.find(selector).length === 0) {
      return;
    }
    cy.get(selector).each(($el) => {
      const rect = $el[0].getBoundingClientRect();
      cy.window().then((win) => {
        expect(rect.left, `${label} ${selector} left`).to.be.gte(-1);
        expect(rect.right, `${label} ${selector} right`).to.be.lte(win.innerWidth + 1);
      });
    });
  });
}

function assertSkipLinkFocuses() {
  cy.get('a[href="#main-content"]').then(($link) => {
    ($link[0] as HTMLAnchorElement).focus();
  });
  cy.get('a[href="#main-content"]').should('be.visible').and('contain.text', 'Skip to main content');
}

function injectAxe() {
  cy.readFile('node_modules/axe-core/axe.min.js', { log: false }).then((source) => {
    cy.window({ log: false }).then((win) => {
      win.eval(source as string);
    });
  });
}

function runAxeSmoke(label: string) {
  cy.window({ log: false }).then((win) => {
    const axe = (win as WindowWithAxe).axe;
    expect(axe, 'axe injected').to.exist;

    return axe!.run(win.document, AXE_OPTIONS).then((result) => {
      const seriousOrCritical = result.violations.filter((violation) =>
        violation.impact === 'critical' || violation.impact === 'serious',
      );
      const blocking = seriousOrCritical.filter((violation) => !NON_BLOCKING_AXE_RULES.has(violation.id));
      const nonBlocking = seriousOrCritical.filter((violation) => NON_BLOCKING_AXE_RULES.has(violation.id));

      if (nonBlocking.length > 0) {
        Cypress.log({
          name: 'axe follow-up',
          message: nonBlocking.map((violation) => `${violation.id}: ${violation.help}`).join('; '),
        });
      }

      const summary = blocking.map((violation) => {
        const node = violation.nodes[0];
        return `${violation.id}: ${violation.help} at ${node?.target.join(' ') ?? 'unknown target'}`;
      });

      expect(summary, `${label} serious/critical accessibility violations`).to.deep.equal([]);
    });
  });
}

describe('Dashboard layout smoke', () => {
  VIEWPORTS.forEach((viewport) => {
    it(`keeps core routes inside the ${viewport.name} viewport`, () => {
      cy.viewport(viewport.width, viewport.height);

      ROUTES.forEach((route) => {
        visitRoute(route);
        assertNoHorizontalOverflow(`${route.name} ${viewport.name}`);
        assertElementInsideViewport('.top-bar', `${route.name} ${viewport.name}`);
        assertElementInsideViewport('[data-testid="standard-status-footer"]', `${route.name} ${viewport.name}`);
        assertElementInsideViewport('[data-testid="current-block-card"]', `${route.name} ${viewport.name}`);
      });
    });
  });

  VIEWPORTS.forEach((viewport) => {
    it(`keeps all operating modes stable in the ${viewport.name} viewport`, () => {
      cy.viewport(viewport.width, viewport.height);

      MODE_CASES.forEach((route) => {
        visitModeRoute(route);
        assertNoHorizontalOverflow(`${route.name} ${viewport.name}`);
        assertElementInsideViewport(route.root, `${route.name} ${viewport.name}`);
        assertElementInsideViewport(route.sentinel, `${route.name} ${viewport.name}`);
      });
    });
  });

  it('keeps the mobile navigation drawer focus-safe and full width', () => {
    cy.viewport(390, 844);
    visitRoute(ROUTES[0]);

    cy.get('#standard-sidebar')
      .should('not.have.class', 'open')
      .and('have.attr', 'aria-hidden', 'true')
      .and(($sidebar) => {
        expect($sidebar.attr('inert'), 'closed mobile sidebar inert attr').to.not.equal(undefined);
      });

    cy.get('.mobile-menu-btn').should('be.visible').click();
    cy.get('#standard-sidebar')
      .should('have.class', 'open')
      .and('have.attr', 'role', 'dialog')
      .and('have.attr', 'aria-modal', 'true')
      .then(($sidebar) => {
        expect($sidebar[0].getBoundingClientRect().width).to.be.gte(260);
      });
    cy.contains('#standard-sidebar .nav-label', 'Pools').should('be.visible');
    cy.get('.sidebar-mobile-close').should('be.focused').click();
    cy.get('.mobile-menu-btn').should('be.focused');
    cy.get('#standard-sidebar').should('not.have.class', 'open').and('have.attr', 'aria-hidden', 'true');
  });

  [
    VIEWPORTS[0],
    VIEWPORTS[2],
  ].forEach((viewport) => {
    it(`passes dashboard accessibility smoke on ${viewport.name}`, () => {
      cy.viewport(viewport.width, viewport.height);
      visitRoute(ROUTES[0]);
      assertSkipLinkFocuses();
      injectAxe();
      runAxeSmoke(`dashboard ${viewport.name}`);
    });
  });
});
