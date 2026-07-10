/// <reference types="cypress" />

const liveBlockFixture = {
  status: 'ok',
  read_only: true,
  internet_dependency: false,
  available: true,
  source: 'local_node',
  source_label: 'Bitcoin Core RPC',
  fetched_at_ms: 1_779_999_600_000,
  cache_ttl_ms: 30000,
  block_height: 841234,
  height: 841234,
  block_hash: '0000000000000000000123456789abcdef0123456789abcdef0123456789abcd',
  hash: '0000000000000000000123456789abcdef0123456789abcdef0123456789abcd',
  timestamp_ms: 1_779_999_558_000,
  age_s: 42,
  difficulty: 83_148_355_189_240,
  previous_hash: '00000000000000000000fedcba9876543210fedcba9876543210fedcba9876',
  tx_count: 2891,
  transaction_count: 2891,
  subsidy_btc: 3.125,
  fees_btc: 0.0662,
  reward_btc: 3.1912,
  reward_source: 'local_node',
  mempool: {
    available: true,
    source: 'local_node',
    fee_rate_sat_vb: 18,
    fastest_fee_sat_vb: 24,
    half_hour_fee_sat_vb: 18,
    hour_fee_sat_vb: 12,
    reason: 'Mempool fees from local Bitcoin Core.',
  },
  pool_job: {
    available: true,
    source: 'recent_share_history',
    job_id: 'job-abc123',
    last_share_timestamp_ms: 1_779_999_530_000,
    difficulty: 131072,
    protocol_meta_present: true,
    reason: 'Pool job linked from recent share history.',
  },
  source_manifest: {
    local_node: {
      enabled: true,
      configured: true,
      available: true,
      live_rpc: true,
      endpoint_label: '127.0.0.1:8332',
      credential_mode: 'cookie_file',
      request_timeout_ms: 1500,
      reason: 'Local node is configured and responded.',
    },
    public_fallback: {
      enabled: false,
      available: false,
      reason: 'Public fallback disabled by default.',
    },
    cache: {
      enabled: true,
      ttl_ms: 30000,
      age_ms: 1200,
      reason: 'Fresh cache entry.',
    },
  },
  reasons: [],
  limitations: ['Read-only dashboard surface; no network writes are performed.'],
};

const staleBlockFixture = {
  ...liveBlockFixture,
  status: 'unavailable',
  available: false,
  fetched_at_ms: 1_779_999_500_000,
  cache_ttl_ms: 1000,
  block_height: 841111,
  height: 841111,
  reasons: ['Cached block data expired before the local node responded.'],
  limitations: ['Read-only dashboard surface; stale cache cannot prove current network tip.'],
  source_manifest: {
    local_node: {
      enabled: true,
      configured: true,
      available: false,
      live_rpc: true,
      endpoint_label: '127.0.0.1:8332',
      credential_mode: 'cookie_file',
      request_timeout_ms: 1500,
      reason: 'Local node timed out during the last fetch.',
    },
    public_fallback: {
      enabled: false,
      available: false,
      reason: 'Public fallback disabled by default.',
    },
    cache: {
      enabled: true,
      ttl_ms: 1000,
      age_ms: 45000,
      reason: 'Cached block entry is expired.',
    },
  },
};

function seedDashboardState(win: Window) {
  win.localStorage.clear();
  win.localStorage.setItem(
    'dcentos-settings',
    JSON.stringify({
      setupComplete: true,
      mode: 'standard',
      minerName: 'Cypress miner',
    }),
  );
  win.localStorage.setItem('dcentos-current-page', 'dashboard');
  win.localStorage.setItem('dcentos-nav-standard', 'dashboard');
}

