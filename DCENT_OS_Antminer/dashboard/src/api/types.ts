// DCENTos REST API TypeScript interfaces
// Hand-maintained REST DTOs. Generated shared capability types are re-exported
// from ./generated/capability.

// ─── Operating Mode ─────────────────────────────────────────
// Backend sends "home", dashboard uses "heater" internally (mapped in store).
// Both are valid values that can appear in API responses.
export {
  CAPABILITY_ERROR_KIND_VALUES,
  CAPABILITY_SCHEMA_VERSION,
  DEVICE_FAMILY_VALUES,
  IDENTITY_CONFIDENCE_VALUES,
  INSTALL_CAPABILITY_VALUES,
  PLANNER_OUTCOME_VALUES,
  PROOF_SCOPE_VALUES,
  RUNTIME_CAPABILITY_VALUES,
  SUPPORT_TIER_VALUES,
} from './generated/capability';
export type {
  AsicCapability,
  BoardCapability,
  CapabilityError,
  CapabilityErrorKind,
  DeviceCapabilityDescriptor,
  DeviceFamily,
  FailSafePolicy,
  HardwareIdentity,
  IdentityConfidence,
  InstallCapability,
  InstallCapabilityPlan,
  PlannerOutcome,
  PowerCapability,
  ProofScope,
  RuntimeCapability,
  SafeDefaults,
  SupportTier,
  ThermalCapability,
  TopologyCapability,
} from './generated/capability';

export type OperatingMode = 'heater' | 'standard' | 'hacker';

export interface DashboardVersionResponse {
  version: string;
  sha256: string | null;
  built_at: number | null;
  size_bytes: number;
  path: string | null;
}

// ─── Core State Types ───────────────────────────────────────
export interface ChainState {
  id: number;
  chips: number;
  frequency_mhz: number;
  voltage_mv: number;
  /**
   * Chain temperature in °C. On S9/Zynq the on-board TMP451/ADT7461/NCT218
   * sensors are read via BM1387 I²C passthrough and need 12V hashboard power;
   * when they return no data (a normal S9 condition) this carries the honest
   * XADC SoC die-temp fallback. `temp_source` says which. 0 means "no
   * temperature at all" (board genuinely unpowered / asleep).
   */
  temp_c: number;
  /**
   * Provenance of `temp_c`: `"board_sensor"` (real hashboard sensor),
   * `"soc_die_fallback"` (XADC SoC die-temp proxy — S9 board sensors silent),
   * or absent/null (legacy daemon, or genuinely no telemetry).
   */
  temp_source?: ChainTempSource | string | null;
  hashrate_ghs: number;
  errors: number;
  status: string;
}

export type ChainTempSource = 'board_sensor' | 'soc_die_fallback';

export interface PerFanReading {
  id: number;
  rpm: number;
  pwm_percent: number;
}

export interface FanState {
  pwm: number;
  rpm: number;
  mode?: string | null;
  per_fan?: PerFanReading[];
}

export interface HardwareInfo {
  capabilities?: {
    voltage_control: string;
    fan_rpm_feedback?: boolean;
    sleep_wake_supported?: boolean;
  };
  autotuner?: AutotunerPolicyStatus | null;
  miner_serial: string | null;
  control_board: string;
  hb_type: string | null;
  chip_type: string;
  psu_model: string | null;
  psu_fw_version: string | null;
  psu_serial: string | null;
  psu_voltage_range: string | null;
  psu_override_active: boolean;
}

export interface PsuOverrideModel {
  id: string;
  name: string;
  voltage_range: string;
}

export interface PsuOverrideResponse {
  active: boolean;
  model: string;
  voltage_v: number;
  voltage_range: string;
  available_models: PsuOverrideModel[];
}

export interface PsuOverrideRequest {
  enabled: boolean;
  model: string;
  voltage_v: number;
}

export interface PowerCalibrationResponse {
  status?: string;
  message?: string;
  enabled: boolean;
  multiplier: number;
  : number | null;
  estimated_wall_watts: number | null;
  estimated_unit_watts: number | null;
  updated_at_ms: number | null;
  current_reported_wall_watts: number;
  current_reported_unit_watts: number;
  power_source: string;
  power_source_detail?: string;
  live_power_available?: boolean;
  power_modeled?: boolean;
  power_note?: string;
  calibrated: boolean;
  calibration_multiplier?: number | null;
  projected_wall_watts?: number | null;
  projected_unit_watts?: number | null;
  projected_power_source_detail?: string;
  projected_power_live_available?: boolean;
  projected_power_modeled?: boolean;
  projected_power_note?: string;
}

export interface RuntimeWattCapState {
  cap_watts: number;
  headroom_watts: number;
  overage_watts: number;
  utilization_pct: number;
  throttling: boolean;
}

export interface PowerTargetingState {
  active: boolean;
  source?: string | null;
  mode?: string | null;
  preset?: string | null;
  schedule_label?: string | null;
  target_watts?: number | null;
  current_wall_watts: number;
  current_wall_watts_measured?: boolean | null;
  current_wall_watts_source_detail?: string | null;
  delta_watts?: number | null;
  comparison?: 'under' | 'near' | 'over' | null;
}

export interface PowerCalibrationRequest {
  enabled?: boolean;
  measured_wall_watts?: number;
}

export interface PoolState {
  url: string;
  status: string;
  difficulty: number;
  last_share_s: number;
  /** Last measured submit->response round-trip latency (ms). 0/undefined = not yet measured. */
  latency_ms?: number;
  donating: boolean;
  failover?: PoolFailoverStatus;
  share_efficiency?: {
    window_s: number;
    accepted_share_count: number;
    // Compatibility alias for accepted_pool_target_difficulty_sum; not achieved share difficulty.
    accepted_difficulty_sum: number;
    accepted_pool_target_difficulty_sum?: number;
    achieved_difficulty_sum?: number | null;
    estimated_wall_energy_kwh: number;
    accepted_shares_per_kwh?: number | null;
    // Compatibility alias for accepted_pool_target_difficulty_per_kwh.
    accepted_difficulty_per_kwh?: number | null;
    accepted_pool_target_difficulty_per_kwh?: number | null;
    achieved_difficulty_per_kwh?: number | null;
    difficulty_source?: string;
    power_source: string;
    calibrated: boolean;
  } | null;
  protocol?: 'sv1' | 'sv2';      // which protocol is active
  encrypted?: boolean;             // true for SV2 or SV1+TLS
  sv2_session?: Sv2SessionInfo;   // populated when connected via SV2
  auto_fallback_active?: boolean;
  auto_retry_sv2_after_s?: number | null;
  auto_fallback_reason?: string | null;

  // FWT-1: per-field provenance the daemon publishes so the UI can distinguish
  // a REAL measurement from an honest-default placeholder. "stratum_status"
  // (or "local_accounting") = real; "honest_default" = fresh-boot/never-observed
  // baseline (e.g. latency_ms 0, acceptance 100%) that must NOT be shown as if
  // measured. Surface these alongside the value (e.g. an "estimate" affordance).
  latency_ms_source?: string;
  encrypted_source?: string;
  donating_source?: string;
  failover_source?: string;
  auto_fallback_source?: string;
  /** Rolling 30-min pool acceptance %. 100.0 with source "honest_default" means
   *  no shares ACKed yet — NOT a real 100%. Render the provenance. */
  rolling_acceptance_pct_30min?: number;
  rolling_acceptance_count_30min?: [number, number];
  rolling_acceptance_source?: string;
  /** Reject counts bucketed by cause (low_diff, stale, dup, above_target,
   *  unauthorized, other). */
  reject_reason_counts?: number[];
  reject_reason_counts_source?: string;
}

export interface PoolFailoverDonationState {
  enabled: boolean;
  active: boolean;
  percent: number;
  cycle_duration_s: number;
  cycle_remaining_s?: number | null;
  pool_visible: boolean;
  pool_host: string;
  fallback_enabled?: boolean;
  fallback_pool_host?: string;
  fallback_worker_redacted?: string;
  fallback_policy?: string;
  disable_supported: boolean;
  excluded_from_user_failover: boolean;
  telemetry_source: string;
}

export interface PoolFailoverPoolState {
  index: number;
  priority: number;
  url: string;
  worker_redacted: string;
  configured: boolean;
  active: boolean;
  status: string;
  protocol: string;
  telemetry_source: string;
}

export interface PoolFailoverStatus {
  schema?: 'dcentos.pool_failover.v1' | string;
  read_only?: boolean;
  control_actions?: boolean;
  hardware_writes?: boolean;
  filesystem_mutation?: boolean;
  external_calls?: boolean;
  license_required?: boolean;
  license_server_required?: boolean;
  activation_required?: boolean;
  mandatory_fee?: boolean;
  fee_route?: string;
  local_first?: boolean;
  secrets_included?: boolean;
  redacted_fields?: string[];
  enabled?: boolean;
  configured_pool_count: number;
  active_pool_index: number;
  active_pool_priority: number;
  active_pool_url: string;
  active_pool_host?: string;
  active_worker_redacted?: string;
  active_route_kind?: 'user' | 'donation' | string;
  current_pool_role?: string;
  pools?: PoolFailoverPoolState[];
  consecutive_failures: number;
  switch_count: number;
  last_switch_reason?: string | null;
  last_failure_reason?: string | null;
  last_failure_pool_index?: number | null;
  last_failure_pool_priority?: number | null;
  stale_jobs_flushed_on_switch: boolean;
  pending_submit_correlations_cleared: number;
  pending_share_preserved: boolean;
  backoff_ms: number;
  return_to_primary_policy?: string;
  primary_stable_since_ms?: number | null;
  return_blocked_reason?: string | null;
  last_flush_at_ms?: number | null;
  flush_event_id?: string;
  pending_submit_correlations?: number | null;
  oldest_pending_submit_age_ms?: number | null;
  shares_unresolved?: number | null;
  pending_submit_dropped?: number | null;
  shares_dropped_while_disconnected?: number | null;
  /** @deprecated Use shares_unresolved. */
  unresolved_submit_count?: number | null;
  donation?: PoolFailoverDonationState;
  hashrate_split?: HashrateSplitState;
  source_basis?: string[];
  event: string;
  telemetry_source: string;
  last_update_ms?: number;
  stale_after_ms?: number;
  stale?: boolean;
  repair_diagnostic?: string;
  docs_link?: string;
  recovery_link?: string;
  limitations?: string[];
}

export interface HashrateSplitState {
  schema?: 'dcentos.hashrate_split.v1' | string;
  enabled: boolean;
  runtime_active?: boolean;
  routing_mode?: string;
  algorithm?: string;
  v1_only?: boolean;
  simultaneous_clients?: boolean;
  primary_pool_index?: number;
  secondary_pool_index?: number;
  active_route?: string;
  active_pool_index?: number;
  active_pool_priority?: number;
  primary_bps?: number;
  secondary_bps?: number;
  primary_pct?: number;
  secondary_pct?: number;
  cycle_duration_s?: number;
  cycle_remaining_s?: number;
  switch_count?: number;
  secondary_shares?: number;
  donation_composed?: boolean;
  donation_pct?: number | null;
  configured_effective_primary_pct?: number;
  configured_effective_secondary_pct?: number;
  requires_restart_or_reconnect?: boolean;
  dispatcher_flush_on_switch?: boolean;
  telemetry_source?: string;
}

export interface DonationConfig {
  enabled: boolean;
  percent: number;
  pool_url: string;
  worker: string;
  password: string;
  fallback_enabled: boolean;
  fallback_pool_url: string;
  fallback_worker: string;
  fallback_password: string;
  cycle_duration_s: number;
}

export interface DonationConfigResponse {
  status?: string;
  message?: string;
  config: DonationConfig;
  restart_required?: boolean;
}

/**
 * W9.5: Public donation pool disclosure shape returned by
 * `GET /api/donation/info`. Intentionally read-only and unauthenticated
 * so operators can independently verify on-chain that the donation
 * slice flows where the firmware claims (trust-but-verify).
 *
 * `payout_address` is the on-chain Bitcoin address that
 * `pool.d-central.tech` pays out to. `explorer_url` is a pre-built
 * mempool.space link to that address's payout history.
 */
export interface DonationInfoResponse {
  pool_url: string;
  pool_host: string;
  worker: string;
  payout_address: string;
  explorer_url: string;
  explorer_name: string;
  verify_label: string;
  trust_model: string;
  disclosure: string;
}

export interface RecentShareEvent {
  timestamp_ms: number;
  result: 'accepted' | 'rejected' | 'lucky' | string;
  job_id: string;
  // Achieved difficulty only when locally proven; null does not imply pool target.
  difficulty?: number | null;
  target_difficulty?: number | null;
  error_code?: number | null;
  error_msg?: string | null;
  worker_name?: string | null;
  nonce?: string | null;
  ntime?: string | null;
  extranonce2?: string | null;
  version_bits?: string | null;
  version?: number | null;
  protocol_meta_present?: boolean;
}

