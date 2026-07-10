/// <reference types="cypress" />

import 'cypress-axe';

interface CypressWsHarness {
  emitJson: (body: unknown) => void;
  socketCount: () => number;
}

declare global {
  interface Window {
    __dcentCypressWs?: CypressWsHarness;
  }
}

const statusBody = {
  hashrate_ghs: 12_000,
  hashrate_5s_ghs: 12_000,
  accepted: 12,
  rejected: 0,
  uptime_s: 3600,
  firmware_version: "0.5.0",
  mode: "standard",
  chains: [
    {
      id: 6,
      chips: 63,
      frequency_mhz: 650,
      voltage_mv: 9100,
      temp_c: 55,
      hashrate_ghs: 4000,
      errors: 0,
      status: "ok",
    },
  ],
  fans: { pwm: 30, rpm: 3000, per_fan: [] },
  pool: {
    url: "stratum+tcp://pool.d-central.tech:3333",
    status: "connected",
    difficulty: 131072,
    last_share_at: 0,
    protocol: "sv1",
    encrypted: false,
    donating: false,
  },
};

const systemInfoBody = {
  firmware: "dcentos",
  version: "0.5.0",
  model: "Antminer S9",
  hostname: "s9-cypress",
  mac: "00:11:22:33:44:55",
  uptime_s: 3600,
  chip_type: "bm1387",
  chip_count: 189,
  chain_count: 3,
  mode: "standard",
  hashrate_ghs: 12_000,
  api_version: "2.0",
  board: "BHB42601",
  soc: "zynq",
  hardware: {
    miner_serial: "S9-CYPRESS",
    control_board: "s9",
    hb_type: "BHB42601",
    chip_type: "bm1387",
    psu_model: "APW3",
  },
};

const thermalPostureBody = {
  schema: "dcentos.thermal.posture.v1",
  status: "ok",
  read_only: true,
  control_actions: false,
  hardware_writes: false,
  filesystem_mutation: false,
  telemetry_source: "cypress",
  source: "fixture",
  mode: "standard",
  generated_at_s: 1_779_999_600,
  fetched_at_ms: Date.now(),
  thermal: {
    available: true,
    reason: "fixture",
    avg_temp_c: 55,
    max_temp_c: 56,
    hottest_chain_id: 6,
    valid_chain_count: 1,
    missing_chain_count: 0,
    chains: [
      {
        id: 6,
        temp_c: 56,
        status: "ok",
        source: "fixture",
      },
    ],
    thresholds: {
      target_c: 70,
      hot_c: 80,
      dangerous_c: 90,
      hysteresis_c: 5,
      source: "fixture",
      reason: "fixture",
    },
  },
  fans: {
    available: true,
    pwm: 30,
    rpm: 3000,
    per_fan: [],
    rpm_: true,
    tach_suspect: false,
    min_pwm: 20,
    max_pwm: 100,
    range_source: "fixture",
    reason: "fixture",
  },
  power: {
    available: true,
    board_watts: 1200,
    wall_watts: 1300,
    efficiency_jth: 108,
    btu_h: 4436,
    source: "fixture",
    calibrated: false,
    age_s: 1,
    watt_cap: null,
    runtime_limits_visible: true,
    dispatcher_limit_count: 0,
    runtime_limits: [],
    reason: "fixture",
  },
  curtailment: {
    available: true,
    state: "none",
    source: "fixture",
    read_only: true,
    reason: "fixture",
  },
  hardware_support: {
    fan_rpm_feedback: true,
    power_source: "fixture",
    power_calibrated: false,
    pmbus_measured: false,
    reason: "fixture",
  },
  runtime_ownership: {
    dispatcher_limits_visible: true,
    thermal_related_limit: false,
    power_cap_active: false,
    reason: "fixture",
  },
  safety: {
    mode: "standard",
    envelope: {
      dangerous_temp_c: 90,
      max_frequency_mhz: 650,
      allow_overclock: false,
      allow_raw_registers: false,
      min_fan_pwm: 20,
      max_power_watts: 1500,
    },
    thermal_blocker: false,
    reason: "fixture",
  },
  sources: ["cypress"],
  limitations: [],
};

