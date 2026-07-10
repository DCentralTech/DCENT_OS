/// <reference types="cypress" />

const WATCH_KEY = 'dcentos-first-share-watch';
const BASELINE_KEY = 'dcentos-first-share-watch-baseline';

type Phase = 0 | 1 | 2;

function statusForPhase(phase: Phase) {
  const accepted = phase >= 2 ? 1 : 0;
  const poolStatus = phase === 0 ? 'connecting' : phase === 1 ? 'authorized' : 'mining';
  return {
    hashrate_ghs: phase === 0 ? 0 : 12_000,
    hashrate_5s_ghs: phase === 0 ? 0 : 12_000,
    accepted,
    rejected: 0,
    uptime_s: 240,
    firmware_version: '0.5.0',
    mode: 'standard',
    chains: [
      {
        id: 6,
        chips: 63,
        frequency_mhz: 650,
        voltage_mv: 9100,
        temp_c: 55,
        hashrate_ghs: phase === 0 ? 0 : 12_000,
        errors: 0,
        status: phase === 0 ? 'idle' : 'active',
      },
    ],
    fans: { pwm: 30, rpm: 3000, per_fan: [] },
    pool: {
      url: 'stratum+tcp://pool.example.com:3333',
      status: poolStatus,
      difficulty: 512,
      last_share_s: accepted > 0 ? 8 : 0,
      protocol: 'sv1',
      encrypted: false,
      donating: false,
      failover: {
        configured_pool_count: 1,
        active_pool_index: 0,
        active_pool_priority: 0,
        active_pool_url: 'stratum+tcp://pool.example.com:3333',
        consecutive_failures: 0,
        switch_count: 0,
        stale_jobs_flushed_on_switch: false,
        pending_submit_correlations_cleared: 0,
        pending_share_preserved: false,
        backoff_ms: 0,
        shares_unresolved: phase === 1 ? 1 : 0,
        event: 'fixture',
        telemetry_source: 'cypress',
      },
    },
  };
}

function postureForPhase(phase: Phase) {
  const jobAvailable = phase >= 1;
  const accepted = phase >= 2 ? 1 : 0;
  return {
    schema: 'dcentos.mining.work.posture.v1',
    status: phase === 0 ? 'connecting' : phase === 1 ? 'mining_capable' : 'active',
    read_only: true,
    control_actions: false,
    hardware_writes: false,
    filesystem_mutation: false,
    telemetry_source: 'cypress',
    source: 'fixture',
    mode: 'standard',
    generated_at_s: 1,
    fetched_at_ms: Date.now(),
    pool: {
      available: true,
      url: 'stratum+tcp://pool.example.com:3333',
      status: phase === 0 ? 'connecting' : phase === 1 ? 'authorized' : 'mining',
      active: phase >= 1,
      connected: phase >= 1,
      connecting: phase === 0,
      mining_capable: phase >= 1,
      published_authorized: phase >= 1,
      protocol: 'sv1',
      encrypted: false,
      pool_target_difficulty: 512,
      difficulty: 512,
      last_accepted_share_s: accepted > 0 ? 8 : null,
      telemetry_source: 'fixture',
      health_limitations: [],
      no_notify_age_s: jobAvailable ? 4 : null,
      failover_policy: 'observability_only',
      auto_fallback_active: false,
    },
    protocol: {
      name: 'sv1',
      encrypted: false,
      source: 'fixture',
      reason: 'fixture',
    },
    donation: {
      active: false,
      source: 'fixture',
      reason: 'fixture',
    },
    sv2: {
      available: false,
      encrypted: false,
      session: null,
      source: 'fixture',
      reason: 'fixture',
    },
    job_declaration: {
      available: false,
      enabled: false,
      configured: false,
      connected: false,
      source: 'fixture',
      reason: 'fixture',
    },
    jobs: {
      available: jobAvailable,
      current_job_available: jobAvailable,
      latest_observed_job_id: jobAvailable ? 'job-abc' : null,
      latest_observed_job_age_s: jobAvailable ? 4 : null,
      latest_observed_job_source: jobAvailable ? 'mining_sync' : 'not_persisted',
      recent_job_ids: jobAvailable ? ['job-abc'] : [],
      reason: 'fixture',
    },
    work: {
      available: true,
      active_hashrate: phase >= 1,
      hashrate_ghs: phase >= 1 ? 12_000 : 0,
      hashrate_5s_ghs: phase >= 1 ? 12_000 : 0,
      current_notify_age_s: jobAvailable ? 4 : null,
      work_ring_occupancy: null,
      dispatch_queue_depth: null,
      source: 'fixture',
      reason: 'fixture',
    },
    shares: {
      available: true,
      accepted_total: accepted,
      rejected_total: 0,
      accept_rate_pct: accepted > 0 ? 100 : 0,
      reject_rate_pct: 0,
      recent_count: accepted > 0 ? 1 : 0,
      latest_event_age_s: accepted > 0 ? 8 : null,
      recent_events: accepted > 0 ? shareEvents() : [],
      source: 'fixture',
      reason: 'fixture',
    },
    limitations: [],
  };
}

