/// <reference types="cypress" />

function seedStandard(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem('dcentos-settings', JSON.stringify({
    setupComplete: true,
    mode: 'standard',
    minerName: 'State Matrix Miner',
  }));
}

function visitStandard(route: string) {
  cy.visit(`/#/${route}`, { onBeforeLoad: seedStandard });
}

function delayedOk(body: unknown = {}) {
  return { statusCode: 200, delay: 30_000, body };
}

function stubStatus(body: Record<string, unknown>) {
  cy.intercept('GET', '/api/status', { statusCode: 200, body });
  cy.intercept('GET', '/api/stats', { statusCode: 200, body });
}

function statusBody(overrides: Record<string, unknown> = {}) {
  return {
    hashrate_ghs: 12_000,
    hashrate_5s_ghs: 12_000,
    accepted: 12,
    rejected: 0,
    uptime_s: 3600,
    firmware_version: '0.5.0',
    mode: 'standard',
    chains: [],
    fans: { pwm: 30, rpm: 3000, per_fan: [] },
    pool: {
      url: 'stratum+tcp://pool.d-central.tech:3333',
      status: 'connected',
      difficulty: 131072,
      last_share_at: 0,
      protocol: 'sv1',
      encrypted: false,
      donating: false,
    },
    ...overrides,
  };
}

function stubEmptyEvidenceRoutes() {
  cy.intercept('GET', '/api/status', { statusCode: 200, body: statusBody({ chains: [] }) });
  cy.intercept('GET', '/api/diagnostics/chain*', {
    statusCode: 200,
    body: {
      schema: 'dcentos.diagnostics.chain.v1',
      id: 6,
      observation: { chips_detected: 0, chips_expected: 0, nonces_returning: false },
      verdict: 'no_data',
      repair_action: 'No chain diagnostic row returned by this fixture.',
      break_point_chip_idx: null,
    },
  });
  cy.intercept('GET', '/api/diagnostics/failure_modes', {
    statusCode: 200,
    body: { schema: 'dcentos.diagnostics.failure_modes.v1', count: 0, modes: [] },
  });
  cy.intercept('GET', '/api/diagnostics/shares/local_rejects*', {
    statusCode: 200,
    body: { schema: 'dcentos.local_rejects.v1', ring_capacity: 32, total_seen: 0, returned: 0, rejects: [] },
  });
  cy.intercept('GET', '/api/system/boot_timeline', {
    statusCode: 200,
    body: { schema: 'dcentos.boot_timeline.v1', family: 'unknown', canonical: [], observed: [] },
  });
  cy.intercept('GET', '/api/hardware/pic_info', {
    statusCode: 200,
    body: { schema: 'dcentos.pic_info.v1', count: 0, variants: [], live_per_slot: null, live_per_slot_note: 'fixture empty' },
  });
  cy.intercept('GET', '/api/hardware/psu_catalog', {
    statusCode: 200,
    body: { schema: 'dcentos.psu_catalog.v1', count: 0, models: [] },
  });
  cy.intercept('GET', '/api/cgminer/catalog', {
    statusCode: 200,
    body: { schema: 'dcentos.cgminer.catalog.v1', count: 0, total: 0, set_count: 0, get_count: 0, luxor_extensions: 0, destructive: 0, commands: [] },
  });
  cy.intercept('GET', '/api/diagnostics/recovery_actions', {
    statusCode: 200,
    body: { schema: 'dcentos.recovery_actions.v1', actions: [], cgi_routes: [], log_groups_whitelist: [], uninstall_steps: [], luxos_recovery_requires_auth: true, note: 'fixture empty' },
  });
  cy.intercept('GET', '/api/history/audit*', {
    statusCode: 200,
    body: { schema: 'dcentos.audit.v1', ring_capacity: 128, total_seen: 0, returned: 0, events: [] },
  });
  cy.intercept('GET', '/api/re/catalog/index', {
    statusCode: 200,
    body: { schema: 'dcentos.re.catalog.index.v1', read_only: true, hardware_reads: false, hardware_writes: false, config_writes: false, mining_control: false, source_crate: 'fixture', base_path: '/api/re/catalog', catalogs: [] },
  });
}

function stubEmptyLogsManifest() {
  cy.intercept('GET', '/api/diagnostics/logs/manifest', {
    statusCode: 200,
    body: { schema: 'dcentos.logs.manifest.v1', sources: [], limitations: [] },
  });
}