const miningWorkPostureBody = {
  schema: "dcentos.mining.work.posture.v1",
  status: "connected",
  read_only: true,
  control_actions: false,
  hardware_writes: false,
  filesystem_mutation: false,
  telemetry_source: "cypress",
  source: "fixture",
  mode: "standard",
  generated_at_s: 1_779_999_600,
  fetched_at_ms: Date.now(),
  pool: {
    available: true,
    url: "stratum+tcp://pool.d-central.tech:3333",
    status: "connected",
    active: true,
    protocol: "sv1",
    encrypted: false,
    difficulty: 131072,
    last_accepted_share_s: 12,
    telemetry_source: "fixture",
    health_limitations: [],
    no_notify_age_s: null,
    failover_policy: "observability_only",
    auto_fallback_active: false,
  },
  protocol: {
    name: "sv1",
    encrypted: false,
    source: "fixture",
    reason: "fixture",
  },
  donation: {
    active: false,
    source: "fixture",
    reason: "fixture",
  },
  sv2: {
    available: false,
    encrypted: false,
    session: null,
    source: "fixture",
    reason: "fixture",
  },
  job_declaration: {
    available: false,
    enabled: false,
    configured: false,
    connected: false,
    runtime_state: "disabled",
    mining_job_token_available: false,
    template_prev_hash_ready: false,
    custom_job_candidate_ready: false,
    custom_job_injection_ready: false,
    custom_job_injection_active: false,
    custom_job_bridge: null,
    mode: "disabled",
    endpoint: "/api/jd/status",
    source: "fixture",
    reason: "fixture",
  },
  jobs: {
    available: false,
    current_job_available: false,
    latest_observed_job_id: null,
    latest_observed_job_age_s: null,
    latest_observed_job_source: "not_persisted",
    recent_job_ids: [],
    reason: "fixture",
  },
  work: {
    available: true,
    active_hashrate: true,
    hashrate_ghs: 12_000,
    hashrate_5s_ghs: 12_000,
    current_notify_age_s: null,
    work_ring_occupancy: null,
    dispatch_queue_depth: null,
    source: "fixture",
    reason: "fixture",
  },
  shares: {
    available: true,
    accepted_total: 12,
    rejected_total: 0,
    accept_rate_pct: 100,
    reject_rate_pct: 0,
    recent_count: 0,
    latest_event_age_s: null,
    recent_events: [],
    source: "fixture",
    reason: "fixture",
  },
  limitations: [],
};

const networkBlockBody = {
  status: "unavailable",
  read_only: true,
  internet_dependency: false,
  available: false,
  source: "unavailable",
  source_label: "Unavailable",
  fetched_at_ms: Date.now(),
  cache_ttl_ms: 30000,
  block_height: null,
  height: null,
  block_hash: null,
  hash: null,
  timestamp_ms: null,
  age_s: null,
  difficulty: null,
  previous_hash: null,
  tx_count: null,
  transaction_count: null,
  subsidy_btc: null,
  fees_btc: null,
  reward_btc: null,
  reward_source: null,
  mempool: {
    available: false,
    source: "unavailable",
    fee_rate_sat_vb: null,
    fastest_fee_sat_vb: null,
    half_hour_fee_sat_vb: null,
    hour_fee_sat_vb: null,
    reason: "fixture",
  },
  pool_job: {
    available: false,
    source: "not_persisted",
    job_id: null,
    last_share_timestamp_ms: null,
    difficulty: null,
    protocol_meta_present: false,
    reason: "fixture",
  },
  source_manifest: {
    local_node: {
      enabled: false,
      configured: false,
      available: false,
      live_rpc: false,
      endpoint_label: null,
      credential_mode: "none",
      request_timeout_ms: 1500,
      reason: "fixture",
    },
    public_fallback: {
      enabled: false,
      available: false,
      reason: "fixture",
    },
    cache: {
      enabled: false,
      ttl_ms: 30000,
      age_ms: null,
      reason: "fixture",
    },
  },
  reasons: ["fixture"],
  limitations: ["fixture"],
};

