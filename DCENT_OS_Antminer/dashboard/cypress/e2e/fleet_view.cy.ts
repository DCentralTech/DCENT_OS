/// <reference types="cypress" />

type FleetRouteMode = "live" | "unavailable";

function stubShellRoutes(fleetRoute: FleetRouteMode = "live") {
  cy.intercept("GET", "/api/config", {
    statusCode: 200,
    body: { mode: { active: "standard" } },
  });
  cy.intercept("GET", "/api/setup/status", {
    statusCode: 200,
    body: { needs_setup: false },
  });
  cy.intercept("GET", "/api/status", {
    statusCode: 200,
    body: {
      hashrate_ghs: 202400,
      hashrate_5s_ghs: 202100,
      accepted: 42,
      rejected: 0,
      uptime_s: 3600,
      firmware_version: "0.5.0",
      mode: "standard",
      chains: [],
      fans: { pwm: 70, rpm: 4500 },
      pool: {
        url: "stratum+tcp://pool.d-central.tech:3333",
        status: "connected",
        difficulty: 131072,
        last_share_s: 3,
        donating: false,
      },
    },
  });
  cy.intercept("GET", "/api/system/info", {
    statusCode: 200,
    body: {
      firmware: "dcentos",
      version: "0.5.0",
      model: "Antminer S21",
      hostname: "s21",
      mac: "00:11:22:33:44:55",
      uptime_s: 3600,
      chip_type: "bm1368",
      chip_count: 324,
      chain_count: 3,
      mode: "standard",
      hashrate_ghs: 202400,
      api_version: "2.0",
      board: "BHB68xxx",
      soc: "amlogic",
      hardware: {
        miner_serial: "S21-LAB-B1",
        control_board: "s21-amlogic",
        hb_type: "BHB68xxx",
        chip_type: "bm1368",
        psu_model: "APW121215",
      },
    },
  });
  cy.intercept("GET", "/api/stats", {
    statusCode: 200,
    body: {
      hashrate_ghs: 202400,
      hashrate_ths: 202.4,
      uptime_s: 3600,
      fans: { fan1: 4500, fan2: 4500 },
      power: { watts: 3500, efficiency_jth: 17.5 },
      chains: [],
    },
  });
  cy.intercept("GET", "/api/system/health", {
    statusCode: 200,
    body: { mode: "native", alive: true, blockers: [] },
  });
  if (fleetRoute === "unavailable") {
    cy.intercept("GET", "/api/fleet/miners", {
      statusCode: 503,
      body: { error: "fleet unavailable" },
    });
  } else {
    cy.intercept("GET", "/api/fleet/miners", {
      statusCode: 200,
      body: {
        generated_at_ms: Date.UTC(2026, 4, 14, 16, 20, 0),
        miners: [
          {
            id: "s21",
            hostname: "s21",
            ip: "203.0.113.135",
            model: "Antminer S21",
            hashrate_ghs: 202400,
            temp_c: 58,
            fan_pwm: 70,
            status: "alive",
            last_seen_ms: Date.UTC(2026, 4, 14, 16, 19, 42),
          },
          {
            id: "s9",
            hostname: "s9",
            ip: "203.0.113.97",
            model: "Antminer S9",
            hashrate_ghs: 12350,
            temp_c: 54,
            fan_pwm: 38,
            status: "alive",
            last_seen_ms: Date.UTC(2026, 4, 14, 16, 19, 18),
          },
          {
            id: "s17-82",
            hostname: "s17-82",
            ip: "203.0.113.82",
            model: "Antminer S17",
            hashrate_ghs: 52100,
            temp_c: 59,
            fan_pwm: 64,
            status: "alive",
            last_seen_ms: Date.UTC(2026, 4, 14, 16, 18, 59),
          },
          {
            id: "s19jpro",
            hostname: "s19jpro",
            ip: "203.0.113.129",
            model: "Antminer S19 Pro",
            hashrate_ghs: 112550,
            temp_c: 62,
            fan_pwm: 82,
            status: "starting",
            last_seen_ms: Date.UTC(2026, 4, 14, 16, 18, 27),
          },
          {
            id: "s19jpro",
            hostname: "s19jpro",
            ip: "203.0.113.139",
            model: "Antminer S19j Pro",
            hashrate_ghs: 0,
            temp_c: 36,
            fan_pwm: 100,
            status: "dead",
            last_seen_ms: Date.UTC(2026, 4, 14, 15, 59, 40),
          },
        ],
      },
    });
  }
  cy.intercept("GET", "/api/fleet/pool-stats", {
    statusCode: 200,
    body: {
      schema: "dcentrald-stratum::pool_api::FleetPoolStats v1",
      status: "ok",
      source: "local_state",
      generated_at_s: 1778775600,
      stats: {
        miner_count: 5,
        connected_miners: 4,
        stale_miners: 0,
        donating_miners: 0,
        shares_submitted: 122,
        shares_accepted: 115,
        shares_rejected: 7,
        shares_unresolved: 0,
        pending_submit_dropped: 0,
        jobs_received: 12,
        failover_switches: 1,
        acceptance_rate: 115 / 122,
        pools: [
          {
            pool_url: "stratum+tcp://backup.pool:3333",
            miner_count: 1,
            connected_miners: 1,
            donating_miners: 0,
            shares_submitted: 10,
            shares_accepted: 5,
            shares_rejected: 5,
            shares_unresolved: 0,
            jobs_received: 2,
            average_difficulty: 65536,
            acceptance_rate: 0.5,
          },
          {
            pool_url: "stratum+tcp://pool.d-central.tech:3333",
            miner_count: 4,
            connected_miners: 3,
            donating_miners: 0,
            shares_submitted: 112,
            shares_accepted: 110,
            shares_rejected: 2,
            shares_unresolved: 0,
            jobs_received: 10,
            average_difficulty: 131072,
            acceptance_rate: 110 / 112,
          },
        ],
        miners: [
          {
            miner_id: "s21",
            host: "203.0.113.135",
            model: "Antminer S21",
            active_pool_url: "stratum+tcp://pool.d-central.tech:3333",
            connected: true,
            donating: false,
            shares_submitted: 101,
            shares_accepted: 100,
            shares_rejected: 1,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            jobs_received: 6,
            current_difficulty: 131072,
            failover_switch_count: 0,
            last_seen_s: 1778775582,
          },
          {
            miner_id: "s9",
            host: "203.0.113.97",
            model: "Antminer S9",
            active_pool_url: "stratum+tcp://pool.d-central.tech:3333",
            connected: true,
            donating: false,
            shares_submitted: 10,
            shares_accepted: 10,
            shares_rejected: 0,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            jobs_received: 3,
            current_difficulty: 4096,
            failover_switch_count: 0,
            last_seen_s: 1778775558,
          },
          {
            miner_id: "s17-82",
            host: "203.0.113.82",
            model: "Antminer S17",
            active_pool_url: "stratum+tcp://backup.pool:3333",
            connected: true,
            donating: false,
            shares_submitted: 10,
            shares_accepted: 5,
            shares_rejected: 5,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            jobs_received: 2,
            current_difficulty: 65536,
            failover_switch_count: 1,
            last_seen_s: 1778775539,
          },
          {
            miner_id: "s19jpro",
            host: "203.0.113.129",
            model: "Antminer S19 Pro",
            active_pool_url: "stratum+tcp://pool.d-central.tech:3333",
            connected: false,
            donating: false,
            shares_submitted: 2,
            shares_accepted: 0,
            shares_rejected: 2,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            jobs_received: 1,
            current_difficulty: 131072,
            failover_switch_count: 0,
            last_seen_s: 1778775507,
          },
          {
            miner_id: "s19jpro",
            host: "203.0.113.139",
            model: "Antminer S19j Pro",
            active_pool_url: "stratum+tcp://pool.d-central.tech:3333",
            connected: false,
            donating: false,
            shares_submitted: 0,
            shares_accepted: 0,
            shares_rejected: 0,
            shares_unresolved: 0,
            pending_submit_dropped: 0,
            jobs_received: 0,
            current_difficulty: 131072,
            failover_switch_count: 0,
            last_seen_s: 1778774380,
          },
        ],
      },
      limitations: ["Local state only"],
    },
  });
  cy.intercept("POST", "/api/system/identify", {
    statusCode: 200,
    body: {
      message: "The device says \"Hi!\" for 30 seconds.",
      active: true,
    },
  }).as("localIdentify");
  cy.intercept("GET", "/api/autotuner/status", {
    statusCode: 200,
    body: { enabled: false, live_runtime: false, stale: true, age_s: 0 },
  });
}