export interface ShareHistoryResponse {
  events: RecentShareEvent[];
}

export type MiningWorkPostureStatus =
  | 'active'
  | 'mining_capable'
  | 'connected'
  | 'connecting'
  | 'waiting'
  | 'unavailable'
  | string;

export interface MiningWorkPostureResponse {
  schema: 'dcentos.mining.work.posture.v1';
  status: MiningWorkPostureStatus;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  telemetry_source: string;
  source: string;
  mode: OperatingMode;
  generated_at_s: number;
  fetched_at_ms: number;
  pool: {
    available: boolean;
    url: string;
    status: string;
    active: boolean;
    connected: boolean;
    connecting: boolean;
    mining_capable: boolean;
    published_authorized?: boolean | null;
    published_authorize_state?: string | null;
    protocol: string;
    encrypted: boolean;
    // Pool target difficulty. This is not achieved local share difficulty.
    pool_target_difficulty: number;
    difficulty: number;
    last_accepted_share_s: number | null;
    telemetry_source: string;
    health_limitations: string[];
    no_notify_age_s: number | null;
    failover_policy: string;
    auto_fallback_active: boolean;
    auto_retry_sv2_after_s?: number | null;
    auto_fallback_reason?: string | null;
  };
  protocol: {
    name: string;
    encrypted: boolean;
    source: string;
    reason: string;
  };
  asic_version_rolling?: {
    bm1362_status: string;
    claim_default_enabled: boolean;
    source: string;
    operator_label: string;
    reason: string;
  };
  donation: {
    active: boolean;
    source: string;
    reason: string;
  };
  sv2: {
    available: boolean;
    encrypted: boolean;
    session?: Sv2SessionInfo | null;
    source: string;
    reason: string;
  };
  job_declaration: {
    available: boolean;
    enabled?: boolean;
    configured?: boolean;
    connected?: boolean;
    runtime_state?: string;
    mining_job_token_available?: boolean;
    template_prev_hash_ready?: boolean;
    custom_job_candidate_ready?: boolean;
    custom_job_injection_ready?: boolean;
    custom_job_injection_active?: boolean;
    custom_job_bridge?: Sv2CustomJobInfo | null;
    mode?: string;
    endpoint: string;
    template_provider_url?: string;
    job_declarator_url?: string;
    source: string;
    reason: string;
  };
  jobs: {
    available: boolean;
    current_job_available: boolean;
    latest_observed_job_id?: string | null;
    latest_observed_job_age_s?: number | null;
    latest_observed_job_source: string;
    recent_job_ids: string[];
    reason: string;
  };
  work: {
    available: boolean;
    active_hashrate: boolean;
    hashrate_ghs: number;
    hashrate_5s_ghs: number;
    current_notify_age_s: number | null;
    work_ring_occupancy: number | null;
    dispatch_queue_depth: number | null;
    source: string;
    reason: string;
  };
  shares: {
    available: boolean;
    accepted_total: number;
    rejected_total: number;
    total: number;
    accept_rate_pct?: number | null;
    reject_rate_pct?: number | null;
    recent_count: number;
    accepted_recent: number;
    rejected_recent: number;
    unknown_recent: number;
    latest_event_timestamp_ms?: number | null;
    latest_event_age_s?: number | null;
    latest_result?: string | null;
    latest_job_id?: string | null;
    source: string;
    recent_events: RecentShareEvent[];
    reason: string;
  };
  sources: string[];
  limitations: string[];
}

export type MiningPipelineManifestStatus = 'publisher_unavailable' | 'available' | 'degraded' | string;

export interface MiningPipelineManifestSurface {
  id: string;
  label: string;
  available: boolean;
  persistent: boolean;
  rest_queryable: boolean;
  source: string;
  fields: string[];
  limitations: string[];
}

export interface MiningPipelineManifestField {
  id: string;
  label: string;
  status: 'unavailable' | 'available' | string;
  source_hint: string;
  publisher_required: boolean;
  hardware_required: boolean;
  regression_risk: 'low' | 'medium' | 'high' | string;
  validation: string;
  reason: string;
}

export type MiningPipelineSnapshotStatus = 'unavailable' | 'live' | 'stale' | string;

export interface MiningPipelineSnapshot {
  schema: 'dcentos.mining.pipeline.snapshot.v1' | string;
  status: MiningPipelineSnapshotStatus;
  publisher_enabled: boolean;
  snapshot_available: boolean;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  generated_at_ms: number;
  publisher_last_update_ms?: number | null;
  snapshot_age_ms?: number | null;
  last_notify_timestamp_ms?: number | null;
  last_notify_age_ms?: number | null;
  current_job_id?: string | null;
  clean_jobs_total?: number | null;
  dispatch_bursts_total?: number | null;
  nonce_bursts_total?: number | null;
  stale_nonce_drops_total?: number | null;
  unsupported_version_drops_total?: number | null;
  local_validation_drops_total?: number | null;
  work_ring_occupancy?: number | null;
  dispatch_queue_depth?: number | null;
  source: string;
  limitations: string[];
}

export interface MiningPipelineSnapshotSchemaField {
  name: string;
  type: string;
  default: string | number | boolean | null;
  source: string;
}

export interface MiningPipelineSnapshotFreshnessContract {
  default_stale_after_ms: number;
  status_unavailable_when: string[];
  status_live_when: string[];
  status_stale_when: string[];
  snapshot_available_only_when: string;
  does_not_populate: string[];
}

export type MiningPipelineFreshnessClassifierStatus =
  | 'unavailable'
  | 'live'
  | 'stale'
  | 'future_clock_skew'
  | 'invalid'
  | string;

export interface MiningPipelineFreshnessClassifierFixture {
  id: MiningPipelineFreshnessClassifierStatus;
  label: string;
  design_only: true;
  non_telemetry: true;
  telemetry_source: 'none' | string;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  dispatcher_reads: false;
  hardware_reads: false;
  pool_socket_reads: false;
  runtime_wired: false;
  live_route_mounted: false;
  inputs: {
    domain_last_update_ms: number | null;
    generated_at_ms: number;
    stale_after_ms: number;
    max_future_skew_ms: number;
  };
  expected_classifier_status: MiningPipelineFreshnessClassifierStatus;
  expected_snapshot_status: MiningPipelineSnapshotStatus | string;
  snapshot_available: boolean;
  reason: string;
}

export interface MiningPipelineFreshnessClassifierContract {
  schema: 'dcentos.mining.pipeline.freshness.classifier.v1' | string;
  status: 'design_only' | string;
  implemented: boolean;
  runtime_wired: false;
  publisher_enabled: boolean;
  snapshot_available: boolean;
  live_route_mounted: boolean;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  telemetry_source: string;
  default_stale_after_ms: number;
  max_future_skew_ms: number;
  inputs: string[];
  outputs: MiningPipelineFreshnessClassifierStatus[];
  fail_closed_when: string[];
  snapshot_status_mapping: Record<string, MiningPipelineSnapshotStatus | string>;
  example_fixtures_schema: 'dcentos.mining.pipeline.freshness.classifier.fixture.v1' | string;
  example_fixture_count: number;
  example_fixtures_are_design_only: boolean;
  example_fixtures_live_telemetry: false;
  example_fixtures: MiningPipelineFreshnessClassifierFixture[];
  does_not_read: string[];
  does_not_populate: string[];
  promotion_note: string;
}

export interface MiningPipelineDomainFreshnessDesignBlock {
  status: 'unavailable' | string;
  last_update_ms: number | null;
  age_ms: number | null;
  stale_after_ms: number | null;
  source: string | null;
  null_reason: string;
  future_fields: string[];
  control_authority: false;
}

export interface MiningPipelineSnapshotDesignV2Contract {
  schema: 'dcentos.mining.pipeline.snapshot.design.v2' | string;
  status: 'implemented_default_off' | 'design_only' | string;
  implemented: boolean;
  publisher_enabled: boolean;
  snapshot_available: boolean;
  live_route_mounted: boolean;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  source: string;
  target_snapshot_schema: string;
  config_gate: string;
  enabled_configs_rejected: boolean;
  publisher_required: boolean;
  domain_freshness_status: 'unavailable' | string;
  blocks: {
    job_freshness: MiningPipelineDomainFreshnessDesignBlock;
    work_freshness: MiningPipelineDomainFreshnessDesignBlock;
    nonce_freshness: MiningPipelineDomainFreshnessDesignBlock;
    share_freshness: MiningPipelineDomainFreshnessDesignBlock;
  };
  forbidden: string[];
  hardware_smoke_required: Array<{
    model: string;
    required: boolean;
    status: 'not_run' | 'pass' | 'fail' | string;
  }>;
  promotion_requires: string[];
  limitations: string[];
}

export interface MiningPipelinePublisherDesignContract {
  schema: 'dcentos.mining.pipeline.publisher.design.v1' | string;
  status: 'implemented_default_off' | 'design_only' | string;
  implemented: boolean;
  publisher_enabled: boolean;
  live_route_mounted: boolean;
  config_gate: string;
  enabled_configs_rejected: boolean;
  owner: string;
  transport: string;
  rest_consumer: string;
  runtime_source?: string;
  bounded_publish_cadence: {
    required: boolean;
    max_hz: number;
    min_interval_ms: number;
    publish_per_nonce: boolean;
    reason: string;
  };
  promotion_blockers: string[];
  forbidden: string[];
  hardware_smoke_required: Array<{
    model: string;
    required: boolean;
    status: 'not_run' | 'pass' | 'fail' | string;
    checks: string[];
  }>;
  promotion_requires: string[];
}

export interface MiningPipelinePublisherPromotionChecklistRequirement {
  id: string;
  label: string;
  status: 'blocked' | 'not_run' | 'pass' | 'fail' | string;
  required: boolean;
  current_state: string;
  evidence_source: string;
  reason: string;
}

export type MiningPipelinePublisherPromotionBlockerId =
  | 'publisher_not_wired'
  | 'live_route_absent'
  | 'domain_freshness_unavailable'
  | 'hardware_smoke_s9_not_run'
  | 'hardware_smoke_s19pro_not_run'
  | 'hardware_smoke_s21_not_run'
  | 'rollback_not_tested'
  | string;

export interface MiningPipelinePublisherPromotionBlocker {
  id: MiningPipelinePublisherPromotionBlockerId;
  label: string;
  active: boolean;
  severity: 'promotion_blocking' | 'hardware_required' | 'cleared' | string;
  evidence_source: string;
  reason: string;
  clears_when: string;
}

export interface MiningPipelinePublisherPromotionChecklistContract {
  schema: 'dcentos.mining.pipeline.publisher.promotion.checklist.v1' | string;
  status: 'implemented_default_off' | 'design_only' | string;
  promotion_state: 'blocked' | 'ready' | string;
  implemented: boolean;
  source: string;
  read_only: true;
  route_required: boolean;
  dispatcher_reads: false;
  hardware_reads: false;
  pool_socket_reads: false;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  publisher_enabled: boolean;
  snapshot_available: boolean;
  live_route_mounted: boolean;
  target_snapshot_design_schema: string;
  target_snapshot_schema: string;
  config_gate: string;
  enabled_configs_rejected: boolean;
  required_publisher_owner: string;
  required_transport: string;
  required_rest_consumer: string;
  required_rollback_path: string;
  blockers_schema: 'dcentos.mining.pipeline.publisher.promotion.blocker.v1' | string;
  blocker_count: number;
  active_blocker_count: number;
  all_blockers_active: boolean;
  active_blocker_ids: MiningPipelinePublisherPromotionBlockerId[];
  requirements: MiningPipelinePublisherPromotionChecklistRequirement[];
  blockers: MiningPipelinePublisherPromotionBlocker[];
  forbidden: string[];
  promotion_allowed_only_when: string[];
}

export interface MiningPipelineFleetParserAliasContract {
  source_path: string;
  kind: string;
  source?: string;
  mirrors?: string;
  ordering?: string;
  missing_means?: string;
  parser_use?: string;
  readiness_evidence: false;
  live_telemetry?: false;
  telemetry_source: 'none' | string;
  export_default?: boolean;
  must_not_display_as_miner_state?: boolean;
  not_authoritative_for: string[];
}

export interface MiningPipelineFleetParserAuthoritativeSource {
  field: string;
  source_path: string;
  reason: string;
}

export interface MiningPipelineFleetParserNotesContract {
  schema: 'dcentos.mining.pipeline.fleet_parser_notes.v1' | string;
  status: 'schema_only' | string;
  read_only: true;
  live_telemetry: false;
  telemetry_source: 'none' | string;
  readiness_evidence: false;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  static_aliases: {
    active_blocker_ids: MiningPipelineFleetParserAliasContract;
    freshness_classifier_example_fixtures: MiningPipelineFleetParserAliasContract;
  };
  authoritative_sources: MiningPipelineFleetParserAuthoritativeSource[];
  live_promotion_requires: string[];
  does_not_read: string[];
  does_not_clear: MiningPipelinePublisherPromotionBlockerId[];
  operator_note: string;
}

