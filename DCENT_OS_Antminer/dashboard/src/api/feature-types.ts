// DCENTos Feature Types — Sprint 5 Differentiation Features
// Separate file to avoid conflicts with types.ts owned by other sprint agent

// ─── TOU Rate Scheduling ────────────────────────────────────
export type RateTier = 'off-peak' | 'mid-peak' | 'on-peak';

export type MiningBehavior = 'full' | 'reduced' | 'sleep';

export interface TouScheduleBlock {
  day: number;        // 0=Sunday, 6=Saturday
  hour: number;       // 0-23
  tier: RateTier;
}

export interface TouTierConfig {
  tier: RateTier;
  rate: number;       // $/kWh
  behavior: MiningBehavior;
}

export interface TouScheduleConfig {
  enabled: boolean;
  schedule: TouScheduleBlock[];
  tiers: TouTierConfig[];
}

// ─── Green Mining / Solar ───────────────────────────────────
export type InverterBrand = 'bridge' | 'ecoflow' | 'enphase' | 'solaredge' | 'victron' | 'tesla' | 'manual';
export type SolarProviderStage = 'live' | 'limited' | 'staged' | 'unsupported';

export interface SolarProviderMetadata {
  providerLiveBackend?: boolean;
  providerTelemetryBacked?: boolean;
  providerStage?: SolarProviderStage;
  providerStageReason?: string | null;
  recommendedProvider?: string | null;
  providerBackendScope?: string | null;
  acceptedPayloadShapes?: string[];
}

export interface SolarConfig extends SolarProviderMetadata {
  enabled: boolean;
  inverterBrand: InverterBrand;
  apiEndpoint: string;
  apiKey: string;
  bridgeBaseUrl?: string;
  bridgeApiKey?: string;
  teslaGatewayHost?: string;
  teslaPassword?: string;
  solarOnlyMode: boolean;       // Mine only on solar surplus
  baseLoadWatts: number;        // House base load to subtract
  batteryThresholdPct: number;  // Min battery SoC to allow mining
  batteryWakeHysteresisPct: number;
  providerMaxSampleAgeMs: number;
  providerFailureHysteresisSamples: number;
  hybridImportDeadbandWatts: number;
  manualProductionWatts: number;
  manualSiteLoadWatts: number;
  manualBatterySocPct: number | null;
}

export interface SolarStatus extends SolarProviderMetadata {
  enabled?: boolean;
  provider?: string;
  providerConfigured?: boolean;
  runtimeAdopted?: boolean;
  commissioningState?: 'disabled' | 'pending_restart' | 'manual_runtime' | 'telemetry_live' | 'telemetry_degraded' | string;
  sourceProfile?: string;
  productionWatts: number;
  consumptionWatts: number;
  miningWatts: number;
  miningWattsSource?: string;
  miningWattsLive?: boolean;
  miningWattsModeled?: boolean;
  miningWattsNote?: string;
  netGridWatts: number;         // Positive = importing, negative = exporting
  solarSurplusWatts: number;
  batterySocPct: number | null;
  connected: boolean;
  transport?: string;
  matchedFields?: string[];
  matched_fields?: string[];
  solarOnlyMode?: boolean;
  controlActive?: boolean;
  sleeping?: boolean;
  batteryFloorActive?: boolean;
  targetFreqMhz?: number | null;
  action?: string;
  sampleAgeMs?: number | null;
  stale?: boolean;
  consecutiveFailures?: number;
  lastSuccessMs?: number | null;
  lastUpdateMs?: number;
  message?: string;
}

export interface SolarTestResponse extends SolarProviderMetadata {
  ok: boolean;
  provider: string;
  connected: boolean;
  transport?: string;
  matchedFields?: string[];
  matched_fields?: string[];
  productionWatts?: number;
  consumptionWatts?: number;
  netGridWatts?: number;
  batterySocPct?: number | null;
  message: string;
}

export interface SolarVerificationSample {
  timestampMs: number;
  provider: string;
  transport: string;
  connected: boolean;
  sampleAgeMs?: number | null;
  stale: boolean;
  consecutiveFailures: number;
  lastSuccessMs?: number | null;
  matchedFields?: string[];
  matched_fields?: string[];
  productionWatts: number;
  consumptionWatts: number;
  netGridWatts: number;
  batterySocPct?: number | null;
  message: string;
}

export interface SolarVerificationHistoryResponse {
  generatedAtMs: number;
  entries: SolarVerificationSample[];
}

export interface GreenMiningMetrics {
  energyStoredTodayKwh: number;
  btcValueStoredUsd: number;
  conversionEfficiencyPct: number;
  greenHoursToday: number;
  totalHoursToday: number;
  greenScorePct: number;
  gridCarbonIntensity: number;  // gCO2/kWh
}

// ─── Methane Mitigation ─────────────────────────────────────
export type GasType = 'flared' | 'vented' | 'landfill' | 'biogas';

export interface MethaneInputs {
  gasFlowRateMcfh: number;      // thousand cubic feet per hour
  gasType: GasType;
  generatorEfficiencyPct: number;
}

export interface MethaneResults {
  powerAvailableKw: number;
  co2OffsetTonsYr: number;
  carbonCreditEstimateUsd: number;
  treesEquivalent: number;
  methaneDestroyedTonsYr: number;
}