function visitFleet(fleetRoute: FleetRouteMode = "live") {
  stubShellRoutes(fleetRoute);
  cy.visit("/#/fleet", {
    onBeforeLoad(win) {
      win.localStorage.setItem(
        "dcentos-settings",
        JSON.stringify({
          setupComplete: true,
          mode: "standard",
        }),
      );
    },
  });
}

describe("FleetView - fleet backend", () => {
  it("renders the fleet inventory endpoint with required operator columns", () => {
    visitFleet();

    cy.get('[data-testid="fleet-view"]').should("exist");
    cy.get('[data-testid="fleet-lan-copy"]')
      .should("contain.text", "Local network snapshot")
      .and("contain.text", "DCENT_OS has no cloud");
    cy.get('[data-testid="fleet-summary"]')
      .should("contain.text", "5")
      .and("contain.text", "3 alive / 1 starting / 1 dead")
      .and("contain.text", "379.40 TH/s");
    cy.get('[data-testid="fleet-pool-stats-summary"]')
      .should("contain.text", "94.3%")
      .and("contain.text", "/api/fleet/pool-stats - local_state");

    cy.get('[data-testid="fleet-row-s21"]')
      .should("contain.text", "s21")
      .and("contain.text", "203.0.113.135")
      .and("contain.text", "Antminer S21")
      .and("contain.text", "202.40 TH/s")
      .and("contain.text", "58 C")
      .and("contain.text", "70%")
      .and("contain.text", "stratum+tcp://pool.d-central.tech:3333")
      .and("contain.text", "unavailable")
      .and("contain.text", "99.0%")
      .and("contain.text", "alive")
      .and("contain.text", "2026-05-14 16:19:42Z");
  });

  it("sorts by every fleet column through the header controls", () => {
    visitFleet();

    const expectedFirstRows = [
      ["hostname", "s21"],
      ["ip", "s17-82"],
      ["model", "s9"],
      ["hashrate_ghs", "s19jpro"],
      ["temp_c", "s19jpro"],
      ["fan_pwm", "s9"],
      ["pool_url", "s17-82"],
      ["acceptance_rate", "s19jpro"],
      ["status", "s21"],
      ["last_seen_ms", "s19jpro"],
    ];

    expectedFirstRows.forEach(([key, hostname]) => {
      cy.get(`[data-testid="fleet-sort-${key}"]`).click();
      cy.get('[data-testid^="fleet-row-"]')
        .first()
        .should("contain.text", hostname);
    });

    cy.get('[data-testid="fleet-sort-hostname"]').click();
    cy.get('[data-testid^="fleet-row-"]')
      .first()
      .should("contain.text", "s9");
  });

  it("does not render demo miners when the fleet endpoint is unavailable", () => {
    visitFleet("unavailable");

    cy.get('[data-testid="fleet-source-notice"]')
      .should("contain.text", "Fleet API unavailable")
      .and("contain.text", "Demo miners are hidden");

    cy.get('[data-testid="fleet-summary"]')
      .should("contain.text", "0 alive / 0 starting / 0 dead")
      .and("contain.text", "0 TH/s");

    cy.get('[data-testid="fleet-row-s21"]').should("not.exist");
    cy.get('[data-testid="fleet-empty-row"]').should(
      "contain.text",
      "No fleet rows are available from the local API.",
    );
  });

  it("identifies the local unit and shows a remote fallback link without claiming success", () => {
    visitFleet();
    cy.intercept("POST", "http://203.0.113.97/api/system/identify", {
      forceNetworkError: true,
    }).as("remoteIdentify");

    cy.get('[data-testid="fleet-identify-s21"]').click();
    cy.wait("@localIdentify");
    cy.get('[data-testid="fleet-identify-status-s21"]')
      .should("contain.text", "The device says");

    cy.get('[data-testid="fleet-identify-s9"]').click();
    cy.wait("@remoteIdentify");
    cy.get('[data-testid="fleet-identify-fallback-s9"]')
      .should("contain.text", "Open unit dashboard to identify")
      .and("have.attr", "href", "http://203.0.113.97/");
  });
});