export interface MiningPipelineSnapshotSchemaResponse {
  schema: 'dcentos.mining.pipeline.snapshot.schema.v1';
  snapshot_schema: 'dcentos.mining.pipeline.snapshot.v1';
  status: 'default_off' | string;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  publisher_default_enabled: false;
  live_snapshot_endpoint: string | null;
  config_gate: {
    toml_path: string;
    default_enabled: false;
    current_config_read: false;
    enabled_configs_rejected: false;
    live_snapshot_endpoint: string | null;
    reason: string;
  };
  generated_at_s: number;
  fetched_at_ms: number;
  default_snapshot: MiningPipelineSnapshot;
  freshness_contract: MiningPipelineSnapshotFreshnessContract;
  freshness_classifier_schema: 'dcentos.mining.pipeline.freshness.classifier.v1' | string;
  freshness_classifier: MiningPipelineFreshnessClassifierContract;
  publisher_design: MiningPipelinePublisherDesignContract;
  snapshot_design_schema: 'dcentos.mining.pipeline.snapshot.design.v2' | string;
  snapshot_design: MiningPipelineSnapshotDesignV2Contract;
  promotion_checklist_schema: 'dcentos.mining.pipeline.publisher.promotion.checklist.v1' | string;
  publisher_promotion_checklist: MiningPipelinePublisherPromotionChecklistContract;
  fleet_parser_notes_schema: 'dcentos.mining.pipeline.fleet_parser_notes.v1' | string;
  fleet_parser_notes: MiningPipelineFleetParserNotesContract;
  fields: MiningPipelineSnapshotSchemaField[];
  forbidden: string[];
  validation_required: string[];
  limitations: string[];
}

export interface MiningPipelineManifestResponse {
  schema: 'dcentos.mining.pipeline.manifest.v1';
  status: MiningPipelineManifestStatus;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  telemetry_source: string;
  source: string;
  generated_at_s: number;
  fetched_at_ms: number;
  publisher_live: boolean;
  snapshot_available: boolean;
  snapshot_schema: string;
  snapshot_contract: MiningPipelineSnapshot;
  publisher_gate: {
    app_state_field: string;
    receiver_configured: boolean;
    receiver_default: string;
    config_toml_path: string;
    config_default_enabled: false;
    enabled_configs_rejected: boolean;
    publisher_default_enabled: false;
    live_snapshot_endpoint: string | null;
    promotion_requires: string[];
  };
  freshness_contract: MiningPipelineSnapshotFreshnessContract;
  freshness_classifier_schema: 'dcentos.mining.pipeline.freshness.classifier.v1' | string;
  freshness_classifier: MiningPipelineFreshnessClassifierContract;
  publisher_design: MiningPipelinePublisherDesignContract;
  snapshot_design_schema: 'dcentos.mining.pipeline.snapshot.design.v2' | string;
  snapshot_design: MiningPipelineSnapshotDesignV2Contract;
  promotion_checklist_schema: 'dcentos.mining.pipeline.publisher.promotion.checklist.v1' | string;
  publisher_promotion_checklist: MiningPipelinePublisherPromotionChecklistContract;
  fleet_parser_notes_schema: 'dcentos.mining.pipeline.fleet_parser_notes.v1' | string;
  fleet_parser_notes: MiningPipelineFleetParserNotesContract;
  live_publisher: {
    available: boolean;
    enabled: boolean;
    snapshot_available: boolean;
    source: string;
    reason: string;
  };
  existing_surfaces: MiningPipelineManifestSurface[];
  candidate_snapshot_fields: MiningPipelineManifestField[];
  publisher_contract: {
    owner: string;
    transport: string;
    update_budget: string;
    rest_consumer: string;
    control_scope: string;
    forbidden: string[];
  };
  validation_plan: {
    automated: string[];
    hardware_required: string[];
  };
  related_endpoints: string[];
  limitations: string[];
}

export type NetworkBlockSource = 'local_node' | 'pool_job' | 'public_fallback' | 'unavailable' | 'none';

export interface NetworkBlockMempoolStatus {
  available: boolean;
  source: NetworkBlockSource | 'unavailable';
  fee_rate_sat_vb?: number | null;
  fastest_fee_sat_vb?: number | null;
  half_hour_fee_sat_vb?: number | null;
  hour_fee_sat_vb?: number | null;
  reason?: string | null;
}

export interface NetworkBlockPoolJobLink {
  available: boolean;
  source: 'recent_share_history' | 'not_persisted' | string;
  job_id?: string | null;
  last_share_timestamp_ms?: number | null;
  difficulty?: number | null;
  protocol_meta_present?: boolean;
  reason?: string | null;
}

export interface NetworkBlockSourceManifest {
  local_node: {
    enabled: boolean;
    configured: boolean;
    available: boolean;
    live_rpc: boolean;
    endpoint_label?: string | null;
    credential_mode?: 'none' | 'user_password' | 'cookie_file' | string;
    request_timeout_ms?: number | null;
    reason?: string | null;
  };
  public_fallback: {
    enabled: boolean;
    available: boolean;
    reason?: string | null;
  };
  cache: {
    enabled: boolean;
    ttl_ms?: number | null;
    age_ms?: number | null;
    reason?: string | null;
  };
}

export interface NetworkBlockResponse {
  status: 'ok' | 'unavailable' | string;
  read_only: boolean;
  internet_dependency: false;
  available: boolean;
  source: NetworkBlockSource;
  source_label: string;
  fetched_at_ms: number;
  cache_ttl_ms: number;
  block_height?: number | null;
  height?: number | null;
  block_hash?: string | null;
  hash?: string | null;
  timestamp_ms?: number | null;
  age_s?: number | null;
  difficulty?: number | null;
  previous_hash?: string | null;
  tx_count?: number | null;
  transaction_count?: number | null;
  subsidy_btc?: number | null;
  fees_btc?: number | null;
  reward_btc?: number | null;
  reward_source?: 'local_node' | 'job_template' | 'subsidy_only' | string | null;
  mempool: NetworkBlockMempoolStatus;
  pool_job: NetworkBlockPoolJobLink;
  source_manifest?: NetworkBlockSourceManifest;
  reasons: string[];
  limitations: string[];
}

// ─── GET /api/status ────────────────────────────────────────
export type ThermalPostureStatus =
  | 'ok'
  | 'watch'
  | 'hot'
  | 'critical'
  | 'limited'
  | 'sensor_limited'
  | 'unknown';

export interface ThermalPostureChainReading {
  id: number;
  temp_c: number | null;
  status: string;
  source: string;
}

export interface ThermalPostureThresholds {
  target_c: number;
  hot_c: number;
  dangerous_c: number;
  hysteresis_c: number;
  source: string;
  reason: string;
}

export interface ThermalPowerPostureResponse {
  schema: 'dcentos.thermal.posture.v1';
  status: ThermalPostureStatus;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  telemetry_source: string;
  source: string;
  mode: OperatingMode;
  generated_at_s: number;
  fetched_at_ms: number;
  thermal: {
    available: boolean;
    reason: string;
    avg_temp_c: number | null;
    max_temp_c: number | null;
    hottest_chain_id: number | null;
    valid_chain_count: number;
    missing_chain_count: number;
    chains: ThermalPostureChainReading[];
    thresholds: ThermalPostureThresholds;
  };
  fans: {
    available: boolean;
    pwm: number;
    rpm: number | null;
    per_fan: PerFanReading[];
    rpm_: boolean;
    tach_suspect: boolean;
    min_pwm: number;
    max_pwm: number;
    range_source: string;
    reason: string;
  };
  power: {
    available: boolean;
    board_watts: number | null;
    wall_watts: number | null;
    efficiency_jth: number | null;
    btu_h: number | null;
    source: string;
    source_detail: string;
    live_power_available: boolean;
    modeled: boolean;
    calibrated: boolean;
    calibration_multiplier?: number | null;
    age_s?: number | null;
    watt_cap?: RuntimeWattCapState | null;
    runtime_limits_visible: boolean;
    dispatcher_limit_count: number;
    runtime_limits: Array<{
      chain_id: number;
      effective_ceiling_mhz?: number | null;
      dominant_source?: string | null;
      active_sources: string[];
    }>;
    reason: string;
    note: string;
  };
  curtailment: {
    available: boolean;
    state: string;
    source: string;
    read_only: true;
    reason: string;
  };
  hardware_support: {
    fan_rpm_feedback: boolean;
    power_source: string;
    power_calibrated: boolean;
    pmbus_measured: boolean;
    reason: string;
  };
  runtime_ownership: {
    dispatcher_limits_visible: boolean;
    thermal_related_limit: boolean;
    power_cap_active: boolean;
    reason: string;
  };
  safety: {
    mode: OperatingMode;
    envelope: {
      dangerous_temp_c: number;
      max_frequency_mhz: number;
      allow_overclock: boolean;
      allow_raw_registers: boolean;
      min_fan_pwm: number;
      max_power_watts: number;
    };
    thermal_blocker: boolean;
    reason: string;
  };
  sources: string[];
  limitations: string[];
}

export interface StatusResponse {
  hashrate_ghs: number;
  hashrate_5s_ghs: number;
  accepted: number;
  rejected: number;
  uptime_s: number;
  firmware_version: string;
  mode: OperatingMode;
  chains: ChainState[];
  fans: FanState;
  pool: PoolState;
  share_efficiency?: PoolState['share_efficiency'];
  power?: {
    watts: number;
    wall_watts: number;
    efficiency_jth: number;
    btu_h: number;
    source?:
      | 'estimated'
      | 'live'
      | 'pmbus'
      | 'adc'
      | 'wall_calibrated_estimate'
      | 'calibrated_estimate'
      | 'live_power_watch'
      | 'static_model_fallback'
      | string;
    source_detail?:
      | 'pmbus_measured'
      | 'adc_measured'
      | 'wall_calibrated_estimate'
      | 'live_runtime_model'
      | 'static_power_fallback_from_miner_state'
      | string;
    live_power_available?: boolean;
    modeled?: boolean;
    note?: string;
    calibrated?: boolean;
    calibration_multiplier?: number | null;
    watt_cap?: RuntimeWattCapState;
    targeting?: PowerTargetingState;
    runtime_limits?: Array<{
      chain_id: number;
      effective_ceiling_mhz?: number | null;
      dominant_source?: string | null;
      active_sources: string[];
    }>;
  };
}

// ─── GET /api/config ────────────────────────────────────────
export interface ConfigResponse {
  mode: { active: OperatingMode };
  firmware_version: string;
  donation?: DonationConfig;
  api: {
    cgminer_port: number;
    http_port: number;
    websocket_enabled: boolean;
    auth_enabled: boolean;
  };
}

export interface ConfigBackupSourceEntry {
  id: string;
  label: string;
  path: string;
  active: boolean;
  writable_target: boolean;
  metadata_status: string;
  exists: boolean;
  size_bytes: number | null;
  modified_ms: number | null;
}

export interface ConfigBackupRedactionPolicy {
  content_included: boolean;
  secret_key_patterns: string[];
  notes: string[];
}

export interface ConfigBackupManifestResponse {
  status: string;
  read_only: boolean;
  content_collected: boolean;
  restore_supported: boolean;
  daemon_config_export_supported: boolean;
  dashboard_preferences_export_supported: boolean;
  sources: ConfigBackupSourceEntry[];
  redaction_policy: ConfigBackupRedactionPolicy;
  limitations: string[];
}

// ─── GET /api/config/export · POST /api/config/import ────────
// COMP-1 daemon config backup/restore (LuxOS/Braiins parity). The export is
// the full effective daemon config as a TOML document with every secret
// (passwords/tokens/keys), pool/donation worker wallet address, and
// credential-bearing pool URL replaced by `secret_placeholder`. Re-importing
// the exported document treats placeholder values as keep-existing, so a
// round-trip never overwrites a stored secret with the mask.
export interface ConfigExportResponse {
  status: string;
  redacted: boolean;
  reimportable: boolean;
  /** The token that replaced every secret/wallet/credential-URL in `config_toml`. */
  secret_placeholder: string;
  /** Key-name patterns the daemon treats as secret-bearing (for the redaction note). */
  secret_key_patterns: string[];
  schema_version?: number | null;
  exported_at_ms: number;
  /** The re-importable TOML document. POST it back verbatim to /api/config/import. */
  config_toml: string;
  notes: string[];
}

export interface ConfigImportResponse {
  status: string;
  restart_required: boolean;
  message: string;
  /** Top-level config sections that were validated and persisted. */
  sections: string[];
}