describe('Standard state matrix', () => {
  describe('loading states', () => {
    it('renders first-load skeletons or loading panels for high-risk standard pages', () => {
      cy.intercept('GET', '/api/status', delayedOk(statusBody()));
      cy.intercept('GET', '/api/stats', delayedOk(statusBody()));
      visitStandard('earnings');
      cy.get('[data-testid="page-skeleton-earnings"]').should('be.visible');

      cy.intercept('GET', '/api/debug/log?lines=200', delayedOk({ lines: [] }));
      stubEmptyLogsManifest();
      visitStandard('logs');
      cy.get('[data-testid="logs-page-loading"]').should('be.visible');

      cy.intercept('GET', '/api/fleet/miners', delayedOk({
        generated_at_ms: Date.now(),
        source: 'api',
        source_label: 'Local miner API',
        demo: false,
        miners: [],
      }));
      visitStandard('fleet');
      cy.get('[data-testid="page-skeleton-fleet"]').should('be.visible');

      cy.intercept('GET', '/api/pools', delayedOk({ pools: [], active: null }));
      visitStandard('pools');
      cy.contains('.state-panel-title', 'Loading pool telemetry').should('be.visible');

      cy.intercept('GET', '/api/diagnostics/failure_modes', delayedOk({ modes: [] }));
      visitStandard('evidence');
      cy.get('[data-testid="page-skeleton-evidence"]').should('be.visible');
    });
  });

  describe('empty and unavailable states', () => {
    it('shows honest empty states without plausible fixture data', () => {
      stubStatus(statusBody({
        hashrate_ghs: 0,
        hashrate_5s_ghs: 0,
        accepted: 0,
        rejected: 0,
        pool: { status: 'disconnected', url: '', difficulty: null },
      }));
      visitStandard('earnings');
      cy.contains('.state-panel-title', 'Miner is not hashing right now').should('be.visible');

      stubEmptyLogsManifest();
      cy.intercept('GET', '/api/debug/log?lines=200', { statusCode: 200, body: { lines: [] } });
      visitStandard('logs');
      cy.get('[data-testid="logs-page-empty"]')
        .should('be.visible')
        .and('contain.text', 'No synthetic logs were generated');

      cy.intercept('GET', '/api/fleet/miners', {
        statusCode: 200,
        body: {
          generated_at_ms: Date.now(),
          source: 'api',
          source_label: 'Local miner API',
          demo: false,
          miners: [],
        },
      });
      cy.intercept('GET', '/api/fleet/pool-stats', { statusCode: 200, body: { stats: { miners: [] } } });
      visitStandard('fleet');
      cy.get('[data-testid="fleet-empty-state"]')
        .should('be.visible')
        .and('contain.text', 'No miners discovered yet');

      cy.intercept('GET', '/api/pools', { statusCode: 200, body: { pools: [], active: null } });
      visitStandard('pools');
      cy.get('[data-testid="pool-config-empty"]')
        .should('be.visible')
        .and('contain.text', 'No pool configured');

      stubEmptyEvidenceRoutes();
      visitStandard('evidence');
      cy.get('[data-testid="evidence-audit-surface"]').should('contain.text', 'live empty');
      cy.get('[data-testid="evidence-diagnostics-surface"]')
        .should('contain.text', 'No chain diagnostics requested')
        .and('contain.text', 'No rows reported by this endpoint');
    });

    it('surfaces endpoint errors without inventing successful data', () => {
      stubEmptyLogsManifest();
      cy.intercept('GET', '/api/debug/log?lines=200', {
        statusCode: 500,
        body: { error: 'debug log unavailable', suggestion: 'Open diagnostics' },
      });
      visitStandard('logs');
      cy.contains('.state-panel-title', 'Could not load logs').should('be.visible');
      cy.contains('.state-panel-message', 'debug log unavailable').should('be.visible');

      cy.intercept('GET', '/api/fleet/miners', {
        statusCode: 503,
        body: { error: 'fleet endpoint unavailable' },
      });
      cy.intercept('GET', '/api/fleet/pool-stats', { statusCode: 503, body: { error: 'pool stats unavailable' } });
      visitStandard('fleet');
      cy.get('[data-testid="fleet-source-notice"]')
        .should('contain.text', 'Fleet API unavailable')
        .and('contain.text', 'Demo miners are hidden');
      cy.get('[data-testid="fleet-empty-state"]').should('be.visible');

      cy.intercept('GET', '/api/pools', {
        statusCode: 500,
        body: { error: 'pool state unavailable' },
      });
      visitStandard('pools');
      cy.contains('.state-panel-title', 'Could not load current pool state').should('be.visible');

      stubEmptyEvidenceRoutes();
      cy.intercept('GET', '/api/history/audit*', {
        statusCode: 500,
        body: { error: 'audit history unavailable' },
      });
      visitStandard('evidence');
      cy.get('[data-testid="evidence-audit-surface"]')
        .should('contain.text', 'Endpoint unavailable')
        .and('contain.text', 'audit history unavailable');
    });
  });
});
