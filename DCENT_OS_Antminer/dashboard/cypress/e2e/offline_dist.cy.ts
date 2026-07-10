/// <reference types="cypress" />

interface OfflineWindow extends Window {
  __dcentOfflineCls?: () => number;
}

function seedOfflineDashboard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Offline artifact',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'dashboard');
  win.localStorage.setItem('dcentos-nav-standard', 'dashboard');
}

function observeCls(win: OfflineWindow) {
  let cls = 0;
  win.__dcentOfflineCls = () => cls;

  if (!('PerformanceObserver' in win)) {
    return;
  }

  try {
    const observer = new win.PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        const layoutShift = entry as PerformanceEntry & {
          hadRecentInput?: boolean;
          value?: number;
        };
        if (!layoutShift.hadRecentInput) {
          cls += layoutShift.value ?? 0;
        }
      }
    });
    observer.observe({ type: 'layout-shift', buffered: true } as PerformanceObserverInit);
  } catch {
    win.__dcentOfflineCls = undefined;
  }
}

describe('Offline dashboard artifact', () => {
  it('opens as a local file with embedded fonts and no remote network resources', () => {
    cy.visit('dist/index.html', {
      onBeforeLoad(win) {
        seedOfflineDashboard(win);
        observeCls(win as OfflineWindow);
      },
    });

    cy.get('main#main-content', { timeout: 10_000 }).should('exist');
    cy.window().then((win) => win.document.fonts.ready);
    cy.document().then((doc) => {
      expect(doc.fonts.check('16px Inter'), 'Inter font face').to.equal(true);
      expect(doc.fonts.check('16px "JetBrains Mono"'), 'JetBrains Mono font face').to.equal(true);
    });

    cy.window().then((win) => {
      const resourceNames = win.performance
        .getEntriesByType('resource')
        .map((entry) => entry.name);
      const remoteResources = resourceNames.filter((name) => {
        if (!/^https?:\/\//i.test(name)) {
          return false;
        }
        return !/^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?\//i.test(name);
      });
      expect(remoteResources, 'remote HTTP(S) resources').to.deep.equal([]);

      const cls = (win as OfflineWindow).__dcentOfflineCls?.() ?? 0;
      expect(cls, 'initial cumulative layout shift').to.equal(0);
    });
  });
});