// ─── POST /api/config ───────────────────────────────────────
export interface ConfigUpdateRequest {
  mode?: { active: OperatingMode };
  donation?: DonationConfig;
  [key: string]: unknown;
}

export interface ConfigUpdateResponse {
  status: string;
  message: string;
}

// Per-channel webhook delivery format. The firmware `[webhook].format` field
// selects how the alert payload is shaped/delivered:
//   - generic  → POST the raw JSON envelope to any URL (ntfy.sh, PagerDuty, …)
//   - discord  → POST to a Discord channel webhook URL
//   - slack    → POST to a Slack incoming-webhook URL
//   - telegram → send via the Telegram Bot API (bot token + chat id, no URL)
export type WebhookFormat = 'generic' | 'discord' | 'slack' | 'telegram';

export interface WebhookConfig {
  enabled: boolean;
  url: string;
  events: string[];
  supported_events: string[];
  restart_required: boolean;
  // Added with the per-channel wiring. Optional so older daemons that don't
  // return them degrade gracefully (treated as 'generic' + empty Telegram).
  format?: WebhookFormat;
  // SECRET — the daemon masks this to "<redacted>" in the GET response (it
  // matches the existing "token" secret-key pattern) and treats the mask as
  // keep-existing on POST, exactly like webhook.url. The chat id is NOT a
  // secret and is returned in cleartext.
  telegram_bot_token?: string;
  telegram_chat_id?: string;
}

export interface WebhookConfigUpdateRequest {
  enabled: boolean;
  url: string;
  events: string[];
  format?: WebhookFormat;
  telegram_bot_token?: string;
  telegram_chat_id?: string;
}

export interface WebhookConfigUpdateResponse {
  status: string;
  message: string;
  config: WebhookConfig;
}

export interface WebhookTestResponse {
  status: string;
  message: string;
  http_status?: number;
}

export interface SetupStatusResponse {
  needs_setup: boolean;
  device_ready?: boolean;
  mining_ready?: boolean;
  power_source?: string;
  resume_requires_auth?: boolean;
  // Freedom-first: the operator explicitly declined to set an owner password
  // and accepted the default. Drives the self-clearing security warning.
  password_opt_out?: boolean;
  // True when the operator has made ANY password decision (set one OR opted
  // out) — used to allow setup completion without forcing a password.
  password_decision_made?: boolean;
  // Freedom-first (exact parallel): the operator explicitly declined the
  // circuit/breaker/safety acknowledgement. Drives the self-clearing
  // "circuit check not done" advisory.
  safety_opt_out?: boolean;
  // True when the operator has made ANY safety decision (acknowledged OR
  // opted out) — used to allow setup completion without forcing the
  // circuit check.
  safety_decision_made?: boolean;
  steps: string[];
  phase?: string;
  progress?: {
    safety: boolean;
    circuit: boolean;
    solar_provider?: boolean;
    password: boolean;
    mode: boolean;
    pool: boolean;
    complete: boolean;
  };
  auth?: {
    password_set: boolean;
    token_issued: boolean;
    password_opt_out?: boolean;
  };
  trust?: {
    install_origin: string;
    bootstrap_transport: string;
    hardening_profile: string;
    credentials_rotated: boolean;
    ssh_keys_enrolled: boolean;
    password_auth_disabled: boolean;
  };
  current?: {
    hostname: string;
    mode: 'home' | 'standard' | 'hacker' | '';
    power_source: string;
    circuit_voltage_v?: number | null;
    circuit_amperage_a?: number | null;
    pool: {
      url: string;
      worker: string;
    };
    // P2-4 (§4.E): daemon-persisted economics so the wizard resumes with the
    // real rate/currency and knows whether the operator has confirmed one.
    electricity_rate?: number;
    currency?: string;
    electricity_rate_calibrated?: boolean;
  };
  commissioning?: {
    solar_provider_required: boolean;
    solar_provider_saved: boolean;
    solar_provider_runtime_adopted: boolean;
    solar_provider?: string | null;
    solar_provider_trust?: 'manual' | 'telemetry' | null;
  } | null;
  completed_at?: string | null;
}

export interface SetupCircuitRequest {
  source?: string | null;
  voltage?: number | null;
  amperage?: number | null;
}

export type OffGridAdcConfig =
  | {
    type: 'ina226';
    i2c_bus: number;
    i2c_addr: number;
    shunt_mohm: number;
    voltage_divider: number;
  }
  | {
    type: 'sysfs';
    voltage_path: string;
    vref: number;
    bits: number;
    voltage_divider: number;
  }
  | {
    type: 'simulated';
    voltage_v: number;
    current_a: number;
  };

export interface OffGridConfigPayload {
  source_profile: 'direct_dc' | 'solar_battery' | '';
  enabled: boolean;
  battery_preset: string;
  adc: OffGridAdcConfig | null;
  freq_step_mhz: number;
  min_frequency_mhz: number;
  loop_interval_ms: number;
  custom_critical_v: number | null;
  custom_low_v: number | null;
  custom_high_v: number | null;
  custom_full_v: number | null;
  custom_recovery_v: number | null;
}

export interface OffGridConfigResponse extends OffGridConfigPayload {
  ready: boolean;
  restart_required: boolean;
  readiness_message: string;
}

export interface OffGridConfigSaveResponse {
  status: 'ok' | 'error';
  message: string;
  config?: OffGridConfigResponse;
}

export interface OffGridStatusResponse {
  enabled: boolean;
  zone: string;
  state: string;
  bus_voltage_v: number;
  current_a: number;
  power_w: number;
  battery_soc_pct: number;
  target_freq_mhz: number;
  freq_pct: number;
  voltage_rate_vps: number;
  uptime_battery_s: number;
  energy_consumed_wh: number;
  critical_v?: number;
  low_v?: number;
  high_v?: number;
  full_v?: number;
  sensor_source?: string;
  has_current?: boolean;
  sensor_ok?: boolean;
  message?: string;
}

export interface OffGridProbeResponse {
  ok: boolean;
  backend: 'ina226' | 'sysfs' | 'simulated' | 'unconfigured';
  sensorSource: string;
  hasCurrent: boolean;
  plausible: boolean;
  voltageV?: number | null;
  currentA?: number | null;
  powerW?: number | null;
  message: string;
}

export interface OffGridPreset {
  id: string;
  label: string;
  critical_v: number;
  low_v: number;
  normal_v: number;
  high_v: number;
  full_v: number;
  recovery_v: number;
}

export interface OffGridPresetsResponse {
  presets: OffGridPreset[];
}

// ─── GET /api/system/info ───────────────────────────────────
export interface SystemInfoResponse {
  firmware: string;
  version: string;
  model: string;
  hostname: string;
  mac: string;
  uptime_s: number;
  chip_type: string;
  chip_count: number;
  chain_count: number;
  mode: OperatingMode;
  hashrate_ghs: number;
  api_version: string;
  board: string;
  soc: string;
  /**
   * Canonical platform tier id from the daemon: "am1-zynq" / "am2-zynq" /
   * "am3-aml" / "am3-bb" / "unknown" (fail-closed). Feed to tierFromPlatformKey
   * + platformCapabilities for deterministic per-platform UI gating instead of
   * coercing the free-form model/board strings. (APIC-2)
   */
  platform_key?: string;
  hardware?: HardwareInfo;
  // AxeOS/pyasic-compatibility fields the daemon emits as 0 only for ecosystem
  // compatibility (e.g. bestDiff, vrTemp) are listed here — they are NOT real
  // telemetry and must be rendered as "n/a", never as a measured 0. (Omega P3-8.)
  unsupported_metrics?: string[];
  field_sources?: Record<string, string>;
}

// GET /api/system/stats
export interface SystemStatsResponse {
  uptime_s: number;
  load_avg_1m: number;
  load_avg_5m: number;
  load_avg_15m: number;
  load_percent_1m?: number | null;
  cpu_count: number;
  mem_total_kb: number;
  mem_available_kb: number;
  mem_used_kb: number;
  mem_used_percent?: number | null;
  soc_temp_c?: number | null;
  soc_temp_source?: string | null;
}

// ─── GET /api/system/health ─────────────────────────────────
export type SystemHealthMode = 'native' | 'proxy' | 'hybrid' | (string & {});

export type BosminerBlocker =
  | 'missing_license'
  | 'dead_pools'
  | 'fw_86_rejection'
  | 'license_cycle'
  | (string & {});

export interface BosminerHealth {
  alive: boolean;
  pid: number | null;
  pid_history?: number[];
  last_seen_ms: number | null;
  blockers: BosminerBlocker[];
  last_summary?: {
    accepted?: number;
    rejected?: number;
    mhs_5s?: number;
  } | null;
}

export type ChainRailVerdict = 'PENDING' | 'ALIVE' | 'DEAD' | 'PARTIAL' | (string & {});

export interface ChainRailTestStep {
  id: string;
  label: string;
  status: 'pass' | 'fail' | 'pending' | (string & {});
}

export interface ChainRailHealth {
  verdict: ChainRailVerdict;
  last_multimeter_reading_v: number | null;
  last_reading_at_ms: number | null;
  uart_rx_bytes_post_enable: number;
  test_steps: ChainRailTestStep[];
  steps_url: string;
}

export type RecoveryAction =
  | { kind: 'bench_multimeter'; rationale: string; doc_url: string }
  | { kind: 'pickit4_icsp'; rationale: string; doc_url: string; sku?: string }
  | { kind: 'hashboard_swap'; rationale: string; doc_url: string; sku?: string }
  | { kind: 'wait_license_cycle'; rationale: string; eta_s?: number }
  | { kind: 'restart_bosminer'; rationale: string }
  | { kind: string; rationale?: string; doc_url?: string; eta_s?: number; sku?: string };

export interface SystemHealthResponse {
  mode: SystemHealthMode;
  daemon?: {
    version?: string;
    uptime_s?: number;
    pid?: number | null;
    /**
     *  HIGH-1 (2026-05-24): true when the daemon is currently
     * dispatching work to ASICs (work_tx > 0, regardless of bosminer state).
     * Distinct from `bosminer.alive` — both can be true simultaneously on
     * the `a lab unit`-class XIL units running the  bosminer-handoff recipe.
     */
    is_mining?: boolean;
  };
  bosminer?: BosminerHealth | null;
  rail?: ChainRailHealth | null;
  recovery?: {
    next_action?: RecoveryAction | null;
  } | null;
  scrape?: {
    cgminer_url?: string;
    cgminer_reachable?: boolean;
    last_poll_ms?: number | null;
    consecutive_failures?: number;
  } | null;
  watchdog?: KernelWatchdogState | null;
  /**
   *  HIGH-1 hardware fingerprint mirror — populated from
   * `/etc/dcentos/platform` and `/etc/dcentos/board_target` by the
   * `/api/system/health` handler. Used to detect `a lab unit`-class XIL
   * S19j Pro units for the  handoff-mining state.
   */
  fingerprint?: {
    platform?: string | null;
    board_target?: string | null;
    psu_hardware_variant?: string | null;
    is_xil_25_class?: boolean;
  };
}

// ───  HIGH-2 — GET /api/mining/chain/presence ───────────────
// (2026-05-24) Per-chain "chips_responding / chips_expected" + chip-rail
// mV vs target. Replaces the lying `mining_enabled = hashrate_ghs > 0`
// derived state on partial-chain  runs. Daemon source: snapshot
// of the existing chain status the work dispatcher already maintains.
export interface ChainPresenceEntry {
  /** Chain index (0 = ttyS1 on .25, 1 = ttyS3, etc.). */
  idx: number;
  /** ASIC chips currently answering chip-UART enumeration. */
  chips_responding: number;
  /** ASIC chips physically expected on this chain (e.g. 63 on .25). */
  chips_expected: number;
  /** Live mV reading on the chip rail, or null if dsPIC is not reporting. */
  mv_actual: number | null;
  /** Commanded target mV (13700 for .25 ). */
  mv_target: number | null;
}

export interface ChainPresenceResponse {
  chains: ChainPresenceEntry[];
}

// ───  HIGH-3 — GET /api/env/recipe ──────────────────────────
// (2026-05-24) Read-only view of the  recipe state for the live
// `a lab unit` daemon process. Operators can see if recipe is applied without
// SSH'ing in to grep the env. Gate-1 Q3: dev-firmware no-auth posture.
export interface EnvRecipeResponse {
  /** Required env vars that ARE set (var name → string value). */
  applied: Record<string, string>;
  /** Required env vars that are MISSING (not set in the live env). */
  missing: string[];
  /**
   * Forbidden env vars ( falsified-list) that ARE set in the
   * live env. Non-empty = daemon will refuse to start on .25 hardware.
   */
  forbidden_detected: string[];
  fingerprint: {
    platform: string | null;
    board_target: string | null;
    psu_hardware_variant: string | null;
  };
  /** True when the fingerprint matches .25-class XIL Loki-spoof hardware. */
  is_xil_25_class: boolean;
  /**
   * True iff every required env is applied AND zero forbidden envs are
   * detected. False otherwise — banner goes red.
   */
  wave54_recipe_intact: boolean;
}

