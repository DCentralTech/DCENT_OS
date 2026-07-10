/// <reference types="cypress" />

function stubEvidenceRoutes() {
  cy.intercept('GET', '/api/diagnostics/failure_modes', {
    statusCode: 200,
    body: {
      schema: 'dcentos.diagnostics.failure_modes.v1',
      count: 2,
      modes: [
        { mode: 'chain_uart_rx_absent', severity: 'blocker', recovery: 'inspect relay and chain diagnostics' },
        { mode: 'degraded_pic_fw', severity: 'safety', recovery: 'refuse voltage unless lab override is explicit' },
      ],
    },
  }).as('failureModes');

  cy.intercept('GET', '/api/diagnostics/chain?id=6', {
    statusCode: 200,
    body: {
      schema: 'dcentos.diagnostics.chain.v1',
      id: 6,
      observation: {
        chips_detected: 63,
        chips_expected: 63,
        nonces_returning: false,
      },
      verdict: 'uart_rx_absent',
      repair_action: 'collect read-only UART timeline',
      break_point_chip_idx: null,
    },
  }).as('chainDiagnostics');

  cy.intercept('GET', '/api/diagnostics/shares/local_rejects?limit=12', {
    statusCode: 200,
    body: {
      schema: 'dcentos.local_rejects.v1',
      ring_capacity: 32,
      total_seen: 1,
      returned: 1,
      rejects: [
        {
          seq: 1,
          timestamp_ms: 1777999600000,
          chain_id: 6,
          chip_index: 12,
          nonce: 123,
          work_id: 9,
          midstate_idx: 0,
          fpga_work_id_raw: 9,
          generation_age: 2,
          computed_hash_be_first8: [0, 1, 2, 3, 4, 5, 6, 7],
          share_target_be_first8: [255, 255, 255, 255, 0, 0, 0, 0],
          reason: 'stale_job',
        },
      ],
    },
  }).as('localRejects');

  cy.intercept('GET', '/api/system/boot_timeline', {
    statusCode: 200,
    body: {
      schema: 'dcentos.boot_timeline.v1',
      family: 's9',
      canonical: [
        { phase: 'init', at_seconds: 0.5, description: 'init starts' },
        { phase: 'daemon_start', at_seconds: 8, description: 'dcentrald starts' },
      ],
      observed: [],
    },
  }).as('bootTimeline');

  cy.intercept('GET', '/api/hardware/pic_info', {
    statusCode: 200,
    body: {
      schema: 'dcentos.pic_info.v1',
      count: 2,
      variants: [
        {
          fw_byte: '0x03',
          fw_byte_decimal: 3,
          architecture: 's9-pic',
          wire_form: 'short',
          reset_safe: true,
          voltage_trusted: true,
          label: 'S9 PIC',
        },
        {
          fw_byte: '0x86',
          fw_byte_decimal: 134,
          architecture: 'am2-dspic',
          wire_form: 'short',
          reset_safe: false,
          voltage_trusted: false,
          label: 'degraded AM2 dsPIC',
        },
      ],
      live_per_slot: null,
      live_per_slot_note: 'fixture does not include live PIC reads',
    },
  }).as('picInfo');

  cy.intercept('GET', '/api/hardware/psu_catalog', {
    statusCode: 200,
    body: {
      schema: 'dcentos.psu_catalog.v1',
      count: 1,
      models: [
        {
          model: 'apw3',
          voltage_min_v: 10,
          voltage_max_v: 14,
          max_current_a: null,
          max_wattage_220v_w: 1600,
          max_wattage_110v_w: 1200,
          ac_input_min_v: 100,
          ac_input_max_v: 240,
          efficiency_pct: 85,
          has_voltage_feedback: false,
          label: 'APW3',
          compatible_miners: ['s9'],
        },
      ],
    },
  }).as('psuCatalog');

  cy.intercept('GET', '/api/cgminer/catalog', {
    statusCode: 200,
    body: {
      schema: 'dcentos.cgminer.catalog.v1',
      count: 2,
      total: 2,
      set_count: 1,
      get_count: 1,
      luxor_extensions: 0,
      destructive: 1,
      commands: [
        { name: 'summary', kind: 'get', luxor_extension: false, destructive: false, doc: 'read summary' },
        { name: 'restart', kind: 'set', luxor_extension: false, destructive: true, doc: 'restart command catalog entry' },
      ],
    },
  }).as('cgminerCatalog');

  cy.intercept('GET', '/api/diagnostics/recovery_actions', {
    statusCode: 200,
    body: {
      schema: 'dcentos.recovery_actions.v1',
      actions: [
        { action: 'collect_logs', is_destructive: false },
        { action: 'restore_stock_package', is_destructive: true },
      ],
      cgi_routes: [],
      log_groups_whitelist: ['system'],
      uninstall_steps: ['preflight', 'schedule'],
      luxos_recovery_requires_auth: true,
      note: 'Catalog guidance only; execution proof is separate.',
    },
  }).as('recoveryActions');

  cy.intercept('GET', '/api/history/audit?limit=16', {
    statusCode: 200,
    body: {
      schema: 'dcentos.audit.v1',
      ring_capacity: 128,
      total_seen: 1,
      returned: 1,
      events: [
        {
          timestamp_ms: Date.now(),
          schema_version: 1,
          actor: 'dashboard',
          event: { event: 'pool_config_saved', pool_index: 0, proof: 'persisted_config_only' },
        },
      ],
    },
  }).as('auditHistory');

  cy.intercept('GET', '/api/re/catalog/index', {
    statusCode: 200,
    body: {
      schema: 'dcentos.re.catalog.index.v1',
      read_only: true,
      hardware_reads: false,
      hardware_writes: false,
      config_writes: false,
      mining_control: false,
      source_crate: 'dcentrald-diagnostics',
      base_path: '/api/re/catalog',
      catalogs: [
        { name: 'pic', path: '/api/re/catalog/pic', description: 'PIC opcode catalog' },
      ],
    },
  }).as('reCatalog');
}