function shareEvents() {
  return [
    {
      timestamp_ms: Date.now(),
      result: 'accepted',
      job_id: 'job-abc',
      difficulty: 12482,
      target_difficulty: 512,
    },
  ];
}

function installWatchFixtures(getPhase: () => Phase) {
  cy.intercept('GET', '/api/status', req => {
    req.reply({ statusCode: 200, body: statusForPhase(getPhase()) });
  }).as('status');
  cy.intercept('GET', '/api/stats', req => {
    req.reply({ statusCode: 200, body: statusForPhase(getPhase()) });
  });
  cy.intercept('GET', '/api/mining/work/posture', req => {
    req.reply({ statusCode: 200, body: postureForPhase(getPhase()) });
  }).as('posture');
  cy.intercept('GET', '/api/history/shares', req => {
    req.reply({ statusCode: 200, body: { events: getPhase() >= 2 ? shareEvents() : [] } });
  }).as('shareHistory');
}

function armWatch(win: Window) {
  win.localStorage.setItem(WATCH_KEY, 'pending');
  win.localStorage.removeItem(BASELINE_KEY);
}

describe('First-Share Watch', () => {
  it('advances from real posture fields and shows the first accepted share once', () => {
    const phase = { current: 0 as Phase };
    installWatchFixtures(() => phase.current);

    cy.visit('/', { onBeforeLoad: armWatch });
    cy.wait('@status');
    cy.wait('@posture');

    cy.get('[data-testid="first-share-watch-card"]').should('be.visible');
    cy.get('[data-testid="first-share-step-telemetry"]').should('have.class', 'is-done');
    cy.get('[data-testid="first-share-step-connected"]').should('have.class', 'is-current');
    cy.window().should(win => {
      const baseline = JSON.parse(win.localStorage.getItem(BASELINE_KEY) || '{}') as { accepted?: number };
      expect(baseline.accepted).to.eq(0);
    });

    cy.then(() => {
      phase.current = 1;
    });
    cy.wait(5500);
    cy.get('[data-testid="first-share-step-connected"]').should('have.class', 'is-done');
    cy.get('[data-testid="first-share-step-authorized"]').should('have.class', 'is-done');
    cy.get('[data-testid="first-share-step-job"]').should('have.class', 'is-done');
    cy.get('[data-testid="first-share-step-submitted"]').should('have.class', 'is-done');
    cy.get('[data-testid="first-share-step-accepted"]').should('have.class', 'is-current');

    cy.then(() => {
      phase.current = 2;
    });
    cy.wait(5500);
    cy.get('[data-testid="first-share-watch-card"]').should('be.visible');
    cy.get('[data-testid="first-share-watch-card"]')
      .contains('First share accepted by pool.example.com:3333 - you are mining.')
      .should('be.visible');
    cy.contains('Achieved difficulty 12,482 / pool target 512').should('be.visible');
    cy.window().then(win => {
      expect(win.localStorage.getItem(WATCH_KEY)).to.eq('done');
    });

    cy.reload();
    cy.get('[data-testid="first-share-watch-card"]').should('not.exist');
  });

  it('does not show when accepted shares predate the watch baseline', () => {
    installWatchFixtures(() => 2);

    cy.visit('/', { onBeforeLoad: armWatch });
    cy.wait('@status');

    cy.window().should(win => {
      expect(win.localStorage.getItem(WATCH_KEY)).to.eq('done');
    });
    cy.get('[data-testid="first-share-watch-card"]').should('not.exist');
  });

  it('can be dismissed from Heater home and stays dismissed', () => {
    installWatchFixtures(() => 0);

    cy.visit('/', {
      onBeforeLoad(win) {
        armWatch(win);
        win.localStorage.setItem('dcentos-settings', JSON.stringify({
          mode: 'heater',
          setupComplete: true,
          minerName: 'Heater Watch',
        }));
        win.localStorage.setItem('dcentos-current-page', 'heater-home');
      },
    });
    cy.wait('@status');

    cy.get('[data-testid="first-share-watch-card"]').should('be.visible');
    cy.get('[data-testid="first-share-watch-card"]').contains('button', 'Dismiss').click();
    cy.get('[data-testid="first-share-watch-card"]').should('not.exist');
    cy.window().then(win => {
      expect(win.localStorage.getItem(WATCH_KEY)).to.eq('dismissed');
    });

    cy.reload();
    cy.get('[data-testid="first-share-watch-card"]').should('not.exist');
  });
});