const miningPipelineManifestBody = {
  schema: "dcentos.mining.pipeline.manifest.v1",
  status: "publisher_unavailable",
  read_only: true,
  control_actions: false,
  hardware_writes: false,
  filesystem_mutation: false,
  content_collected: false,
  probe_performed: false,
  handlers_executed: false,
  telemetry_source: "cypress",
  source: "fixture",
  generated_at_s: 1_779_999_600,
  fetched_at_ms: Date.now(),
  publisher_live: false,
  snapshot_available: false,
  snapshot_schema: "dcentos.mining.pipeline.snapshot.v1",
  snapshot_contract: {
    status: "unavailable",
    publisher_last_update_ms: null,
    snapshot_age_ms: null,
  },
  publisher_gate: {
    app_state_field: "mining_pipeline_snapshot_rx",
    receiver_configured: false,
    receiver_default: "None",
    config_toml_path: "mining.pipeline_snapshot.enabled",
    config_default_enabled: false,
    enabled_configs_rejected: false,
    publisher_default_enabled: false,
    live_snapshot_endpoint: "/api/mining/pipeline/snapshot",
    promotion_requires: [],
  },
  freshness_contract: {
    default_stale_after_ms: 5000,
    snapshot_available_only_when: "publisher_fresh",
    does_not_populate: [],
  },
  freshness_classifier: {
    schema: "dcentos.mining.pipeline.freshness.classifier.v1",
    status: "unavailable",
    runtime_wired: false,
    outputs: [],
    fail_closed_when: [],
    example_fixtures: [],
  },
  publisher_design: {
    status: "default_off",
    implemented: true,
    live_route_mounted: true,
    bounded_publish_cadence: {
      max_hz: 1,
      min_interval_ms: 1000,
      publish_per_nonce: false,
    },
    promotion_requires: [],
    hardware_smoke_required: [],
  },
  publisher_promotion_checklist: {
    status: "blocked",
    promotion_state: "blocked",
    requirements: [],
    blockers: [],
    active_blocker_count: 0,
    active_blocker_ids: [],
    all_blockers_active: false,
  },
  fleet_parser_notes: {
    schema: "dcentos.mining.pipeline.fleet_parser_notes.v1",
    read_only: true,
    live_telemetry: false,
  },
  live_publisher: {
    available: false,
    enabled: false,
    snapshot_available: false,
    source: "fixture",
    reason: "fixture",
  },
  existing_surfaces: [],
  candidate_snapshot_fields: [],
  publisher_contract: {
    owner: "mining_pipeline",
    transport: "none",
    update_budget: "bounded",
    rest_consumer: "read_only",
    control_scope: "none",
    forbidden: [],
  },
  validation_plan: {
    automated: [],
    hardware_required: [],
  },
  related_endpoints: [],
  limitations: [],
};