// ─── MQTT / Home Assistant ──────────────────────────────────
export interface MqttConfig {
  enabled: boolean;
  broker: string;
  topicPrefix: string;
  discovery: boolean;
  username: string;
  password: string;
  publishIntervalS: number;
  restartRequired: boolean;
  runtimeMessage: string;
}

export interface MqttTestResponse {
  ok: boolean;
  connected: boolean;
  message: string;
  broker?: string;
  client_id?: string;
  state_topic?: string;
  availability_topic?: string;
  restart_required?: boolean;
}

// Live publisher health from GET /api/mqtt/status. Read-only observability of
// the running [mqtt] publisher task — NEVER a control surface. Every field is
// optional so the dashboard renders an honest "unavailable"/"—" state when the
// daemon doesn't report a value (or the route 404s on an older build).
export interface MqttStatusResponse {
  // Whether the [mqtt] publisher is enabled in config.
  enabled: boolean;
  // Whether the publisher currently holds a live broker connection.
  connected: boolean;
  // Host-only / redacted broker authority the publisher targets.
  broker?: string;
  // Whether HA discovery is enabled (the discovery config retain-publish path).
  discovery?: boolean;
  // Whether the optional writable command/subscribe path is active. When false,
  // the 3 writable command entities (Fan PWM / Target Power / Space Heater) are
  // NOT advertised to Home Assistant.
  commands_enabled?: boolean;
  // Count of HA discovery entities the publisher has advertised this session.
  entity_count?: number | null;
  // Epoch-ms timestamp of the last successful state publish (null = never yet).
  last_publish_ms?: number | null;
  // Optional convenience age, if the daemon computes it server-side.
  last_publish_age_s?: number | null;
  // Cumulative successful state publishes this session.
  publish_count?: number | null;
  // Last publisher error string (host/credential-redacted), if any.
  error?: string | null;
  // Optional human-readable status note.
  message?: string;
}

export interface HaEntity {
  entityId: string;
  name: string;
  type: 'sensor' | 'binary_sensor';
  unit: string;
  icon: string;
}

// ─── Fleet Discovery ────────────────────────────────────────
export interface DiscoveredMiner {
  ip: string;
  hostname: string;
  model: string;
  firmware: string;
  hashrateThs: number;
  powerWatts?: number | null;
  status: 'online' | 'sleeping' | 'error';
  uptimeS: number;
  mac: string;
}

export interface FleetStats {
  totalMiners: number;
  totalHashrateThs: number;
  totalPowerWatts: number | null;
  onlineCount: number;
  sleepingCount: number;
  errorCount: number;
}

export interface FleetDiscoverRequest {
  includeConfigured: boolean;
  manualIps: string[];
  hintIps: string[];
}

export interface FleetDiscoverResponse {
  status: string;
  source?: string;
  miners: DiscoveredMiner[];
  limitations?: string[];
  request?: FleetDiscoverRequest;
}

// ─── Data Export ─────────────────────────────────────────────
export type ExportFormat = 'csv' | 'json';
export type ExportDataType = 'hashrate' | 'temperature' | 'power' | 'earnings' | 'all';

export interface ExportRequest {
  format: ExportFormat;
  dataType: ExportDataType;
  startDate: string;            // ISO date
  endDate: string;              // ISO date
}

export interface TaxReportEntry {
  date: string;
  btcMined: number;
  btcPriceUsd: number;
  valueUsd: number;
  powerCostUsd: number;
  netUsd: number;
}

// ─── Circuit Capacity ───────────────────────────────────────
export type CircuitVoltage = 120 | 240;
export type BreakerAmps = 15 | 20 | 30 | 40 | 50;

export interface CircuitConfig {
  voltage: CircuitVoltage;
  breakerAmps: BreakerAmps;
  safetyFactorPct: number;      // NEC 80% = 80
}

export interface CircuitResult {
  maxContinuousWatts: number;
  maxContinuousAmps: number;
  currentUsageWatts: number;
  usagePct: number;
  safe: boolean;
}

// ─── Demand Response ────────────────────────────────────────
export type GridOperator = 'ercot' | 'caiso' | 'pjm' | 'nyiso' | 'miso' | 'hydro-quebec' | 'manual';

export interface DemandResponseConfig {
  enabled: boolean;
  gridOperator: GridOperator;
  curtailmentThresholdCentsKwh: number;
  negativePriceMining: boolean;
  apiEndpoint: string;
}

// ─── Immersion Cooling ──────────────────────────────────────
export type CoolantType = 'mineral-oil' | 'dielectric-fluid' | 'engineered-fluid' | 'custom';

export interface ImmersionConfig {
  enabled: boolean;
  coolantType: CoolantType;
  maxChipTempC: number;
  inletTempC: number | null;
  outletTempC: number | null;
  flowRateLpm: number | null;
}

// ─── Community Tuning Profiles ──────────────────────────────
export interface CommunityProfile {
  name: string;
  author: string;
  model: string;
  target: 'efficiency' | 'performance' | 'quiet' | 'balanced';
  frequencyMhz: number;
  voltageMv: number;
  fanMode: string;
  description: string;
  version: string;
  createdAt: string;
  hashrateThs: number | null;
  efficiencyJth: number | null;
}