describe('network block card and modal', () => {
  it('renders current block information and opens the source manifest modal', () => {
    cy.intercept('GET', '/api/network/block', {
      statusCode: 200,
      body: liveBlockFixture,
    }).as('networkBlock');

    cy.visit('/#/dashboard', {
      onBeforeLoad(win) {
        seedDashboardState(win);
      },
    });

    cy.wait('@networkBlock');

    cy.get('[data-testid="standard-status-footer"]').should('be.visible');
    cy.get('[data-testid="status-footer-logs"]').should('be.visible').and('have.attr', 'type', 'button');
    cy.get('[data-testid="status-footer-telemetry"]').should('be.visible');
    cy.get('[data-testid="status-footer-pool"]').should('be.visible');
    cy.get('[data-testid="status-footer-hashboards"]').should('be.visible');
    cy.get('[data-testid="status-footer-shares"]').should('be.visible');

    cy.get('[data-testid="current-block-card"]')
      .should('be.visible')
      .and('have.attr', 'aria-haspopup', 'dialog')
      .and('have.attr', 'aria-expanded', 'false')
      .and('contain.text', 'Current Block')
      .and('contain.text', 'Source: Bitcoin Core RPC')
      .and('contain.text', '841,234')
      .and('contain.text', '2,891')
      .and('contain.text', '3.1912 BTC')
      .and('contain.text', 'Node: available')
      .and('contain.text', 'Cache: fresh')
      .and('contain.text', '24 fast / 18 30m / 12 1h sat/vB')
      .click();

    cy.get('[data-testid="current-block-card"]').should('have.attr', 'aria-expanded', 'true');
    cy.get('[data-testid="current-block-close"]').should('be.focused');
    cy.get('body').should('have.css', 'overflow', 'hidden');
    cy.get('[data-testid="current-block-close"]').then($el => {
      const rect = $el[0].getBoundingClientRect();
      expect(rect.width).to.be.gte(44);
      expect(rect.height).to.be.gte(44);
    });

    cy.get('[role="dialog"][aria-label="Bitcoin block source details"]')
      .should('be.visible')
      .and('have.attr', 'aria-modal', 'true')
      .and('have.attr', 'aria-labelledby', 'current-block-title')
      .within(() => {
        cy.get('[data-testid="current-block-modal"]').should('be.visible');
        cy.contains('h2', 'Bitcoin Block Source').should('be.visible');
        cy.contains('.current-block-detail-row', 'Block height').should('contain.text', '841,234');
        cy.contains('.current-block-detail-row', 'Transactions').should('contain.text', '2,891');
        cy.contains('.current-block-detail-row', 'Reward').should('contain.text', '3.1912 BTC');
        cy.contains('.current-block-detail-row', 'Subsidy').should('contain.text', '3.125 BTC');
        cy.contains('.current-block-detail-row', 'Block fees').should('contain.text', '0.0662 BTC');
        cy.contains('.current-block-detail-row', 'Safety').should('contain.text', 'Read-only');
        cy.contains('.current-block-detail-row', 'Cache').should('contain.text', 'fresh');
        cy.contains('.current-block-detail-row', 'Block hash')
          .should('contain.text', '0000000000')
          .find('strong')
          .should('have.attr', 'title', liveBlockFixture.block_hash);
        cy.contains('.current-block-detail-row', 'Latest observed pool job').should('contain.text', 'job-abc123');
        cy.contains('.current-block-detail-row', 'Pool target difficulty').should('contain.text', '131,072');
        cy.contains('.current-block-detail-row', 'RPC endpoint').should('contain.text', '127.0.0.1:8332');
        cy.contains('.current-block-detail-row', 'Credential mode').should('contain.text', 'cookie_file');
        cy.contains('h3', 'Source manifest').scrollIntoView().should('be.visible');
        cy.contains('Local node is configured and responded.').should('be.visible');
        cy.contains('Public fallback disabled by default.').should('be.visible');
        cy.contains('Read-only dashboard surface; no network writes are performed.')
          .scrollIntoView()
          .should('be.visible');
      });

    cy.get('body').type('{esc}');
    cy.get('[role="dialog"][aria-label="Bitcoin block source details"]').should('not.exist');
    cy.focused().should('have.attr', 'data-testid', 'current-block-card');

    cy.get('[data-testid="current-block-card"]').click();
    cy.get('[data-testid="current-block-close"]').click();
    cy.get('[role="dialog"][aria-label="Bitcoin block source details"]').should('not.exist');
  });

  it('flags stale cache state and keeps the block modal usable on phone widths', () => {
    cy.viewport(390, 844);
    cy.intercept('GET', '/api/network/block', {
      statusCode: 200,
      body: staleBlockFixture,
    }).as('networkBlock');

    cy.visit('/#/dashboard', {
      onBeforeLoad(win) {
        seedDashboardState(win);
      },
    });

    cy.wait('@networkBlock');

    cy.get('[data-testid="standard-status-footer"]').should('be.visible');
    cy.get('[data-testid="status-footer-logs"]').should('be.visible');
    cy.get('[data-testid="status-footer-telemetry"]').should('be.visible');
    cy.get('[data-testid="status-footer-pool"]').should('be.visible');
    cy.get('[data-testid="status-footer-hashboards"]').should('be.visible');
    cy.get('[data-testid="status-footer-shares"]').should('be.visible');
    cy.get('[data-testid="status-footer-uptime"]').should('be.visible');
    cy.get('.standard-status-footer-items').should('not.have.attr', 'aria-live');
    cy.get('[data-testid="status-footer-logs"]').then($el => {
      const rect = $el[0].getBoundingClientRect();
      expect(rect.height).to.be.gte(44);
    });

    cy.get('[data-testid="current-block-card"]')
      .should('be.visible')
      .and('have.class', 'current-block-card-warning')
      .and('contain.text', 'Block data unavailable')
      .and('contain.text', 'Cache: expired')
      .and('contain.text', 'Node: configured')
      .click();

    cy.get('[role="dialog"][aria-label="Bitcoin block source details"]')
      .should('be.visible')
      .and('have.attr', 'aria-labelledby', 'current-block-title')
      .within(() => {
        cy.get('[data-testid="current-block-modal"]').should('be.visible');
        cy.contains('.current-block-detail-row', 'Block height').should('contain.text', 'Block data unavailable');
        cy.contains('.current-block-detail-row', 'Block hash').should('contain.text', 'Unavailable');
        cy.contains('.current-block-detail-row', 'Cache').should('contain.text', 'expired');
        cy.contains('.current-block-detail-row', 'Public fallback').should('contain.text', 'Off');
        cy.contains('.current-block-detail-row', 'Safety').should('contain.text', 'Read-only');
        cy.contains('Cached block data expired before the local node responded.').scrollIntoView().should('be.visible');
      });

    cy.document().then(doc => {
      expect(doc.documentElement.scrollWidth).to.be.lte(doc.documentElement.clientWidth + 4);
    });
  });
});