// ───  HIGH-1 — GET /api/mining/handoff/state ────────────────
// (2026-05-24) Canonical mode classifier the dashboard can read without
// re-deriving it from `bosminer.alive` + `daemon.is_mining` heuristics.
export type HandoffMiningMode =
  | 'handoff_mining'
  | 'standalone'
  | 'bosminer_only'
  | 'idle'
  | (string & {});

export interface HandoffStateResponse {
  mode: HandoffMiningMode;
  /** ms since DCENT_OS took over the chain driver, or null. */
  last_handoff_ms: number | null;
  /** True when bosminer was the cold-boot bring-up before the handoff. */
  bosminer_was_engaged: boolean;
  /** True when the recipe state suggests the operator should AC-cycle. */
  ac_cycle_recommended: boolean;
}

export interface KernelWatchdogState {
  available: boolean;
  source: string;
  state: string;
  reason?: string;
  identity?: string | null;
  status?: string | null;
  state_text?: string | null;
  bootstatus?: number | null;
  timeout_s?: number | null;
  timeleft_s?: number | null;
  nowayout?: boolean | null;
  read_only?: boolean;
}

export interface SystemUpgradeStatusResponse {
  status: string;
  read_only: boolean;
  state: 'idle' | 'validated_or_staged' | 'pending_boot_commit' | (string & {});
  stage_root: string;
  stage_root_present: boolean;
  staged_package_count: number;
  staged_packages: Array<{
    path: string;
    filename: string;
    size_bytes: number;
    modified_ms: number | null;
    source: string;
  }>;
  upgrade_stage: string | null;
  bootcount: string | null;
  bootlimit: string | null;
  boot_slot: string | null;
  sources?: Record<string, string>;
  limitations?: string[];
}

// ─── GET /api/system/asic ───────────────────────────────────
export interface AsicInfo {
  chain_id: number;
  chips: number;
  frequency: number;
  voltage: number;
  temp: number;
  hashrate: number;
  status: string;
  errors: number;
}

export type ApiCompatibilitySupport =
  | 'implemented'
  | 'implemented_alias'
  | 'recognized_unsupported'
  | 'documented_only'
  | (string & {});

export interface ApiCompatibilityRouteEntry {
  method: 'GET' | 'POST' | 'DELETE' | (string & {});
  path: string;
  support: ApiCompatibilitySupport;
  mutates: boolean;
  compatibility: string[];
  provenance: string;
  unsupported_fields: string[];
  limitations: string[];
}

export interface ApiCompatibilityCommandEntry {
  name: string;
  support: ApiCompatibilitySupport;
  mutates: boolean;
  provenance: string;
  limitations: string[];
}

export interface ApiCompatibilitySurface {
  id: string;
  label: string;
  protocol: string;
  default_port: number | null;
  default_bind: string | null;
  compatibility: string[];
  routes: ApiCompatibilityRouteEntry[];
  commands: ApiCompatibilityCommandEntry[];
  limitations: string[];
}

export interface ApiCompatibilityOmission {
  path?: string | null;
  surface?: string | null;
  reason: string;
}

export interface ApiCompatibilityManifestResponse {
  status: string;
  schema_version: number;
  read_only: boolean;
  content_collected: boolean;
  probe_performed: boolean;
  handlers_executed: boolean;
  surfaces: ApiCompatibilitySurface[];
  omissions: ApiCompatibilityOmission[];
  limitations: string[];
}

export type CompetitiveReadinessStatus =
  | 'proven'
  | 'partial'
  | 'blocked'
  | 'saved_only'
  | 'requires_restart'
  | 'unsafe'
  | 'not_implemented'
  | string;

export interface CompetitiveExternalDependency {
  id: string;
  purpose: string;
  default_state: string;
  required: string;
  disable_impact: string;
}

export interface CompetitiveDonationGate {
  default_enabled: boolean;
  current_enabled: boolean | null;
  default_percent: number;
  current_percent: number | null;
  cycle_duration_s_default: number;
  current_cycle_duration_s: number | null;
  pool_visible: boolean;
  disable_supported: boolean;
  donation_off_test_status: string;
  current_state_source: string;
}

export interface CompetitiveWriteSurface {
  surface: string;
  default: string;
  write_gate: string;
  audit_status: string;
}

export interface CompetitiveDecentralizationGate {
  license_required: boolean;
  license_server_required: boolean;
  activation_required: boolean;
  license_check_performed: boolean;
  mandatory_fee: boolean;
  fee_route: string;
  donation: CompetitiveDonationGate;
  offline_behavior: string;
  external_dependencies: CompetitiveExternalDependency[];
  source_basis: string[];
  repair_diagnostic: string;
  write_surfaces: CompetitiveWriteSurface[];
  home_miner_safe: boolean;
  home_miner_safe_status: string;
  docs_link: string;
  docs_link_status: string;
  recovery_link: string;
  recovery_link_status: string;
}

export interface CompetitiveFeatureDecentralization {
  license_required: boolean;
  mandatory_fee: boolean;
  fee_route: string;
  offline_behavior: string;
  source_basis: string[];
  repair_diagnostic: string;
  home_miner_safe_status: string;
}

export interface CompetitiveReadinessFeature {
  id: string;
  label: string;
  status: CompetitiveReadinessStatus;
  priority: string;
  competitor_reference: string;
  home_miner_value: string;
  current_behavior: string;
  risk: string;
  clean_room_path: string;
  acceptance_test: string;
  source_basis: string;
  telemetry_source: string;
  confidence: string;
  blockers: string[];
  docs_link: string;
  recovery_link: string;
  license_required: boolean;
  mandatory_fee: boolean;
  promotion_allowed: boolean;
  decentralization: CompetitiveFeatureDecentralization;
}

export interface CompetitiveReadinessResponse {
  schema: 'dcentos.competitive.readiness.v1' | string;
  status: CompetitiveReadinessStatus;
  read_only: true;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  content_collected: false;
  probe_performed: false;
  handlers_executed: false;
  telemetry_source: string;
  source: string;
  generated_at_s: number;
  fetched_at_ms: number;
  decentralization_gate: CompetitiveDecentralizationGate;
  feature_count: number;
  features: CompetitiveReadinessFeature[];
  promotion_allowed_only_when: string[];
  limitations: string[];
}

export interface SystemAsicResponse {
  asics: AsicInfo[];
}

// ─── Pools ──────────────────────────────────────────────────
export interface PoolInfo {
  id: number;
  url: string;
  worker: string;
  password?: string;
  status: string;
  priority: number;
  difficulty: number;
  accepted: number;
  rejected: number;
  last_share_s: number;
  /** Last measured submit->response RTT (ms) for this pool. `null` when the pool
   *  is not active (no measurement yet) — render an honest "—", never a fake 0. */
  latency_ms?: number | null;
  /** True only when the daemon has a REAL measured RTT for this pool. A 0 ms with
   *  `latency_measured === false` is a never-measured placeholder, not a fast pool. */
  latency_measured?: boolean;
  /** FWT-1 provenance: "honest_default" = fresh-boot/never-observed baseline. */
  latency_ms_source?: string;
  share_efficiency?: PoolState['share_efficiency'];
  stratum_active: boolean;
  protocol?: string;
  sv2_url?: string;
  encrypted?: boolean;
  donating?: boolean;
  telemetry_source?: string;
  health_limitations?: string[];
  no_notify_age_s?: number | null;
  failover_policy?: string;
  failover_active_pool_index?: number;
  failover_last_switch_reason?: string | null;
  failover_switch_count?: number;
  failover_stale_jobs_flushed_on_switch?: boolean;
  pending_submit_correlations_cleared?: number;
  shares_unresolved?: number | null;
  pending_submit_dropped?: number | null;
  auto_fallback_active?: boolean;
  auto_retry_sv2_after_s?: number | null;
  auto_fallback_reason?: string | null;
  hashrate_split_bps?: number | null;
  hashrate_split_pct?: number | null;
  hashrate_split_active?: boolean;
  hashrate_split_route?: string;
}

/**
 * W5.5: active donation route surfaced on /api/pools so the dashboard can
 * render which donation pool the donation slice is currently flowing
 * through (primary D-Central vs visible Braiins fallback).
 */
export interface PoolsDonationStatus {
  /** Whether the donation slice is currently active. Mirrors `pool.donating`. */
  active: boolean;
  /**
   * Human-readable route label.
   *  - `user_pool` — donation slice is NOT active (user pool is mining).
   *  - `donation_primary` — primary D-Central donation pool is in use.
   *  - `donation_fallback` — visible Braiins fallback worker is in use
   *    (typically `DungeonMaster`).
   */
  route: 'user_pool' | 'donation_primary' | 'donation_fallback';
  /** URL of the active donation pool, password-stripped. Empty when inactive. */
  active_url: string;
  /** Worker name authenticated with the active donation pool. Empty when inactive. */
  active_worker: string;
  /** 0 = primary D-Central donation, 1 = visible Braiins fallback worker. */
  pool_index: number;
}

export interface PoolsResponse {
  pools: PoolInfo[];
  failover?: PoolFailoverStatus;
  hashrate_split?: HashrateSplitState;
  /** W5.5: active donation route, see `PoolsDonationStatus`. */
  donation?: PoolsDonationStatus;
}

export interface PoolConfigEntry {
  url: string;
  worker: string;
  password: string;
  priority?: number;
  protocol?: string;
  sv2_url?: string;
  split_bps?: number;
}

export interface HashrateSplitRequest {
  enabled: boolean;
  secondary_pool_index?: number;
  secondary_pct?: number;
  cycle_duration_s?: number;
}

export type PoolConfigRequest = PoolConfigEntry | { pools: PoolConfigEntry[]; hashrate_split?: HashrateSplitRequest };

export interface PoolConfigResponse {
  status: string;
  message: string;
  pool: { url: string; worker: string; priority: number; protocol?: string; sv2_url?: string };
  pools?: Array<{ url: string; worker: string; priority: number; protocol?: string; sv2_url?: string }>;
}

export interface PoolTestResponse {
  status: string;
  reachable: boolean;
  message: string;
}

// ─── Stats ──────────────────────────────────────────────────
export interface StatsChain {
  id: number;
  chips: number;
  frequency_mhz: number;
  voltage_mv: number;
  voltage_v: number;
  temp_c: number;
  hashrate_ghs: number;
  hashrate_ths: number;
  errors: number;
  status: string;
  accepted: number;
  rejected: number;
  accepted_source?: string;
  rejected_source?: string;
  share_accounting?: {
    tracked: boolean;
    scope: string;
    source: string;
    reason?: string;
  };
  hw_errors: number;
}

// Power response now includes live data
export interface PowerStats {
  watts: number;
  wall_watts: number;
  efficiency_jth: number;
  btu_h: number;              // BTU/h = wall_watts x 3.412142 (canonical, TERM-3 §3.3)
  per_chain_w?: number[];
  source:
    | 'estimated'
    | 'live'
    | 'pmbus'
    | 'adc'
    | 'wall_calibrated_estimate'
    | 'calibrated_estimate'
    | 'live_power_watch'
    | 'static_model_fallback'
    | string;
  source_detail?:
    | 'pmbus_measured'
    | 'adc_measured'
    | 'wall_calibrated_estimate'
    | 'live_runtime_model'
    | 'static_power_fallback_from_miner_state'
    | string;
  live_power_available?: boolean;
  modeled?: boolean;
  note?: string;
  calibrated?: boolean;
  calibration_multiplier?: number | null;
  watt_cap?: RuntimeWattCapState;
  targeting?: PowerTargetingState;
  runtime_limits?: Array<{
    chain_id: number;
    effective_ceiling_mhz?: number | null;
    dominant_source?: string | null;
    active_sources: string[];
  }>;
}

export interface StatsProfitabilitySummary {
  daily_electricity_cost_usd: string;
  daily_electricity_cost_power_watts?: number;
  daily_electricity_cost_power_live_available?: boolean;
  daily_electricity_cost_power_modeled?: boolean;
  daily_electricity_cost_power_source_detail?: string;
  daily_electricity_cost_note?: string;
  electricity_rate_kwh: number;
  currency?: string;
  electricity_rate_calibrated?: boolean;
  heating_offset_fraction?: number;
  heat_reuse_credit_usd_per_day?: string;
  net_daily_cost_after_heat_credit_usd?: string;
  heat_reuse_note?: string;
  network_difficulty?: unknown;
}