describe('Evidence page provenance surfacing', () => {
  it('distinguishes live audit/diagnostic rows from static catalogs and absent boot proof', () => {
    stubEvidenceRoutes();

    cy.visit('/#/evidence');
    cy.wait([
      '@failureModes',
      '@chainDiagnostics',
      '@localRejects',
      '@bootTimeline',
      '@picInfo',
      '@psuCatalog',
      '@cgminerCatalog',
      '@recoveryActions',
      '@auditHistory',
      '@reCatalog',
    ]);

    cy.get('[data-testid="evidence-provenance-summary"]').should('contain.text', 'Static catalogs');
    cy.get('[data-testid="evidence-audit-surface"]')
      .should('contain.text', 'live rows')
      .and('contain.text', 'pool_config_saved')
      .and('contain.text', 'persisted_config_only');

    cy.get('[data-testid="evidence-boot-surface"]')
      .should('contain.text', 'canonical catalog')
      .and('contain.text', 'no observed proof')
      .and('contain.text', 'Canonical phases do not prove boot completion');

    cy.get('[data-testid="evidence-diagnostics-surface"]')
      .should('contain.text', 'failure catalog')
      .and('contain.text', 'chain evidence')
      .and('contain.text', 'uart_rx_absent')
      .and('contain.text', 'stale_job');

    cy.get('[data-testid="evidence-catalog-surface"]')
      .should('contain.text', 'PIC catalog')
      .and('contain.text', 'PSU catalog')
      .and('contain.text', 'CGMiner catalog')
      .and('contain.text', '0x86')
      .and('contain.text', 'APW3');

    cy.get('[data-testid="evidence-recovery-surface"]')
      .should('contain.text', 'catalog guidance')
      .and('contain.text', 'Catalog guidance only; execution proof is separate.');
  });
});