Cypress.on("window:before:load", (win) => {
  const sockets: Array<{ emitJson: (body: unknown) => void }> = [];

  class MockWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    readonly url: string;
    readyState = MockWebSocket.CONNECTING;
    onopen: ((event: Event) => void) | null = null;
    onmessage: ((event: MessageEvent) => void) | null = null;
    onclose: ((event: CloseEvent) => void) | null = null;
    onerror: ((event: Event) => void) | null = null;

    constructor(url: string) {
      super();
      this.url = url;
      sockets.push(this);
      setTimeout(() => {
        this.readyState = MockWebSocket.OPEN;
        const event = new Event("open");
        this.onopen?.(event);
        this.dispatchEvent(event);
      }, 0);
    }

    send() {
      // no-op: Cypress e2e uses REST fixtures, not live websocket frames.
    }

    emitJson(body: unknown) {
      if (this.readyState !== MockWebSocket.OPEN) {
        throw new Error("Cypress mock WebSocket is not open");
      }
      const event = new MessageEvent("message", { data: JSON.stringify(body) });
      this.onmessage?.(event);
      this.dispatchEvent(event);
    }

    close() {
      this.readyState = MockWebSocket.CLOSED;
      const event = new CloseEvent("close");
      this.onclose?.(event);
      this.dispatchEvent(event);
    }
  }

  win.__dcentCypressWs = {
    emitJson(body: unknown) {
      const socket = sockets[sockets.length - 1];
      if (!socket) {
        throw new Error("No Cypress mock WebSocket has been created");
      }
      socket.emitJson(body);
    },
    socketCount() {
      return sockets.length;
    },
  };
  win.WebSocket = MockWebSocket as unknown as typeof WebSocket;
});

beforeEach(() => {
  cy.intercept("GET", "/api/**", {
    statusCode: 200,
    body: {},
  });

  cy.intercept("GET", "/api/dashboard/health", {
    statusCode: 200,
    body: {
      pid: 1234,
      alive: true,
      uptime_s: 3600,
      last_log_lines: ["dcentrald cypress fixture"],
      last_health_probe_ts: Date.now(),
    },
  });

  cy.intercept("GET", "/api/setup/status", {
    statusCode: 200,
    body: { needs_setup: false },
  });
  cy.intercept("GET", "/api/config", {
    statusCode: 200,
    body: { mode: { active: "standard" } },
  });
  cy.intercept("GET", "/api/status", { statusCode: 200, body: statusBody });
  cy.intercept("GET", "/api/stats", { statusCode: 200, body: statusBody });
  cy.intercept("GET", "/api/system/info", {
    statusCode: 200,
    body: systemInfoBody,
  });
  cy.intercept("GET", "/api/system/health", {
    statusCode: 200,
    body: { mode: "native", alive: true, blockers: [] },
  });
  cy.intercept("GET", "/api/system/upgrade/status", {
    statusCode: 200,
    body: { stage: "idle", active: false, entries: [] },
  });
  cy.intercept("GET", "/api/system/restore-to-stock/status", {
    statusCode: 200,
    body: {
      state: "idle",
      last_safety_findings: [],
      transitions: 0,
      last_backup_fw_setenv_present: true,
    },
  });
  cy.intercept("GET", "/api/system/restore-to-stock/preflight-checks", {
    statusCode: 404,
    body: { error: "preflight checks unavailable in Cypress default fixture" },
  });
  cy.intercept("GET", "/api/thermal/posture", {
    statusCode: 200,
    body: thermalPostureBody,
  });
  cy.intercept("GET", "/api/mining/work/posture", {
    statusCode: 200,
    body: miningWorkPostureBody,
  });
  cy.intercept("GET", "/api/network/block", {
    statusCode: 200,
    body: networkBlockBody,
  });
  cy.intercept("GET", "/api/mining/pipeline/manifest", {
    statusCode: 200,
    body: miningPipelineManifestBody,
  });
  cy.intercept("GET", "/api/pools", {
    statusCode: 200,
    body: { pools: [], active: null },
  });
  cy.intercept("GET", "/api/pools/failover", {
    statusCode: 200,
    body: { primary: null, backup: null, donation: null },
  });
  cy.intercept("GET", "/api/history", {
    statusCode: 200,
    body: { history: [] },
  });
  cy.intercept("GET", "/api/history/shares", {
    statusCode: 200,
    body: { events: [] },
  });
  cy.intercept("GET", "/api/autotuner/status", {
    statusCode: 200,
    body: { enabled: false, live_runtime: false, stale: true, age_s: 0 },
  });
  cy.intercept("GET", "/api/profiles/silicon", {
    statusCode: 200,
    body: [],
  });
});