export interface StatsResponse {
  hashrate_ghs: number;
  hashrate_ths: number;
  accepted?: number;
  rejected?: number;
  uptime_s: number;
  chains: StatsChain[];
  share_accounting?: {
    totals_tracked: boolean;
    totals_scope: string;
    totals_source: string;
    per_chain_tracked: boolean;
    per_chain_source: string;
    reason?: string;
  };
  fans: FanState;
  share_efficiency?: PoolState['share_efficiency'];
  power: PowerStats | {
    watts: number;
    efficiency_jth: number;
  };
  profitability_summary?: StatsProfitabilitySummary;
}

export interface RollingAverageBucket {
  window_s: number;
  sample_count: number;
  avg_hashrate_ths: number;
  avg_wall_watts: number;
  wall_power_sample_count: number;
  wall_power_measured_sample_count: number;
  wall_power_modeled_sample_count: number;
  wall_power_unavailable_sample_count: number;
  avg_max_chip_temp_c: number;
  avg_error_rate: number;
  avg_max_fan_pwm: number;
  accepted_shares: number;
  rejected_shares: number;
}

export interface RollingMetricsResponse {
  now_ms: number;
  total_samples: number;
  w5s: RollingAverageBucket;
  w1m: RollingAverageBucket;
  w5m: RollingAverageBucket;
}

// ─── Autotuner ────────────────────────────────────────────────
export interface SiliconGradeCounts {
  a: number;
  b: number;
  c: number;
  d: number;
}

export interface AutotunerPolicyStatus {
  requested_preset?: string | null;
  effective_preset?: string | null;
  requested_preset_supported?: boolean | null;
  requested_preset_display_name?: string | null;
  effective_preset_display_name?: string | null;
  requested_preset_reason?: string | null;
  degraded_from_requested?: boolean;
  capabilities?: {
    profile_key: string;
    family_key: string;
    voltage_control: string;
    quiet_home_presets: boolean;
    voltage_optimization_supported: boolean;
    dvfs_runtime_supported: boolean;
    mixed_family_ready: boolean;
    supported_preset_slugs: string[];
  } | null;
  active_objective?: string | null;
  active_limiting_factor?: string | null;
  safety_override?: string | null;
}

export interface AutotunerStatusResponse {
  enabled: boolean;
  live_runtime: boolean;
  stale: boolean;
  age_s: number;
  source: string;
  state: string;
  phase: string;
  percent_complete: number;
  completed_chips: number;
  active_chips: number;
  total_chips: number;
  active_chain_id?: number | null;
  active_chain_total_chips?: number | null;
  target_chains: number;
  tuned_chains: number;
  failed_chains: number;
  tuned_chain_ids: number[];
  failed_chain_ids: number[];
  estimated_remaining_s?: number | null;
  avg_frequency_mhz?: number | null;
  efficiency_jth?: number | null;
  silicon_grades?: SiliconGradeCounts | null;
  policy?: AutotunerPolicyStatus | null;
  dispatcher_limits?: Array<{
    chain_id: number;
    effective_ceiling_mhz?: number | null;
    dominant_source?: string | null;
    active_sources: string[];
  }>;
  last_update_s: number;
  message: string;
}

export interface AutotunerTelemetryChip {
  chip_index: number;
  nonces: number;
  errors: number;
  freq_mhz: number;
  decision?: string | null;
}

export interface AutotunerTelemetrySample {
  elapsed_s: number;
  chain_id: number;
  chips: AutotunerTelemetryChip[];
  board_temp_c?: number | null;
  tuner_state: string;
  difficulty: number;
}

export interface AutotunerTuningRun {
  started_at: number;
  duration_s: number;
  completed: boolean;
  samples: AutotunerTelemetrySample[];
}

export interface AutotunerTelemetryResponse {
  live_runtime: boolean;
  recording: boolean;
  runs: AutotunerTuningRun[];
  last_update_s: number;
  message: string;
  source?: string;
  stale?: boolean;
  age_s?: number;
  live_runtime_available?: boolean;
}

// ─── Silicon report (W9 / W15) — GET /api/autotuner/silicon-report ─────────
// Full silicon-quality analytics derived from the saved tuning profiles.
// PURE TELEMETRY — drives no hardware. When `characterized` is false the
// autotuner has not measured these chips yet, so grades are NOT fabricated
// and `quality_tier` is "Not Characterized" (the W15 honesty contract — the
// UI must surface this state, never present an un-measured chain as graded).
// Mirrors `dcentrald_autotuner::SiliconReport`.
export type ChipGradeLetter = 'A' | 'B' | 'C' | 'D';

export interface ChipRanking {
  chain_id: number;
  chip_index: number;
  max_stable_mhz: number;
  /** Stored frequency-bin grade (kept for back-compat). */
  grade: ChipGradeLetter | string;
  /** Effective grade — freq-bin grade refined by measured error-rate + nonce count. */
  effective_grade: ChipGradeLetter | string;
  error_rate: number;
  nonces_counted: number;
  characterized: boolean;
}

export interface ChainSiliconReport {
  chain_id: number;
  chip_count: number;
  quality_score: number;
  avg_max_stable_mhz: number;
  /** [A, B, C, D] effective-grade counts. */
  grade_distribution: [number, number, number, number];
}

export interface SiliconReportResponse {
  characterized: boolean;
  not_characterized_chips: number;
  quality_score: number;
  quality_tier: string;
  total_chips: number;
  grade_a_count: number;
  grade_b_count: number;
  grade_c_count: number;
  grade_d_count: number;
  grade_a_pct: number;
  grade_b_pct: number;
  grade_c_pct: number;
  grade_d_pct: number;
  avg_max_stable_mhz: number;
  best_chip_mhz: number;
  worst_chip_mhz: number;
  frequency_std_dev_mhz: number;
  chain_reports: ChainSiliconReport[];
  top_5_chips: ChipRanking[];
  bottom_5_chips: ChipRanking[];
}

// ─── Thermal supervisor snapshot (Wave-G) — GET /api/thermal/supervisor ────
// Read-only view of the live ThermalSupervisor. The chip-imbalance fields are
// DIAGNOSTIC ONLY (never drive a control decision); the `*_c` values are null
// until a board has read >= 2 valid per-chip sensors. Mirrors
// `dcentrald-thermal::supervisor::SupervisorSnapshot`.
export interface BoardStateSnapshot {
  chain_id: number;
  recovery_attempts: number;
  dropped_pcb_sensors: number;
  dropped_chip_sensors: number;
  /** Inter-chip temp spread (°C). null until >= 2 valid chip sensors read. */
  chip_imbalance_c: number | null;
  /** true if the last spread exceeded the diagnostic threshold. */
  chip_imbalance_flagged: boolean;
}

export interface ThermalSupervisorSnapshot {
  enabled: boolean;
  uptime_secs: number;
  secs_since_last_step: number;
  board_states: BoardStateSnapshot[];
  fan_max_pwm: number;
  chip_imbalance_threshold_c: number;
  worst_chip_imbalance_c: number | null;
  hydro_configured?: boolean;
}

// ─── Persistent audit log (W10) — GET /api/audit-log?offset&limit ──────────
// Paginated (newest-first), redacted view of the persistent /data/audit.log
// (survives reboots — the operator/fleet forensics surface). `event` is a
// serde-tagged union keyed by `event`; rendered generically. Secrets
// (passwords, worker names) are never present — the backend redacts before
// serializing. Mirrors `dcentrald_api_types::audit_log::{AuditRecord,AuditEvent}`.
export interface AuditRecord {
  timestamp_ms: number;
  schema_version: number;
  actor: string;
  /** Serde-tagged union: always carries an `event` discriminator + variant fields. */
  event: { event: string } & Record<string, unknown>;
}

export interface AuditLogResponse {
  schema: string;
  path: string;
  total: number;
  offset: number;
  limit: number;
  returned: number;
  redacted: boolean;
  events: AuditRecord[];
}

export interface AutotunerChipHealthStatus {
  chain_id: number;
  chip_index: number;
  health_score: number;
  trend: number;
  estimated_days_to_warning?: number | null;
  error_rate_pct: number;
  freq_mhz: number;
  backoff_count: number;
  hashrate_ratio: number;
  status: string;
}

export interface AutotunerChipHealthResponse {
  source: string;
  live_runtime: boolean;
  stale: boolean;
  age_s: number;
  last_update_s: number;
  message: string;
  total_chips: number;
  chips: AutotunerChipHealthStatus[];
}

// ─── GET /api/chips (RE-010 per-chip telemetry) ─────────────
// Mirrors the Rust DTOs in dcentrald-diagnostics/src/chip_health.rs.
// `/api/chips` returns the full `ChipHealthSnapshot` (chains[].chipmap.cells[]);
// it is the honest per-chip source that replaces ChipHeatMap's old sine-wave
// fabrication. The optional fields (expected_nonce_rate_hz / health_ts /
// die_temp_c) are RE-010 LOW-1/2/3 additive fields — omitted from the wire
// when None, so they are `T | null` here.

// ChipColor serializes as the capitalized Rust enum variant name.
export type ChipColor = 'Green' | 'Yellow' | 'Orange' | 'Red' | 'Gray';

// Health grade: a single-char string A/B/C/D/F (Rust `char`).
export type ChipGrade = 'A' | 'B' | 'C' | 'D' | 'F' | string;

export interface ChipMapCell {
  index: number;
  address: number;
  health_score: number;
  grade: ChipGrade;
  color: ChipColor;
  frequency_mhz: number;
  nonce_count: number;
  crc_errors: number;
  // RE-010 LOW-1/2/3 additive fields — absent from the wire when null.
  expected_nonce_rate_hz?: number | null;
  health_ts?: number | null;
  die_temp_c?: number | null;
}

export interface ChipMap {
  chain_id: number;
  chip_count: number;
  columns: number;
  rows: number;
  cells: ChipMapCell[];
}

export interface ChipHealthChainSnapshot {
  chain_id: number;
  source: string;
  chip_count: number;
  responding_chips: number;
  board_temp_c: number;
  board_hashrate_ghs: number;
  board_health_score: number;
  frequency_mhz: number;
  voltage_mv: number;
  errors: number;
  status: string;
  chipmap: ChipMap;
}

export interface ChipHealthSnapshotResponse {
  report_id: string;
  generated_at: string;
  report_type: string;
  source: string;
  total_boards: number;
  total_chips: number;
  warnings: string[];
  recommendations: string[];
  chains: ChipHealthChainSnapshot[];
}

// ─── History ────────────────────────────────────────────────
export interface AutotunerVisibilityProfileEntry {
  chain_id: number;
  file: string;
  present: boolean;
  read_ok: boolean;
  parse_ok: boolean;
  chip_count?: number | null;
  tuned_at?: string | null;
  avg_freq_mhz?: number | null;
  reason?: string | null;
}

export interface AutotunerVisibilityResponse {
  status: 'ok' | string;
  read_only: boolean;
  control_actions: false;
  hardware_writes: false;
  filesystem_mutation: false;
  generated_at_s: number;
  source: string;
  fetched_at_ms: number;
  runtime: {
    available: boolean;
    enabled: boolean;
    state: string;
    phase: string;
    source: string;
    stale: boolean;
    age_s: number;
    message: string;
    dispatcher_limits_visible: boolean;
    dispatcher_limit_count: number;
  };
  saved_profiles: {
    available: boolean;
    chains_with_profiles: number;
    expected_chains: number;
    entries: AutotunerVisibilityProfileEntry[];
    reason: string;
  };
  telemetry: {
    available: boolean;
    live_runtime: boolean;
    recording: boolean;
    run_count: number;
    last_update_s: number;
    csv_available: boolean;
    json_endpoint?: string;
    csv_endpoint?: string;
    latest_run?: {
      started_at_s: number;
      duration_s: number;
      completed: boolean;
      sample_count: number;
    } | null;
    reason: string;
  };
  rollback: {
    available: boolean;
    backup_profiles: AutotunerVisibilityProfileEntry[];
    backup_profile_count: number;
    config_visible: boolean;
    automatic_rollback_visible: boolean;
    reason: string;
  };
  simulation: {
    available: boolean;
    simulation_only: boolean;
    reason: string;
  };
  limitations: string[];
}

export interface HistoryResponse {
  history: HistoryPoint[];
  interval_s: number;
  count?: number;
  message?: string;
}

export interface HistoryPoint {
  timestamp: number;
  timestamp_s?: number;
  hashrate_ghs: number;
  temp_c: number;
  power_watts: number;
  power_source?: string;
  power_source_detail?: string;
  live_power_available?: boolean;
  power_modeled?: boolean;
  power_calibrated?: boolean;
  power_calibration_multiplier?: number | null;
  power_note?: string;
  fan_rpm: number;
}

// ─── Profiles ───────────────────────────────────────────────
export interface TuningProfile {
  name: string;
  frequency_mhz: number;
  voltage_mv: number;
  fan_mode: string;
}

export interface ProfilesResponse {
  profiles: TuningProfile[];
  active_profile: string | null;
}

// ─── Heater ─────────────────────────────────────────────────
export interface HeaterStatusResponse {
  power_watts: number;
  wall_watts?: number;
  btu_h: number;
  source?:
    | 'estimated'
    | 'live'
    | 'pmbus'
    | 'adc'
    | 'wall_calibrated_estimate'
    | 'calibrated_estimate'
    | 'live_power_watch'
    | 'static_model_fallback'
    | string;
  power_source_detail?:
    | 'pmbus_measured'
    | 'adc_measured'
    | 'wall_calibrated_estimate'
    | 'live_runtime_model'
    | 'static_power_fallback_from_miner_state'
    | string;
  live_power_available?: boolean;
  power_modeled?: boolean;
  power_note?: string;
  calibrated?: boolean;
  calibration_multiplier?: number | null;
  targeting?: PowerTargetingState;
  noise_db: number | null;
  noise_source?: 'tach_estimate' | 'unavailable_no_rpm_feedback' | string;
  noise_note?: string;
  airflow_cfm: number;
  preset: string;
  room_temp_c: number | null;
  cost_today_usd: number;
  daily_cost_usd?: number;
  daily_cost_power_watts?: number;
  daily_cost_power_live_available?: boolean;
  daily_cost_power_modeled?: boolean;
  daily_cost_power_source_detail?: string;
  daily_cost_note?: string;
  sats_today: number;
  // P0-4: honest calibration metadata for `sats_today`. When
  // `sats_today_calibrated` is false the backend could not read live network
  // difficulty, so `sats_today` is 0 (not fabricated) and the UI labels it an
  // uncalibrated estimate. `network_difficulty` is the difficulty the estimate
  // was anchored to (null when uncalibrated) so the client can run the same
  // canonical model via `estimateDailySats`.
  sats_today_calibrated?: boolean;
  sats_today_note?: string;
  network_difficulty?: number | null;
  // P2-4 (§4.E): daemon-persisted electricity economics — the SINGLE SOURCE OF
  // TRUTH for the rate/currency. The dashboard surfaces these instead of its own
  // localStorage guess. `electricity_rate_calibrated === false` ⇒ the rate is
  // the daemon default (not operator-confirmed), so cost/earnings must be
  // labelled an uncalibrated estimate until the operator confirms a rate.
  electricity_rate?: number;
  currency?: string;
  electricity_rate_calibrated?: boolean;
  night_mode_active: boolean;
  night_mode_starts_in_s: number | null;
  hashrate_ghs: number;
  circuit_usage_pct?: string;
  circuit_status?: 'ok' | 'warning' | 'danger' | 'unavailable' | string;
  circuit_power_watts?: number;
  circuit_power_live_available?: boolean;
  circuit_power_modeled?: boolean;
  circuit_power_source_detail?: string;
  circuit_note?: string;
  fans?: {
    pwm?: number;
    rpm?: number;
    max_rpm?: number;
    rpm_?: boolean;
  };
}

export interface HeaterTargetRequest {
  preset?: string;
  watts?: number;
}

export interface HeaterTargetResponse {
  status: string;
  message: string;
  preset?: string;
  watts?: number;
}

export interface HeaterPreset {
  name: string;
  display_name?: string;
  watts: number;
  wall_watts?: number;
  btu_h: number;
  noise_db: number | null;
  estimated_noise_db_s9?: number;
  noise_note?: string;
  hashrate_ths?: number;
  description: string;
}

export interface HeaterPresetScope {
  kind: string;
  family: string;
  chip_type: string;
  label: string;
  universal: boolean;
}

export interface HeaterPresetsResponse {
  presets: HeaterPreset[];
  scope?: HeaterPresetScope;
}

export interface RoomTempRequest {
  temp_c: number;
}

export interface NightModeResponse {
  enabled: boolean;
  start_hour: number;
  end_hour: number;
  max_fan_pwm: number;
  power_reduction_pct: number;
  active: boolean;
}

export interface NightModeRequest {
  enabled: boolean;
  start_hour?: number;
  end_hour?: number;
  max_fan_pwm?: number;
  power_reduction_pct?: number;
}

// P2-4 (§4.E): captured at first-boot setup. Persists the electricity rate +
// currency to the daemon `[home]` config (the single source of truth) and
// flips `electricity_rate_calibrated` true so cost/earnings stop reading as an
// uncalibrated estimate.
export interface SetupEconomicsRequest {
  electricity_rate: number;
  currency?: string;
}

// ─── Actions ────────────────────────────────────────────────
export interface ActionResponse {
  status: string;
  message: string;
}

export interface FirmwareUploadResponse extends ActionResponse {
  filename?: string;
  staged_path?: string;
  bytes_written?: number;
  validation_only?: boolean;
  update_started?: boolean;
  reused_staged_path?: boolean;
}

// ─── Debug (Hacker mode) ────────────────────────────────────
export interface RegisterReadResponse {
  chain: number;
  offset: string;
  count: number;
  values: number[];
  message?: string;
}

export interface RegisterWriteRequest {
  chain: number;
  offset: string;
  value: string;
  confirm?: boolean;
}

export interface I2cReadResponse {
  bus: number;
  addr: string;
  reg?: string;
  data: number[];
  message?: string;
}

export interface I2cWriteRequest {
  bus: number;
  addr: string;
  data: number[];
  confirm?: boolean;
}

export interface AsicCommandRequest {
  chain: number;
  command: string;
  chip?: number;
  register?: string;
  confirm?: boolean;
}

export interface AsicCommandResponse {
  status: string;
  chain: number;
  command: string;
  response: number[];
  message?: string;
}

export interface PidStateResponse {
  kp: number;
  ki: number;
  kd: number;
  setpoint: number;
  current_temp: number;
  output: number;
  integral: number;
  last_error: number;
  message?: string;
}

export interface PidParamsRequest {
  kp?: number;
  ki?: number;
  kd?: number;
  setpoint?: number;
  confirm?: boolean;
}

export interface ChipFrequencyRequest {
  chain: number;
  chip: number;
  freq_mhz: number;
  confirm?: boolean;
}

export interface ChipVoltageRequest {
  chain: number;
  pic_value: number;
  confirm?: boolean;
}

export interface ChipVoltageResponse {
  status: string;
  chain: number;
  pic_value: number;
  estimated_voltage_v: number;
  message?: string;
  warning?: string;
}

// ─── Diagnostics ────────────────────────────────────────────
export interface DiagnosticStartRequest {
  chain?: number;
  duration_minutes?: number;
}

export interface DiagnosticStartResponse {
  status: string;
  test_id: string;
  test_type: string;
  duration_minutes?: number;
  measurement_type?: string;
  message?: string;
  report_available?: boolean;
  report_url?: string;
}

export interface DiagnosticStatusResponse {
  test_id: string;
  status: string;
  progress_pct: number;
  phase: string;
  message: string;
  measurement_type?: string;
  generated_at?: string;
  report_available?: boolean;
  report_url?: string;
}

export interface DiagnosticResultResponse {
  test_id?: string;
  status?: string;
  message?: string;
  measurement_type?: string;
  report_available?: boolean;
  report_url?: string;
  [key: string]: unknown;
}

export interface DiagnosticReportMetadata {
  report_id: string;
  test_type: string;
  generated_at: string;
  firmware_version: string;
  html_size_bytes: number;
  json_size_bytes: number;
  grade?: string | null;
}

export interface RecentDiagnosticReportsResponse {
  status: string;
  reports: DiagnosticReportMetadata[];
}

export interface LogSourceManifestEntry {
  id: string;
  label: string;
  path: string;
  content_endpoint: string | null;
  content_access: string;
  metadata_status: string;
  exists: boolean;
  size_bytes: number | null;
  modified_ms: number | null;
  limitations: string[];
}

export interface LogManifestResponse {
  status: string;
  read_only: boolean;
  content_collected: boolean;
  sources: LogSourceManifestEntry[];
  limitations: string[];
}

// Read-only diagnostic and catalog endpoints mounted by dcentrald.
export interface DiagnosticsFailureMode {
  mode: string;
  severity: string;
  recovery: string;
}

export interface DiagnosticsFailureModesResponse {
  schema: string;
  count: number;
  modes: DiagnosticsFailureMode[];
}

export interface ChainDiagnosticsResponse {
  schema: string;
  id: number;
  observation: {
    chips_detected: number;
    chips_expected: number;
    nonces_returning: boolean;
  };
  verdict: string;
  repair_action: string;
  break_point_chip_idx?: number | null;
}

export interface LocalRejectDiagnostic {
  seq: number;
  timestamp_ms: number;
  chain_id: number;
  chip_index: number;
  nonce: number;
  work_id: number;
  midstate_idx: number;
  fpga_work_id_raw: number;
  generation_age: number;
  computed_hash_be_first8: number[];
  share_target_be_first8: number[];
  reason: string;
}

export interface LocalRejectsResponse {
  schema: string;
  ring_capacity: number;
  total_seen: number;
  returned: number;
  rejects: LocalRejectDiagnostic[];
}

export interface PicFirmwareVariant {
  fw_byte: string;
  fw_byte_decimal: number;
  architecture: string;
  wire_form: string;
  reset_safe: boolean;
  voltage_trusted: boolean;
  label: string;
}

export interface HardwarePicInfoResponse {
  schema: string;
  count: number;
  variants: PicFirmwareVariant[];
  live_per_slot: unknown | null;
  live_per_slot_note: string;
}

export interface RecoveryActionEntry {
  action: string;
  is_destructive: boolean;
}

export interface RecoveryCgiRoute {
  cgi: string;
  path: string;
}

export interface RecoveryActionsResponse {
  schema: string;
  actions: RecoveryActionEntry[];
  cgi_routes: RecoveryCgiRoute[];
  log_groups_whitelist: string[];
  uninstall_steps: string[];
  luxos_recovery_requires_auth: boolean;
  note: string;
}

export interface BootTimelinePhase {
  phase: string;
  at_seconds: number;
  description: string;
}

export interface ObservedBootPhase {
  phase: string;
  at_unix_ms: number;
}

export interface SystemBootTimelineResponse {
  schema: string;
  family: string;
  canonical: BootTimelinePhase[];
  observed: ObservedBootPhase[];
}

export interface AuditHistoryRecord {
  timestamp_ms: number;
  schema_version: number;
  actor: string;
  event: { event: string; [key: string]: unknown };
}

export interface HistoryAuditResponse {
  schema: string;
  ring_capacity: number;
  total_seen: number;
  returned: number;
  events: AuditHistoryRecord[];
}

export interface PsuCatalogModel {
  model: string;
  voltage_min_v: number;
  voltage_max_v: number;
  max_current_a: number | null;
  max_wattage_220v_w: number | null;
  max_wattage_110v_w: number | null;
  ac_input_min_v: number;
  ac_input_max_v: number;
  efficiency_pct: number;
  has_voltage_feedback: boolean;
  label: string;
  compatible_miners: string[];
}

export interface PsuCatalogResponse {
  schema: string;
  count: number;
  models: PsuCatalogModel[];
}

export interface CgminerCatalogCommand {
  name: string;
  kind: string;
  luxor_extension: boolean;
  destructive: boolean;
  doc: string;
}

export interface CgminerCatalogResponse {
  schema: string;
  count: number;
  total: number;
  set_count: number;
  get_count: number;
  luxor_extensions: number;
  destructive: number;
  commands: CgminerCatalogCommand[];
}

export interface ReadOnlyCatalogMeta {
  schema: string;
  read_only: boolean;
  hardware_reads: boolean;
  hardware_writes: boolean;
  config_writes: boolean;
  mining_control: boolean;
  source_crate: string;
}

export interface ReCatalogEndpoint {
  name: string;
  path: string;
  description: string;
}

export interface ReCatalogIndexResponse extends ReadOnlyCatalogMeta {
  base_path: string;
  catalogs: ReCatalogEndpoint[];
}

export interface NetworkTroubleshootResponse {
  ethernet: { mac: string; link_up: boolean };
  dns_ok?: boolean;
  gateway_reachable?: boolean;
  pool_reachable?: boolean;
  ntp_synced?: boolean;
  message: string;
}

export interface PsuTroubleshootResponse {
  detected: boolean;
  model?: string;
  fw_version?: string;
  transport?: string;
  control_mode?: string;
  output_enabled?: boolean;
  output_gate_enabled?: boolean;
  voltage_range?: string;
  voltage_in?: number;
  voltage_out?: number;
  current_a?: number;
  power_w?: number;
  temp_c?: number;
  supports_output_gate?: boolean;
  supports_voltage_set?: boolean;
  supports_watchdog?: boolean;
  message: string;
}

export interface PsuControlRequest {
  action: 'enable_watchdog' | 'disable_watchdog' | 'feed_watchdog' | 'set_voltage' | 'enable_output' | 'disable_output';
  voltage_v?: number;
  confirm?: boolean;
}

export interface PsuControlResponse {
  status: 'ok' | 'error' | 'not_implemented';
  action?: string;
  message: string;
  control_mode?: string;
  model?: string | null;
  fw_version?: string | null;
  output_enabled?: boolean | null;
  output_gate_enabled?: boolean | null;
  voltage_out?: number | null;
  target_voltage_v?: number | null;
  measured_voltage_v?: number | null;
}

export interface FpgaTroubleshootResponse {
  fpga_version?: string;
  build_id?: string;
  chains: unknown[];
  message: string;
}

// ─── WebSocket Messages ─────────────────────────────────────
export interface WsStatsMessage {
  type: 'stats';
  timestamp: number;
  hashrate_ghs: number;
  hashrate_5s_ghs: number;
  accepted: number;
  rejected: number;
  chains: ChainState[];
  fans: FanState;
  pool: PoolState;
  power_watts?: number;
  wall_watts?: number;
  efficiency_jth?: number;
  btu_h?: number;
  power_source?:
    | 'unavailable'
    | 'estimated'
    | 'live'
    | 'pmbus'
    | 'adc'
    | 'wall_calibrated_estimate'
    | 'calibrated_estimate'
    | 'live_power_watch'
    | string;
  power_source_detail?:
    | 'live_power_unavailable'
    | 'pmbus_measured'
    | 'adc_measured'
    | 'wall_calibrated_estimate'
    | 'live_runtime_model'
    | string;
  live_power_available?: boolean;
  power_modeled?: boolean;
  power_note?: string;
  power_calibrated?: boolean;
  power_calibration_multiplier?: number | null;
  watt_cap?: RuntimeWattCapState;
}

export interface WsHeaterMessage {
  type: 'heater_status';
  power_watts: number;
  wall_watts?: number;
  btu_h: number;
  power_source?:
    | 'unavailable'
    | 'estimated'
    | 'live'
    | 'pmbus'
    | 'adc'
    | 'wall_calibrated_estimate'
    | 'calibrated_estimate'
    | 'live_power_watch'
    | 'static_model_fallback'
    | string;
  power_source_detail?:
    | 'live_power_unavailable'
    | 'pmbus_measured'
    | 'adc_measured'
    | 'wall_calibrated_estimate'
    | 'live_runtime_model'
    | 'static_power_fallback_from_miner_state'
    | string;
  live_power_available?: boolean;
  power_modeled?: boolean;
  power_note?: string;
  power_calibrated?: boolean;
  power_calibration_multiplier?: number | null;
  noise_db: number | null;
  noise_source?: 'tach_estimate' | 'unavailable_no_rpm_feedback' | string;
  noise_note?: string;
  fans?: {
    pwm?: number;
    rpm?: number;
    max_rpm?: number;
    rpm_?: boolean;
  };
  airflow_cfm: number;
  preset: string;
  room_temp_c: number | null;
  cost_today_usd: number;
  sats_today: number;
  night_mode_active: boolean;
  night_mode_starts_in_s: number | null;
}

export interface WsDiagnosticMessage {
  type: 'diagnostic_progress';
  test_id: string;
  phase: string;
  progress_pct: number;
  elapsed_s: number;
  eta_s: number | null;
  detail: string;
}

export interface WsLogMessage {
  type: 'log';
  level: 'info' | 'warn' | 'error' | 'debug';
  source: 'mining' | 'system';
  timestamp: number;
  message: string;
}

export interface WsAutotunerStatusMessage {
  type: 'autotuner_status';
  payload: AutotunerStatusResponse;
}

export interface WsAutotunerEfficiencyMessage {
  type: 'autotuner_efficiency';
  payload: Record<string, unknown>;
}

export interface WsAutotunerChipHealthMessage {
  type: 'autotuner_chip_health';
  payload: AutotunerChipHealthResponse;
}

export interface WsMiningSyncMessage {
  type: 'mining_sync';
  timestamp_ms: number;
  event:
    | 'job_received'
    | 'clean_job'
    | 'dispatch_burst'
    | 'nonce_burst'
    | 'share_accepted'
    | 'share_rejected'
    | 'lucky_share';
  chain_id?: number | null;
  count?: number | null;
  job_id?: string | null;
  // Achieved difficulty only when locally proven.
  difficulty?: number | null;
  target_difficulty?: number | null;
  intensity?: number | null;
  error_code?: number | null;
  error_msg?: string | null;
}

export type WsMessage =
  | WsStatsMessage
  | WsHeaterMessage
  | WsDiagnosticMessage
  | WsLogMessage
  | WsAutotunerStatusMessage
  | WsAutotunerEfficiencyMessage
  | WsAutotunerChipHealthMessage
  | WsMiningSyncMessage;

// ─── LED Types ──────────────────────────────────────────────────────

export interface LedStatusResponse {
  enabled: boolean;
  current_pattern: string;
  locate_active: boolean;
  locate_remaining_s: number | null;
  night_mode_active: boolean;
}

export interface LedPatternInfo {
  id: string;
  name: string;
  description: string;
  duration_s: number;
}

export interface LedPatternsResponse {
  patterns?: LedPatternInfo[];
  locate_patterns?: LedPatternInfo[];
  background_patterns?: LedPatternInfo[];
  selected?: string;
  locate_count?: number;
}

export interface LocateRequest {
  pattern_id?: string;
}

export interface LedConfigResponse {
  enabled: boolean;
  heartbeat_on_ms: number;
  heartbeat_off_ms: number;
  locate_pattern: string;
  locate_duration_s: number;
  flash_on_accepted_share: boolean;
  flash_on_rejected_share: boolean;
  night_mode_disable: boolean;
  celebration_on_lucky_share: boolean;
  chain_status_blink_codes: boolean;
}

export interface LedConfigUpdateRequest {
  locate_pattern?: string;
  heartbeat_on_ms?: number;
  heartbeat_off_ms?: number;
  locate_duration_s?: number;
  flash_on_accepted_share?: boolean;
  flash_on_rejected_share?: boolean;
  night_mode_disable?: boolean;
  celebration_on_lucky_share?: boolean;
  chain_status_blink_codes?: boolean;
  enabled?: boolean;
}

// ─── SV2 Protocol Types ─────────────────────────────────────
export interface Sv2SessionInfo {
  cipher_suite: string;
  handshake_latency_ms: number;
  pool_pubkey_fingerprint: string;
  certificate_valid_from: number;
  certificate_not_after: number;
  channel_id?: number;
  noise_nonce_tx: number;
  noise_nonce_rx: number;
  bytes_encrypted: number;
  bytes_decrypted: number;
  messages_sent: number;
  messages_received: number;
}

export interface Sv2CustomJobInfo {
  status?: string;
  channel_id?: number | null;
  request_id?: number | null;
  template_id?: number | null;
  job_id?: number | null;
  last_error?: string | null;
  updated_at_s?: number;
}

export interface Sv2MessageRecord {
  direction: 'sent' | 'recv';
  msg_type: number;
  msg_name: string;
  timestamp_ms: number;
  payload_size: number;
}

export interface Sv2StatusResponse {
  session?: Sv2SessionInfo;
  connected: boolean;
  protocol_version?: string;
}

export interface Sv2HandshakeResponse {
  cipher_suite?: string;
  handshake_latency_ms?: number;
  pool_pubkey_fingerprint?: string;
  certificate_valid_from?: number;
  certificate_not_after?: number;
}

export interface Sv2MessagesResponse {
  messages?: Sv2MessageRecord[];
  total?: number;
}

// ─── Job Declaration Types ───────────────────────────────────
export interface JobDeclarationStatus {
  enabled?: boolean;
  configured?: boolean;
  connected?: boolean;
  template_provider_connected?: boolean;
  job_declarator_connected?: boolean;
  mining_job_token_available?: boolean;
  template_prev_hash_ready?: boolean;
  custom_job_candidate_ready?: boolean;
  custom_job_injection_ready?: boolean;
  custom_job_injection_active?: boolean;
  custom_job_bridge?: Sv2CustomJobInfo | null;
  protocol_ready?: boolean;
  live_jdc_runtime?: boolean;
  restart_required?: boolean;
  mode?: string;
  bitcoind_url?: string;
  template_provider_url?: string;
  job_declarator_url?: string;
  templates_constructed?: number;
  last_template_age_s?: number;
  current_template_id?: number;
  last_declared_job_id?: number;
  custom_job_last_request_id?: number;
  custom_job_last_template_id?: number;
  coinbase_value_remaining_sats?: number;
  coinbase_output_count?: number;
  last_connection_attempt_s?: number;
  last_update_s?: number;
  last_error?: string;
  current_tx_count?: number;
  current_fees_btc?: number;
  runtime_state?: string;
  reason?: string;
  config?: JobDeclarationConfig & {
    configured?: boolean;
    bitcoind_rpc_password_set?: boolean;
  };
}

export interface JobDeclarationConfig {
  enabled?: boolean;
  mode?: string;
  bitcoind_rpc_url?: string;
  bitcoind_rpc_user?: string;
  bitcoind_rpc_password?: string;
  bitcoind_rpc_cookie?: string;
  template_provider_url?: string;
  job_declarator_url?: string;
  coinbase_output_address?: string;
  template_refresh_interval_s?: number;
  fallback_to_pool_templates?: boolean;
  declare_tx_data?: boolean;
  coinbase_output_max_additional_size?: number;
  coinbase_output_max_additional_sigops?: number;
}

// ─── W11.12 stock-CGI parity ────────────────────────────────
// Read-only endpoints aligned with RE2 §15.2 + competing-firmware
// dashboards (BraiinsOS+ web, VNish dashd). See
// dcentrald-api/src/routes/stock_parity.rs for the full contract.
export interface NetworkInfoResponse {
  hostname: string;
  mac: string;
  primary_interface: string;
  ipv4_cidr: string;
  ipv4: string;
  ipv6: string;
  gateway: string;
  dns: string;
  link_state: string;
  dhcp: boolean;
  warnings: string[];
}

export interface NetworkHostnameRequest {
  hostname: string;
}

export interface NetworkHostnameResponse {
  status: string;
  persisted: boolean;
  hostname: string;
  note?: string;
}

export interface MinerTypeResponse {
  model: string;
  asic: string;
  chip_count: number;
  chain_count: number;
  control_board: string;
  soc: string;
  hashboard: string;
  mac: string;
  hostname: string;
  firmware: string;
  firmware_version: string;
  // W13.D1 — PVT envelope fields. Default to "standard" / 0 / false on
  // older daemons or non-BM1362 SKUs, so the dashboard degrades gracefully.
  pvt_grade?: string;
  pvt_voltage_min_mv?: number;
  pvt_voltage_max_mv?: number;
  pvt_freq_min_mhz?: number;
  pvt_freq_max_mhz?: number;
  voltage_fixed?: boolean;
  mix_levels_supported?: boolean;
  requires_apw12_plus?: boolean;
  inverted_curve?: boolean;
  sku_chain_count?: number;
  sku_asics_per_chain?: number;
}

// ─── W13.D1: PVT table ──────────────────────────────────────────────
export interface PvtLevelEntry {
  freq_mhz: number;
  voltages_mv: number[];
}

export interface PvtTableResponse {
  sku: string;
  grade: string;
  voltage_fixed: boolean;
  mix_levels: boolean;
  requires_apw12_plus: boolean;
  inverted_curve: boolean;
  chain_count: number;
  asics_per_chain: number;
  levels: PvtLevelEntry[];
}

// ─── W13.D1: Boot phase taxonomy ────────────────────────────────────
//
// CV1835 cold-boot 6-substate (per R4 `bmminer_init_trace_cv1835.md`).
export type Cv1835BootSubstate =
  | 'boot_psu_init'
  | 'boot_pic_dc_dc_enable'
  | 'boot_asic_enum'
  | 'boot_misc_ctrl_triple_write'
  | 'boot_first_work_tx'
  | 'boot_awaiting_first_nonce';

// Generic 3-substate fallback (non-CV1835 platforms).
export type GenericBootSubstate = 'booting' | 'starting' | 'mining';

// `serde(tag="kind", content="phase")` shape.
export type BootPhase =
  | { kind: 'cv1835'; phase: Cv1835BootSubstate }
  | { kind: 'generic'; phase: GenericBootSubstate }
  | { kind: 'hybrid_mode_no_api' };

export interface BootPhaseResponse {
  phase: BootPhase;
  started_at_unix_ms: number | null;
  is_live: boolean;
}

export interface BootTimelineEntry {
  phase: BootPhase;
  started_at_unix_ms: number;
  ended_at_unix_ms: number | null;
}

export interface BootTimelineResponse {
  entries: BootTimelineEntry[];
}
